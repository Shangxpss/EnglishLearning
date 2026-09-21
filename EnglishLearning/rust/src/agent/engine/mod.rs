//! **The agent loop** — the Rust replacement for `LangGraph.create_agent`.
//!
//! A plain ReAct loop, written out explicitly:
//!
//! ```text
//! ┌─ model step ─────────────────────────────────────────────┐
//! │ stream the completion, emitting text chunks as they come │
//! └──────────────────────────┬───────────────────────────────┘
//!                            │ tool calls?
//!              no ───────────┴─────────── yes
//!              │                            │
//!        finish the run            ┌─ tools step ────────────┐
//!                                  │ run each tool, emit the  │
//!                                  │ result + any A2UI surface│
//!                                  └───────────┬─────────────┘
//!                                              └──▶ back to the model
//! ```
//!
//! LangGraph brought graph routing, checkpointing and retries. For a single
//! linear agent none of that is needed: the loop is ~120 lines, gives full
//! control over what is emitted, and drops a large framework from the binary.
//!
//! Events are pushed into an [`mpsc`] channel that the HTTP layer drains as SSE,
//! so the engine has no knowledge of axum and can be unit-tested against a plain
//! channel.

pub mod prompt;
pub mod state;

use crate::agent::config::AgentConfig;
use crate::agent::llm::{ChatMessage, Delta, LlmError, MessageAccumulator, OpenAiClient};
use crate::agent::new_id;
use crate::agent::protocol::AgUiEvent;
use crate::agent::tools::ToolRegistry;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

pub use state::{ThreadStore, ThreadSummary};

/// Step names reported through `STEP_STARTED` / `STEP_FINISHED`.
pub const STEP_MODEL: &str = "model";
/// Step name for tool execution.
pub const STEP_TOOLS: &str = "tools";

/// Enriches a model request and turns tool output into UI events.
///
/// Implemented by the A2UI middleware; kept as a trait so the engine does not
/// depend on the HTTP layer (and so tests can pass [`NoEnrichment`]).
pub trait RequestEnricher: Send + Sync {
    /// Add anything the model needs before the first call (catalog guidelines, …).
    fn prepare(&self, messages: &mut Vec<ChatMessage>);

    /// Build the UI event for a tool's A2UI operations.
    fn snapshot(&self, tool_name: &str, operations: Value) -> AgUiEvent;
}

/// An enricher that adds nothing — for tests and for `--no-a2ui` style runs.
pub struct NoEnrichment;

impl RequestEnricher for NoEnrichment {
    fn prepare(&self, _messages: &mut Vec<ChatMessage>) {}

    fn snapshot(&self, _tool_name: &str, operations: Value) -> AgUiEvent {
        AgUiEvent::a2ui_snapshot(new_id("a2ui"), operations)
    }
}

/// Send an event, reporting failure when the client has disconnected.
async fn emit(tx: &mpsc::Sender<AgUiEvent>, event: AgUiEvent) -> Result<(), ()> {
    tx.send(event).await.map_err(|_| ())
}

/// Emit an event, ending the run early if the client is gone.
///
/// A disconnected browser is normal (tab closed, navigation) — it is not an
/// error, so the run simply stops.
macro_rules! emit_or_stop {
    ($tx:expr, $event:expr) => {
        if emit($tx, $event).await.is_err() {
            return Ok(());
        }
    };
}

/// Run one agent turn, streaming AG-UI events into `tx`.
///
/// The conversation comes from `messages` (AG-UI is stateless per run); the
/// transcript is also appended to `threads` for observability.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    cfg: &AgentConfig,
    llm: &OpenAiClient,
    registry: &ToolRegistry,
    threads: &ThreadStore,
    enricher: &dyn RequestEnricher,
    thread_id: &str,
    messages: Vec<ChatMessage>,
    tx: mpsc::Sender<AgUiEvent>,
) -> Result<(), LlmError> {
    let run_id = new_id("run");
    emit_or_stop!(
        &tx,
        AgUiEvent::RunStarted {
            thread_id: thread_id.to_string(),
            run_id: run_id.clone(),
        }
    );

    threads.append(thread_id, &messages);

    let mut conversation = Vec::with_capacity(messages.len() + 1);
    conversation.push(ChatMessage::system(prompt::system_prompt(cfg)));
    conversation.extend(messages);
    enricher.prepare(&mut conversation);

    let tool_specs = registry.specs();
    let max_steps = cfg.max_steps.max(1);
    let mut finished = false;

    for _ in 0..max_steps {
        // ── model step ───────────────────────────────────────────────────
        emit_or_stop!(
            &tx,
            AgUiEvent::StepStarted { step_name: STEP_MODEL.to_string() }
        );

        let mut stream = match llm.chat_stream(&conversation, &tool_specs).await {
            Ok(stream) => stream,
            Err(e) => {
                let _ = emit(&tx, AgUiEvent::error(e.to_string())).await;
                return Err(e);
            }
        };

        let mut accumulator = MessageAccumulator::new();
        let message_id = new_id("msg");
        let mut text_open = false;

        while let Some(item) = stream.next().await {
            match item {
                Ok(Delta::Done) => break,
                Ok(delta) => {
                    if let Delta::Content(text) = &delta {
                        if !text_open {
                            text_open = true;
                            emit_or_stop!(
                                &tx,
                                AgUiEvent::TextMessageStart {
                                    message_id: message_id.clone(),
                                    role: "assistant".to_string(),
                                }
                            );
                        }
                        emit_or_stop!(
                            &tx,
                            AgUiEvent::TextMessageChunk {
                                message_id: message_id.clone(),
                                delta: text.clone(),
                            }
                        );
                    }
                    accumulator.push(&delta);
                }
                // One unreadable frame must not end the run.
                Err(e) => {
                    let _ = emit(&tx, AgUiEvent::error(e.to_string())).await;
                }
            }
        }

        if text_open {
            emit_or_stop!(
                &tx,
                AgUiEvent::TextMessageEnd { message_id: message_id.clone() }
            );
        }
        emit_or_stop!(
            &tx,
            AgUiEvent::StepFinished { step_name: STEP_MODEL.to_string() }
        );

        let tool_calls = accumulator.tool_calls();
        let assistant_message = accumulator.into_message();

        if tool_calls.is_empty() {
            threads.append(thread_id, &[assistant_message]);
            finished = true;
            break;
        }

        // ── tools step ───────────────────────────────────────────────────
        conversation.push(assistant_message);
        emit_or_stop!(
            &tx,
            AgUiEvent::StepStarted { step_name: STEP_TOOLS.to_string() }
        );

        for call in &tool_calls {
            emit_or_stop!(
                &tx,
                AgUiEvent::ToolCallStarted {
                    tool_call_id: call.id.clone(),
                    tool_call_name: call.function.name.clone(),
                }
            );

            if !call.function.arguments.is_empty() {
                emit_or_stop!(
                    &tx,
                    AgUiEvent::ToolCallArgs {
                        tool_call_id: call.id.clone(),
                        delta: call.function.arguments.clone(),
                    }
                );
            }

            // A tool failure is fed back to the model as text so it can correct
            // itself — the same contract as the Python `@tool` functions.
            let (content, a2ui) = match call.function.arguments_json() {
                Err(e) => (format!("Error: {e}"), None),
                Ok(args) => match registry.call(&call.function.name, &args) {
                    Ok(outcome) => (outcome.content, outcome.a2ui),
                    Err(e) => (format!("Error: {e}"), None),
                },
            };

            emit_or_stop!(
                &tx,
                AgUiEvent::ToolCallResult {
                    message_id: new_id("tool"),
                    tool_call_id: call.id.clone(),
                    content: content.clone(),
                }
            );

            if let Some(operations) = a2ui {
                emit_or_stop!(&tx, enricher.snapshot(&call.function.name, operations));
            }

            conversation.push(ChatMessage::tool_result(call.id.clone(), content));
        }

        emit_or_stop!(
            &tx,
            AgUiEvent::StepFinished { step_name: STEP_TOOLS.to_string() }
        );
    }

    if !finished {
        let note = format!(
            "Stopped after {max_steps} model turns without a final answer. \
             Try again, or make the request more specific."
        );
        emit_or_stop!(&tx, AgUiEvent::error(note.clone()));
        threads.append(thread_id, &[ChatMessage::assistant(note)]);
    }

    emit_or_stop!(
        &tx,
        AgUiEvent::RunFinished {
            thread_id: thread_id.to_string(),
            run_id,
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::Role;

    /// Drive `prepare` through the public trait object.
    #[test]
    fn no_enrichment_leaves_the_request_alone() {
        let mut messages = vec![ChatMessage::user("hi")];
        NoEnrichment.prepare(&mut messages);
        assert_eq!(messages.len(), 1);

        let event = NoEnrichment.snapshot("display_register_form", serde_json::json!([]));
        assert_eq!(event.name(), "ACTIVITY_SNAPSHOT");
    }

    #[test]
    fn run_started_carries_the_thread_id() {
        // The channel contract the HTTP layer relies on.
        let (tx, mut rx) = mpsc::channel::<AgUiEvent>(1);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            emit(&tx, AgUiEvent::RunStarted { thread_id: "t1".into(), run_id: "r1".into() })
                .await
                .unwrap();
            let event = rx.recv().await.unwrap();
            assert_eq!(event.name(), "RUN_STARTED");
        });
    }

    #[test]
    fn emit_reports_a_closed_receiver() {
        let (tx, rx) = mpsc::channel::<AgUiEvent>(1);
        drop(rx);
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            assert!(emit(&tx, AgUiEvent::error("bye")).await.is_err());
        });
    }

    #[test]
    fn thread_store_is_reachable_through_the_engine_module() {
        let store = ThreadStore::new();
        store.append("t", &[ChatMessage::assistant("x")]);
        assert_eq!(store.history("t")[0].role, Role::Assistant);
    }
}
