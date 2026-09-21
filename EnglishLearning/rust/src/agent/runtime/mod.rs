//! **HTTP surface** — the Rust replacement for the Bun `agent-runtime`
//! (`CopilotRuntime` + `CopilotRuntimeHandler` + `HttpAgent`) *and* the Python
//! FastAPI app, in one process.
//!
//! ```text
//! React (CopilotKit) ──HTTP/SSE──▶ this module ──▶ engine::run_turn ──▶ DeepSeek
//! ```
//!
//! Because the engine already speaks AG-UI, the runtime has no proxying to do:
//! it validates the request, spawns the turn, and pipes the engine's event
//! channel out as Server-Sent Events. The `HttpAgent` hop and the second HTTP
//! server are gone.
//!
//! # Endpoints
//!
//! | method | path | purpose |
//! | --- | --- | --- |
//! | `GET` | `/health` | liveness |
//! | `GET` | `/copilotkit/info` | agent + catalog information |
//! | `POST` | `/copilotkit/agent/{agent_id}/run` | run a turn (SSE) |
//! | `GET` | `/copilotkit/agent/{agent_id}/connect` | persistent SSE channel |
//! | `POST` | `/copilotkit/agent/{agent_id}/stop/{thread_id}` | stop a running turn |
//! | `GET`/`POST` | `/copilotkit/threads` | list / create threads |
//! | `DELETE` | `/copilotkit/threads/{thread_id}` | forget a thread |
//! | `POST` | `/copilotkit` | single-endpoint alias for `/run` |

pub mod middleware;

use crate::agent::config::AgentConfig;
use crate::agent::engine::{self, ThreadStore};
use crate::agent::llm::{ChatMessage, OpenAiClient};
use crate::agent::new_id;
use crate::agent::protocol::AgUiEvent;
use crate::agent::tools::ToolRegistry;
use middleware::A2uiMiddleware;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

/// Channel depth between the engine and the SSE writer.
///
/// Small enough to bound memory, large enough that a burst of tool events never
/// blocks the engine.
const EVENT_BUFFER: usize = 64;

// ─────────────────────────────────────────────────────────────────────────
// Service state
// ─────────────────────────────────────────────────────────────────────────

/// Everything the handlers share.
pub struct AgentService {
    cfg: AgentConfig,
    registry: ToolRegistry,
    llm: OpenAiClient,
    threads: ThreadStore,
    middleware: A2uiMiddleware,
    /// Threads asked to stop. Checked by the SSE writer, which then drops the
    /// event channel — the engine notices on its next emit and unwinds.
    stopped: Mutex<HashSet<String>>,
    started: Instant,
}

impl AgentService {
    /// Build the service (opens the model client and registers the tools).
    pub fn new(cfg: AgentConfig) -> Result<Self, String> {
        let llm = OpenAiClient::new(&cfg).map_err(|e| e.to_string())?;
        let registry = ToolRegistry::with_builtins();
        let middleware = A2uiMiddleware::new(cfg.catalog_id.clone());
        Ok(AgentService {
            cfg,
            registry,
            llm,
            threads: ThreadStore::new(),
            middleware,
            stopped: Mutex::new(HashSet::new()),
            started: Instant::now(),
        })
    }

    fn mark_stopped(&self, thread_id: &str) {
        self.stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(thread_id.to_string());
    }

    fn clear_stopped(&self, thread_id: &str) {
        self.stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(thread_id);
    }

    fn is_stopped(&self, thread_id: &str) -> bool {
        self.stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(thread_id)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Request payload (AG-UI `RunAgentInput`)
// ─────────────────────────────────────────────────────────────────────────

/// The body CopilotKit posts to `/run`.
///
/// Only the fields this service uses are modelled; unknown fields are ignored so
/// a newer frontend cannot break the agent.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAgentInput {
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<ClientMessage>,
    #[serde(default)]
    pub state: Option<Value>,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default)]
    pub forwarded_props: Option<Value>,
}

/// One message from the browser.
#[derive(Debug, Deserialize)]
pub struct ClientMessage {
    pub role: String,
    /// Either a string or an array of content parts (`[{"type":"text","text":…}]`).
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub id: Option<String>,
}

impl RunAgentInput {
    /// Convert the client messages into model messages, dropping anything empty
    /// or of an unknown role (the frontend echoes tool traffic back, which the
    /// model must not see twice).
    pub fn into_chat_messages(self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter_map(ClientMessage::to_chat_message)
            .collect()
    }
}

impl ClientMessage {
    fn to_chat_message(&self) -> Option<ChatMessage> {
        let text = content_to_text(self.content.as_ref()?);
        if text.trim().is_empty() {
            return None;
        }
        match self.role.to_ascii_lowercase().as_str() {
            "system" => Some(ChatMessage::system(text)),
            "user" => Some(ChatMessage::user(text)),
            "assistant" => Some(ChatMessage::assistant(text)),
            _ => None,
        }
    }
}

/// Flatten an AG-UI content value into plain text.
fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────

async fn health(State(service): State<Arc<AgentService>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "sentence-video agent",
        "uptimeSeconds": service.started.elapsed().as_secs(),
        "threads": service.threads.len(),
        "credentials": service.cfg.has_credentials(),
    }))
}

async fn info(State(service): State<Arc<AgentService>>) -> impl IntoResponse {
    let cfg = &service.cfg;
    Json(json!({
        "agents": {
            cfg.agent_id.clone(): {
                "name": cfg.agent_name,
                "description": cfg.agent_description,
                "model": service.llm.model(),
            }
        },
        "a2ui": {
            "defaultCatalogId": service.middleware.catalog_id(),
            "injectA2UITool": true,
        },
        "tools": service.registry.names(),
        "endpoint": service.llm.endpoint(),
    }))
}

async fn list_threads(State(service): State<Arc<AgentService>>) -> impl IntoResponse {
    Json(json!({ "threads": service.threads.summaries() }))
}

async fn create_thread() -> impl IntoResponse {
    let thread_id = new_id("thread");
    (StatusCode::CREATED, Json(json!({ "id": thread_id })))
}

async fn delete_thread(
    State(service): State<Arc<AgentService>>,
    Path(thread_id): Path<String>,
) -> impl IntoResponse {
    let existed = service.threads.clear(&thread_id);
    Json(json!({ "deleted": existed, "id": thread_id }))
}

/// `POST /copilotkit/agent/{agent_id}/run`
async fn run_agent(
    State(service): State<Arc<AgentService>>,
    Path(agent_id): Path<String>,
    Json(input): Json<RunAgentInput>,
) -> Response {
    if agent_id != service.cfg.agent_id {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": format!("unknown agent '{agent_id}'"),
                "agents": [service.cfg.agent_id.clone()],
            })),
        )
            .into_response();
    }
    stream_run(service, input)
}

/// `POST /copilotkit` — the single-endpoint mode.
async fn run_default_agent(
    State(service): State<Arc<AgentService>>,
    Json(input): Json<RunAgentInput>,
) -> Response {
    stream_run(service, input)
}

/// `GET /copilotkit/agent/{agent_id}/connect`
///
/// A persistent SSE channel. This service is stateless per run, so the channel
/// carries no traffic of its own — it exists so the client's connection
/// handshake succeeds, and it is kept alive until the client goes away.
async fn connect(State(service): State<Arc<AgentService>>, Path(agent_id): Path<String>) -> Response {
    if agent_id != service.cfg.agent_id {
        return StatusCode::NOT_FOUND.into_response();
    }
    let idle = futures_util::stream::pending::<Result<Event, Infallible>>();
    Sse::new(idle).keep_alive(KeepAlive::default()).into_response()
}

/// `POST /copilotkit/agent/{agent_id}/stop/{thread_id}`
async fn stop_run(
    State(service): State<Arc<AgentService>>,
    Path((agent_id, thread_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if agent_id != service.cfg.agent_id {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "unknown agent" })));
    }
    service.mark_stopped(&thread_id);
    (StatusCode::OK, Json(json!({ "stopped": true, "threadId": thread_id })))
}

// ─────────────────────────────────────────────────────────────────────────
// SSE plumbing
// ─────────────────────────────────────────────────────────────────────────

/// Spawn the turn and stream its events back as SSE.
fn stream_run(service: Arc<AgentService>, input: RunAgentInput) -> Response {
    let thread_id = input
        .thread_id
        .clone()
        .unwrap_or_else(|| new_id("thread"));

    // A thread that was stopped earlier must be runnable again.
    service.clear_stopped(&thread_id);

    let messages = input.into_chat_messages();
    let (tx, rx) = mpsc::channel::<AgUiEvent>(EVENT_BUFFER);

    {
        let service = Arc::clone(&service);
        let thread_id = thread_id.clone();
        tokio::spawn(async move {
            if messages.is_empty() {
                // Nothing to answer — report it as a normal run error so the UI
                // shows a message instead of an empty response.
                let _ = tx
                    .send(AgUiEvent::error("the request contained no user message"))
                    .await;
                let _ = tx
                    .send(AgUiEvent::RunFinished {
                        thread_id: thread_id.clone(),
                        run_id: new_id("run"),
                    })
                    .await;
                return;
            }

            if let Err(e) = engine::run_turn(
                &service.cfg,
                &service.llm,
                &service.registry,
                &service.threads,
                &service.middleware,
                &thread_id,
                messages,
                tx.clone(),
            )
            .await
            {
                // The engine already emitted a RUN_ERROR for the model path;
                // this covers anything that escaped it.
                let _ = tx.send(AgUiEvent::error(e.to_string())).await;
            }
        });
    }

    // Bridge the event channel to SSE. Ending this stream drops `rx`, which makes
    // the engine's next emit fail and unwinds the turn — that is how /stop and a
    // closed tab both cancel a run.
    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(event) = rx.recv().await {
            if service.is_stopped(&thread_id) {
                break;
            }
            let terminal =
                event.name() == crate::agent::protocol::ag_ui::kind::RUN_FINISHED;
            yield Ok::<Event, Infallible>(
                Event::default().event(event.name()).data(event.to_sse_data()),
            );
            if terminal {
                break;
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────
// Server
// ─────────────────────────────────────────────────────────────────────────

/// Build the router. Exposed for tests and for embedding in another server.
pub fn router(service: Arc<AgentService>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/copilotkit", post(run_default_agent))
        .route("/copilotkit/info", get(info))
        .route("/copilotkit/threads", get(list_threads).post(create_thread))
        .route("/copilotkit/threads/{thread_id}", delete(delete_thread))
        .route("/copilotkit/agent/{agent_id}/run", post(run_agent))
        .route("/copilotkit/agent/{agent_id}/connect", get(connect))
        .route(
            "/copilotkit/agent/{agent_id}/stop/{thread_id}",
            post(stop_run),
        )
        // Permissive CORS mirrors the old Bun runtime and the FastAPI app; the
        // service binds to loopback by default, so this is a local-only surface.
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(service)
}

/// Start the service and block until the process ends.
pub fn serve(cfg: AgentConfig) -> Result<(), String> {
    let bind = cfg.bind_addr();
    let service = Arc::new(AgentService::new(cfg)?);
    let app = router(Arc::clone(&service));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;

    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(|e| format!("cannot bind {bind}: {e}"))?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;

        println!("sentence-video agent: listening on http://{local}/copilotkit");
        println!(
            "sentence-video agent: agent '{}', model '{}', catalog '{}'",
            service.cfg.agent_id,
            service.llm.model(),
            service.middleware.catalog_id()
        );
        println!(
            "sentence-video agent: tools: {}",
            service.registry.names().join(", ")
        );
        if !service.cfg.has_credentials() {
            eprintln!(
                "sentence-video agent: DEEPSEEK_API_KEY is not set — /run will answer with RUN_ERROR."
            );
        }

        axum::serve(listener, app)
            .await
            .map_err(|e| format!("agent server stopped: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_string_and_part_array_content() {
        assert_eq!(content_to_text(&json!("hello")), "hello");
        assert_eq!(
            content_to_text(&json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }])),
            "ab"
        );
        assert_eq!(content_to_text(&Value::Null), "");
    }

    #[test]
    fn keeps_only_speakable_roles_with_text() {
        let input = RunAgentInput {
            thread_id: Some("t1".into()),
            messages: vec![
                ClientMessage { role: "system".into(), content: Some(json!("rules")), id: None },
                ClientMessage { role: "user".into(), content: Some(json!("hi")), id: None },
                ClientMessage { role: "tool".into(), content: Some(json!("ignored")), id: None },
                ClientMessage { role: "assistant".into(), content: Some(json!("")), id: None },
                ClientMessage {
                    role: "user".into(),
                    content: Some(json!([{ "type": "text", "text": "second" }])),
                    id: None,
                },
            ],
            ..Default::default()
        };

        let messages = input.into_chat_messages();
        let roles: Vec<_> = messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                crate::agent::llm::Role::System,
                crate::agent::llm::Role::User,
                crate::agent::llm::Role::User
            ]
        );
        assert_eq!(messages[2].content.as_deref(), Some("second"));
    }

    #[test]
    fn service_reports_the_configured_agent_and_tools() {
        let service = AgentService::new(AgentConfig::default()).unwrap();
        assert_eq!(service.cfg.agent_id, "sample_agent");
        assert_eq!(service.middleware.catalog_id(), "generative-agent-catalog");
        assert!(service.registry.names().contains(&"display_register_form"));
        assert!(!service.cfg.has_credentials());
    }

    #[test]
    fn stop_flags_are_per_thread_and_clearable() {
        let service = AgentService::new(AgentConfig::default()).unwrap();
        assert!(!service.is_stopped("t1"));

        service.mark_stopped("t1");
        assert!(service.is_stopped("t1"));
        assert!(!service.is_stopped("t2"));

        service.clear_stopped("t1");
        assert!(!service.is_stopped("t1"));
    }
}
