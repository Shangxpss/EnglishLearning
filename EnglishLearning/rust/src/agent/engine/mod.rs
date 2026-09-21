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
//!        finish the turn            ┌─ tools step ────────────┐
//!                                  │ run each tool, emit the  │
//!                                  │ result + any A2UI surface│
//!                                  └───────────┬─────────────┘
//!                                              └──▶ back to the model
//! ```
//!
//! LangGraph brought graph routing, checkpointing and retries. For a single
//! linear agent none of that is needed: the loop is ~130 lines, gives full
//! control over what is emitted, and drops a large framework from the binary.
//!
//! # Relationship to `ag_ui`
//!
//! Events are `ag_ui::Event` values, and the loop writes them through the
//! [`EventSink`] trait. Production passes a sink backed by
//! `ag_ui::server::RunContext`; tests pass [`RecordingSink`]. The engine
//! therefore never touches HTTP, and the AG-UI runtime owns the parts that are
//! protocol-level rather than agent-level:
//!
//! * `RUN_STARTED` / `RUN_FINISHED` / `RUN_ERROR` are emitted by the ag-ui run
//!   driver, **not** here — an `Agent` implementation only reports
//!   [`ag_ui::RunOutcome`] (or returns an error) and the driver frames the run.
//! * event ordering is checked by the crate's `verify` state machine.
//! * cancellation is the crate's cancel token; a cancelled run makes every
//!   [`EventSink::emit`] fail, which unwinds the loop through
//!   [`SinkClosed`].

pub mod prompt;
pub mod state;

use crate::agent::config::AgentConfig;
use crate::agent::llm::{ChatMessage, Delta, LlmError, MessageAccumulator, OpenAiClient};
use crate::agent::new_id;
use crate::agent::tools::ToolRegistry;
use ag_ui::{Event, TextMessageRole};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::mpsc as std_mpsc;

// `ThreadSummary` is part of the engine's public surface (the shape
// `ThreadStore::summaries` returns) but is referenced only by the runtime's
// handlers, so it is re-exported rather than warned about here.
#[allow(unused_imports)]
pub use state::{ThreadStore, ThreadSummary};

/// Step names reported through `STEP_STARTED` / `STEP_FINISHED`.
pub const STEP_MODEL: &str = "model";
/// Step name for tool execution.
pub const STEP_TOOLS: &str = "tools";

/// The client went away: disconnected, cancelled, or the run was torn down.
///
/// Not an error condition — the loop simply stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkClosed;

impl std::fmt::Display for SinkClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the event sink is closed")
    }
}

impl std::error::Error for SinkClosed {}

/// Where the engine writes AG-UI events.
///
/// Implemented for `ag_ui::server::RunContext` in
/// [`crate::agent::runtime`], and for the recording sinks below.
pub trait EventSink: Send {
    /// Deliver one event.
    ///
    /// Returns [`SinkClosed`] once the run is cancelled or the client has gone;
    /// the engine treats that as "stop now".
    fn emit(&mut self, event: Event) -> Result<(), SinkClosed>;
}

/// A sink that keeps events in memory.
///
/// Used by the tests here and by callers that want the events rather than a
/// stream (for example an embedding application).
#[derive(Default)]
pub struct RecordingSink {
    /// Every event emitted, in order.
    pub events: Vec<Event>,
}

impl RecordingSink {
    /// An empty sink.
    pub fn new() -> Self {
        RecordingSink::default()
    }

    /// The `type` discriminator of each event, for compact assertions.
    pub fn names(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|event| {
                serde_json::to_value(event)
                    .ok()
                    .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
            })
            .collect()
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, event: Event) -> Result<(), SinkClosed> {
        self.events.push(event);
        Ok(())
    }
}

/// A sink that forwards events to a channel.
///
/// Lets a caller drain the run from another thread without depending on axum or
/// the ag-ui server runtime.
pub struct ChannelSink {
    tx: std_mpsc::Sender<Event>,
}

impl ChannelSink {
    /// Wrap a channel sender.
    pub fn new(tx: std_mpsc::Sender<Event>) -> Self {
        ChannelSink { tx }
    }
}

impl EventSink for ChannelSink {
    fn emit(&mut self, event: Event) -> Result<(), SinkClosed> {
        self.tx.send(event).map_err(|_| SinkClosed)
    }
}

/// Enriches a model request and turns tool output into UI events.
///
/// Implemented by the A2UI middleware; kept as a trait so the engine does not
/// depend on the HTTP layer (and so tests can pass [`NoEnrichment`]).
pub trait RequestEnricher: Send + Sync {
    /// Add anything the model needs before the first call (catalog guidelines, …).
    fn prepare(&self, messages: &mut Vec<ChatMessage>);

    /// Build the UI event for a tool's A2UI operations.
    fn snapshot(&self, tool_name: &str, operations: Value) -> Event;
}

/// An enricher that adds nothing — for tests and for `--no-a2ui` style runs.
pub struct NoEnrichment;

impl RequestEnricher for NoEnrichment {
    fn prepare(&self, _messages: &mut Vec<ChatMessage>) {}

    fn snapshot(&self, tool_name: &str, operations: Value) -> Event {
        Event::activity_snapshot(
            new_id("a2ui"),
            crate::agent::protocol::A2UI_ACTIVITY_TYPE,
            crate::agent::protocol::a2ui::activity_content(tool_name, operations),
        )
    }
}

/// Emit an event, ending the turn early if the client is gone.
///
/// A disconnected browser is normal (tab closed, navigation, stop button), so
/// the loop unwinds quietly instead of reporting an error.
macro_rules! emit_or_stop {
    ($sink:expr, $event:expr) => {
        if $sink.emit($event).is_err() {
            return Ok(());
        }
    };
}

/// Run one agent turn, writing AG-UI events into `sink`.
///
/// The conversation comes from `messages` (AG-UI is stateless per run); the
/// transcript is also appended to `threads` for observability. The run framing
/// events (`RUN_STARTED`/`RUN_FINISHED`/`RUN_ERROR`) are the caller's job.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    cfg: &AgentConfig,
    llm: &OpenAiClient,
    registry: &ToolRegistry,
    threads: &ThreadStore,
    enricher: &dyn RequestEnricher,
    thread_id: &str,
    messages: Vec<ChatMessage>,
    sink: &mut dyn EventSink,
) -> Result<(), LlmError> {
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
        emit_or_stop!(sink, Event::step_started(STEP_MODEL));

        let mut stream = match llm.chat_stream(&conversation, &tool_specs).await {
            Ok(stream) => stream,
            Err(e) => {
                // The driver turns this into RUN_ERROR; nothing more to emit.
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
                                sink,
                                Event::text_message_start(
                                    message_id.clone(),
                                    TextMessageRole::Assistant
                                )
                            );
                        }
                        emit_or_stop!(
                            sink,
                            Event::text_message_content(message_id.clone(), text.clone())
                        );
                    }
                    accumulator.push(&delta);
                }
                // One unreadable frame must not end the run. Report it as text
                // so the user sees something rather than a silent truncation.
                Err(e) => {
                    if text_open {
                        emit_or_stop!(
                            sink,
                            Event::text_message_content(
                                message_id.clone(),
                                format!("\n[stream warning: {e}]")
                            )
                        );
                    }
                }
            }
        }

        if text_open {
            emit_or_stop!(sink, Event::text_message_end(message_id.clone()));
        }
        emit_or_stop!(sink, Event::step_finished(STEP_MODEL));

        let tool_calls = accumulator.tool_calls();
        let assistant_message = accumulator.into_message();

        if tool_calls.is_empty() {
            threads.append(thread_id, &[assistant_message]);
            finished = true;
            break;
        }

        // ── tools step ───────────────────────────────────────────────────
        conversation.push(assistant_message);
        emit_or_stop!(sink, Event::step_started(STEP_TOOLS));

        for call in &tool_calls {
            emit_or_stop!(
                sink,
                Event::tool_call_start(call.id.clone(), call.function.name.clone())
            );

            if !call.function.arguments.is_empty() {
                emit_or_stop!(
                    sink,
                    Event::tool_call_args(call.id.clone(), call.function.arguments.clone())
                );
            }
            // Closes the call before its result, which is the order the
            // protocol's ordering verifier expects.
            emit_or_stop!(sink, Event::tool_call_end(call.id.clone()));

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
                sink,
                Event::tool_call_result(
                    new_id("tool"),
                    call.id.clone(),
                    content.clone()
                )
            );

            if let Some(operations) = a2ui {
                emit_or_stop!(sink, enricher.snapshot(&call.function.name, operations));
            }

            conversation.push(ChatMessage::tool_result(call.id.clone(), content));
        }

        emit_or_stop!(sink, Event::step_finished(STEP_TOOLS));
    }

    if !finished {
        // Out of turns: say so in the transcript instead of failing the run, so
        // the user keeps everything the agent already produced.
        let note = format!(
            "Stopped after {max_steps} model turns without a final answer. \
             Try again, or make the request more specific."
        );
        // One id for the whole message: start → content → end must agree, or the
        // ordering verifier rejects the run.
        let note_id = new_id("msg");
        emit_or_stop!(sink, Event::step_started(STEP_MODEL));
        emit_or_stop!(
            sink,
            Event::text_message_start(note_id.clone(), TextMessageRole::Assistant)
        );
        emit_or_stop!(
            sink,
            Event::text_message_content(note_id.clone(), note.clone())
        );
        emit_or_stop!(sink, Event::text_message_end(note_id));
        emit_or_stop!(sink, Event::step_finished(STEP_MODEL));
        threads.append(thread_id, &[ChatMessage::assistant(note)]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::Role;
    use serde_json::json;

    /// The `type` field of an event, as it goes on the wire.
    fn event_type(event: &Event) -> String {
        serde_json::to_value(event).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn recording_sink_collects_events_in_order() {
        let mut sink = RecordingSink::new();
        sink.emit(Event::step_started("model")).unwrap();
        sink.emit(Event::text_message_start("m1", TextMessageRole::Assistant))
            .unwrap();
        assert_eq!(sink.names(), vec!["STEP_STARTED", "TEXT_MESSAGE_START"]);
    }

    #[test]
    fn channel_sink_reports_a_closed_receiver() {
        let (tx, rx) = std_mpsc::channel();
        let mut sink = ChannelSink::new(tx);
        assert!(sink.emit(Event::step_started("model")).is_ok());
        drop(rx);
        assert_eq!(sink.emit(Event::step_started("model")), Err(SinkClosed));
    }

    #[test]
    fn no_enrichment_still_builds_a_valid_activity_snapshot() {
        let mut messages = vec![ChatMessage::user("hi")];
        NoEnrichment.prepare(&mut messages);
        assert_eq!(messages.len(), 1);

        let event = NoEnrichment.snapshot(
            "display_register_form",
            json!([{ "createSurface": { "surfaceId": "register-form" } }]),
        );
        assert_eq!(event_type(&event), "ACTIVITY_SNAPSHOT");

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["activityType"], crate::agent::protocol::A2UI_ACTIVITY_TYPE);
        assert_eq!(value["content"]["tool"], "display_register_form");
        assert_eq!(value["content"]["operations"][0]["createSurface"]["surfaceId"], "register-form");
    }

    #[test]
    fn thread_store_is_reachable_through_the_engine_module() {
        let store = ThreadStore::new();
        store.append("t", &[ChatMessage::assistant("x")]);
        assert_eq!(store.history("t")[0].role, Role::Assistant);
    }

    #[test]
    fn the_engine_does_not_frame_the_run() {
        // RUN_STARTED / RUN_FINISHED / RUN_ERROR belong to the ag-ui driver; the
        // engine must never emit them or the ordering verifier rejects the run.
        let mut sink = RecordingSink::new();
        sink.emit(Event::step_started("model")).unwrap();
        sink.emit(Event::step_finished("model")).unwrap();
        for name in sink.names() {
            assert!(!name.starts_with("RUN_"), "engine emitted a run framing event: {name}");
        }
    }
}
