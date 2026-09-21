//! **HTTP surface** — the Rust replacement for the Bun `agent-runtime`
//! (`CopilotRuntime` + `CopilotRuntimeHandler` + `HttpAgent`) *and* the Python
//! FastAPI app, in one process.
//!
//! ```text
//! React (CopilotKit) ──HTTP/SSE──▶ ag_ui::axum ──▶ CopilotAgent::run ──▶ engine ──▶ DeepSeek
//! ```
//!
//! The protocol half is **not** hand-written any more: [`ag_ui`] owns the event
//! vocabulary, SSE framing, request parsing, event-ordering verification and
//! cancellation. This module contributes only the application half — the
//! [`CopilotAgent`] implementation and a few side endpoints.
//!
//! | concern | owner |
//! | --- | --- |
//! | `POST` body → `RunAgentInput` | `ag_ui::axum` |
//! | `RUN_STARTED` / `RUN_FINISHED` / `RUN_ERROR` framing | `ag_ui::server` run driver |
//! | SSE encoding, `Accept` negotiation, CORS-compatible body | `ag_ui::encode` + `ag_ui::axum` |
//! | cancellation on client disconnect | `ag_ui::server::CancellationToken` |
//! | the agent loop, tools, model calls, A2UI payloads | this crate |
//!
//! # Endpoints
//!
//! | method | path | purpose |
//! | --- | --- | --- |
//! | `POST` | `/copilotkit` | run a turn (AG-UI SSE) |
//! | `POST` | `/copilotkit/agent/{agent_id}/run` | same, addressed by agent id |
//! | `GET` | `/health` | liveness |
//! | `GET` | `/copilotkit/info` | agent + catalog information |
//! | `GET`/`POST` | `/copilotkit/threads` | list / create threads |
//! | `DELETE` | `/copilotkit/threads/{thread_id}` | forget a thread |
//! | `POST` | `/copilotkit/agent/{agent_id}/stop/{thread_id}` | see [`stop_run`] |

pub mod middleware;

use crate::agent::config::AgentConfig;
use crate::agent::engine::{self, EventSink, RequestEnricher, SinkClosed, ThreadStore};
use crate::agent::llm::{ChatMessage, LlmError, OpenAiClient};
use crate::agent::new_id;
use crate::agent::tools::ToolRegistry;
use middleware::A2uiMiddleware;

use ag_ui::axum::RouterExt;
use ag_ui::server::{Agent, Result as AgUiResult, RunContext};
use ag_ui::{Event, RunOutcome};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

// ─────────────────────────────────────────────────────────────────────────
// The agent
// ─────────────────────────────────────────────────────────────────────────

/// The application half of the AG-UI endpoint.
///
/// `State = ()` because the conversation arrives with every run (AG-UI is
/// stateless per request) — there is no per-run state to publish.
pub struct CopilotAgent {
    cfg: AgentConfig,
    registry: ToolRegistry,
    llm: OpenAiClient,
    threads: ThreadStore,
    middleware: A2uiMiddleware,
    started: Instant,
}

impl CopilotAgent {
    /// Build the agent (opens the model client and registers the tools).
    pub fn new(cfg: AgentConfig) -> Result<Self, String> {
        let llm = OpenAiClient::new(&cfg).map_err(|e| e.to_string())?;
        Ok(CopilotAgent {
            cfg,
            registry: ToolRegistry::with_builtins(),
            llm,
            threads: ThreadStore::new(),
            middleware: A2uiMiddleware::new(cfg.catalog_id.clone()),
            started: Instant::now(),
        })
    }

    /// The enricher handed to the engine (A2UI catalog guidance + surfaces).
    fn enricher(&self) -> &dyn RequestEnricher {
        &self.middleware
    }
}

impl Agent for CopilotAgent {
    type State = ();

    /// Serve one run.
    ///
    /// Everything protocol-shaped — `RUN_STARTED`, ordering checks,
    /// `RUN_FINISHED`/`RUN_ERROR`, SSE framing — belongs to the ag-ui driver,
    /// which calls this method in the middle of a run it has already opened.
    async fn run(&self, ctx: &mut RunContext<()>) -> AgUiResult<RunOutcome> {
        let thread_id = ctx.thread_id().as_str().to_string();
        let messages = history_from(ctx);

        if messages.is_empty() {
            return Err(error_message("the request contained no user message"));
        }

        let mut sink = RunContextSink { ctx };
        match engine::run_turn(
            &self.cfg,
            &self.llm,
            &self.registry,
            &self.threads,
            self.enricher(),
            &thread_id,
            messages,
            &mut sink,
        )
        .await
        {
            Ok(()) => Ok(RunOutcome::Success),
            Err(e) => Err(engine_error(e)),
        }
    }
}

/// Adapts `ag_ui::server::RunContext` to the engine's [`EventSink`].
///
/// `RunContext::emit` already fails once the run is cancelled or the client has
/// disconnected, so cancellation needs no extra plumbing: the engine unwinds on
/// the first failed emit.
struct RunContextSink<'a> {
    ctx: &'a mut RunContext<()>,
}

impl EventSink for RunContextSink<'_> {
    fn emit(&mut self, event: Event) -> Result<(), SinkClosed> {
        self.ctx.emit(event).map_err(|_| SinkClosed)
    }
}

/// Turn a model failure into a `RUN_ERROR` payload.
fn engine_error(error: LlmError) -> ag_ui::server::Error {
    error_message(error.to_string())
}

/// Build a `server::Error` carrying a message.
///
/// `ag_ui::server::Error` exposes no message constructor, but the crate does
/// convert `serde_json::Error` (it uses `?` on `serde_json::from_value` while
/// decoding state), so the text rides in as a JSON error.
fn error_message(message: impl Into<String>) -> ag_ui::server::Error {
    serde_json::Error::io(std::io::Error::other(message.into())).into()
}

// ─────────────────────────────────────────────────────────────────────────
// Reading the conversation out of the run context
// ─────────────────────────────────────────────────────────────────────────

/// Read the client's conversation from the run context.
///
/// AG-UI sends the whole history with every run, so the context — not a local
/// store — is the source of truth. `Message` is a protocol union whose content
/// may be a string or an array of parts, so it is flattened through its JSON
/// form rather than matched field by field; that keeps working when the
/// protocol adds a message kind.
fn history_from(ctx: &RunContext<()>) -> Vec<ChatMessage> {
    let Ok(value) = serde_json::to_value(ctx.messages()) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items.iter().filter_map(message_to_chat).collect()
}

/// Convert one serialized AG-UI message into a model message.
///
/// Messages with no text (tool traffic the frontend echoes back, empty
/// assistant turns) are dropped — the model must not see them twice.
fn message_to_chat(message: &Value) -> Option<ChatMessage> {
    let role = message.get("role")?.as_str()?;
    let text = content_to_text(message.get("content")?);
    if text.trim().is_empty() {
        return None;
    }
    match role.to_ascii_lowercase().as_str() {
        // AG-UI has both `system` and `developer` roles; both are instructions.
        "system" | "developer" => Some(ChatMessage::system(text)),
        "user" => Some(ChatMessage::user(text)),
        "assistant" => Some(ChatMessage::assistant(text)),
        _ => None,
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
// Side endpoints
// ─────────────────────────────────────────────────────────────────────────

async fn health(State(agent): State<Arc<CopilotAgent>>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "sentence-video agent",
        "uptimeSeconds": agent.started.elapsed().as_secs(),
        "threads": agent.threads.len(),
        "credentials": agent.cfg.has_credentials(),
        "protocol": "ag-ui (ag-ui crate)",
    }))
}

async fn info(State(agent): State<Arc<CopilotAgent>>) -> impl IntoResponse {
    let cfg = &agent.cfg;
    Json(json!({
        "agents": {
            cfg.agent_id.clone(): {
                "name": cfg.agent_name,
                "description": cfg.agent_description,
                "model": agent.llm.model(),
            }
        },
        "a2ui": {
            "defaultCatalogId": agent.middleware.catalog_id(),
            "injectA2UITool": true,
        },
        "tools": agent.registry.names(),
        "endpoint": agent.llm.endpoint(),
    }))
}

async fn list_threads(State(agent): State<Arc<CopilotAgent>>) -> impl IntoResponse {
    Json(json!({ "threads": agent.threads.summaries() }))
}

async fn create_thread() -> impl IntoResponse {
    let thread_id = new_id("thread");
    (StatusCode::CREATED, Json(json!({ "id": thread_id })))
}

async fn delete_thread(
    State(agent): State<Arc<CopilotAgent>>,
    Path(thread_id): Path<String>,
) -> impl IntoResponse {
    let existed = agent.threads.clear(&thread_id);
    Json(json!({ "deleted": existed, "id": thread_id }))
}

/// `POST /copilotkit/agent/{agent_id}/stop/{thread_id}`
///
/// Kept for API compatibility only. There is nothing to signal here: a run is
/// cancelled by the client closing the SSE response, which the ag-ui transport
/// turns into a cancel token that fails every subsequent emit. Reporting that
/// honestly is better than pretending this endpoint stops anything.
async fn stop_run(
    State(agent): State<Arc<CopilotAgent>>,
    Path((agent_id, thread_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if agent_id != agent.cfg.agent_id {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("unknown agent '{agent_id}'") })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "stopped": false,
            "threadId": thread_id,
            "reason": "cancellation is driven by closing the SSE response, not by this endpoint",
        })),
    )
}

// ─────────────────────────────────────────────────────────────────────────
// Server
// ─────────────────────────────────────────────────────────────────────────

/// Build the router. Exposed for tests and for embedding in another server.
pub fn router(agent: Arc<CopilotAgent>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/copilotkit/info", get(info))
        .route("/copilotkit/threads", get(list_threads).post(create_thread))
        .route("/copilotkit/threads/{thread_id}", delete(delete_thread))
        .route(
            "/copilotkit/agent/{agent_id}/stop/{thread_id}",
            post(stop_run),
        )
        .with_state(Arc::clone(&agent))
        // `route_agui` supplies request parsing, ordering verification, SSE
        // framing, content negotiation and disconnect cancellation.
        .route_agui("/copilotkit", Arc::clone(&agent))
        .route_agui("/copilotkit/agent/{agent_id}/run", agent)
        // Permissive CORS mirrors the old Bun runtime and the FastAPI app; the
        // service binds to loopback by default, so this is a local-only surface.
        .layer(tower_http::cors::CorsLayer::permissive())
}

/// Start the service and block until the process ends.
pub fn serve(cfg: AgentConfig) -> Result<(), String> {
    let bind = cfg.bind_addr();
    let agent = Arc::new(CopilotAgent::new(cfg)?);
    let app = router(Arc::clone(&agent));

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
            agent.cfg.agent_id,
            agent.llm.model(),
            agent.middleware.catalog_id()
        );
        println!(
            "sentence-video agent: tools: {}",
            agent.registry.names().join(", ")
        );
        if !agent.cfg.has_credentials() {
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
    use crate::agent::llm::Role;

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
        let chat = |v: Value| message_to_chat(&v);

        assert_eq!(
            chat(json!({ "role": "user", "content": "hi" })).unwrap().role,
            Role::User
        );
        assert_eq!(
            chat(json!({ "role": "developer", "content": "rules" })).unwrap().role,
            Role::System
        );
        assert!(chat(json!({ "role": "tool", "content": "ignored" })).is_none());
        assert!(chat(json!({ "role": "assistant", "content": "" })).is_none());
        assert!(chat(json!({ "role": "user" })).is_none());
        assert_eq!(
            chat(json!({ "role": "user", "content": [{ "type": "text", "text": "second" }] }))
                .unwrap()
                .content
                .as_deref(),
            Some("second")
        );
    }

    #[test]
    fn history_comes_from_the_run_context() {
        let input: ag_ui::RunAgentInput = serde_json::from_value(json!({
            "threadId": "thread-1",
            "runId": "run-1",
            "messages": [
                { "id": "m1", "role": "system", "content": "rules" },
                { "id": "m2", "role": "user", "content": "hello" }
            ]
        }))
        .expect("RunAgentInput must deserialize the AG-UI request shape");

        let (ctx, _receiver) = RunContext::<()>::new(input).expect("context");
        let messages = history_from(&ctx);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].content.as_deref(), Some("hello"));
        assert_eq!(ctx.thread_id().as_str(), "thread-1");
    }

    #[test]
    fn agent_reports_the_configured_identity_and_tools() {
        let agent = CopilotAgent::new(AgentConfig::default()).unwrap();
        assert_eq!(agent.cfg.agent_id, "sample_agent");
        assert_eq!(agent.middleware.catalog_id(), "generative-agent-catalog");
        assert!(agent.registry.names().contains(&"display_register_form"));
        assert!(!agent.cfg.has_credentials());
    }

    #[test]
    fn errors_are_reported_as_run_errors_not_panics() {
        // Constructing a server error must not require a variant this crate
        // cannot see; the message has to survive the trip.
        let error = error_message("boom");
        assert!(error.to_string().contains("boom"));
    }
}
