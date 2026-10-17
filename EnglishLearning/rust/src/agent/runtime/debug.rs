//! **Debug event bus** — a tap on the AG-UI events a run emits.
//!
//! Reference: CopilotKit's runtime constructs a `debugEventBus` and hands it to
//! every SSE handler (`@copilotkit/runtime/dist/v2/runtime/core/debug-event-bus.mjs`),
//! so the web inspector can watch a run's events as they are produced. The shape
//! is the same here: [`DebugBus`] rebroadcasts what [`DebugTap`] sees, and
//! [`events`] exposes a subscription over SSE.
//!
//! This is observability only — a run behaves identically with or without a
//! subscriber:
//!
//! * `broadcast` never blocks a run. A subscriber that cannot keep up is told it
//!   fell behind and loses the oldest events, rather than stalling the run.
//! * With nobody subscribed, publishing is skipped, so the tap costs one integer
//!   read and no clone per event.
//!
//! Boundary worth knowing: the tap sits on the engine's [`EventSink`], so it sees
//! what the *agent* emits — steps, text, tool calls, A2UI surfaces. The
//! `RUN_STARTED` / `RUN_FINISHED` / `RUN_ERROR` framing is written by the ag-ui
//! run driver around this sink and is therefore not in the stream.

use crate::agent::engine::{EventSink, SinkClosed};
use crate::agent::runtime::CopilotAgent;
use ag_ui::Event;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue},
    response::Response,
};
use serde::Serialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// How many events a subscriber may fall behind before it starts losing them.
const BACKLOG: usize = 256;

/// One observed event, tagged with the run it belongs to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEvent {
    pub thread_id: String,
    /// Milliseconds since the Unix epoch, at the moment it was emitted.
    pub at: u64,
    pub event: Event,
}

/// A rebroadcast of run events to any number of subscribers.
pub struct DebugBus {
    tx: broadcast::Sender<DebugEvent>,
}

impl Default for DebugBus {
    fn default() -> Self {
        DebugBus::new()
    }
}

impl DebugBus {
    /// An empty bus with room for [`BACKLOG`] events.
    pub fn new() -> Self {
        DebugBus {
            tx: broadcast::channel(BACKLOG).0,
        }
    }

    /// Watch the bus; called once per debug connection.
    pub fn subscribe(&self) -> broadcast::Receiver<DebugEvent> {
        self.tx.subscribe()
    }

    /// Publish one event. Does nothing when nobody is listening.
    fn publish(&self, thread_id: &str, event: &Event) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        // An error here only ever means every subscriber left in between.
        let _ = self.tx.send(DebugEvent {
            thread_id: thread_id.to_string(),
            at: now_ms(),
            event: event.clone(),
        });
    }
}

/// A sink that taps each event on its way to the real sink.
pub struct DebugTap<'a> {
    inner: &'a mut dyn EventSink,
    bus: &'a DebugBus,
    thread_id: &'a str,
}

impl<'a> DebugTap<'a> {
    /// Wrap `inner`, recording what it is asked to emit.
    pub fn wrap(inner: &'a mut dyn EventSink, bus: &'a DebugBus, thread_id: &'a str) -> Self {
        DebugTap {
            inner,
            bus,
            thread_id,
        }
    }
}

impl EventSink for DebugTap<'_> {
    fn emit(&mut self, event: Event) -> Result<(), SinkClosed> {
        // Tap before delivering: an event the client never received is exactly
        // what a debugger is looking for when a chat looks truncated.
        self.bus.publish(self.thread_id, &event);
        self.inner.emit(event)
    }
}

/// `GET /copilotkit/debug/events` — subscribe to the run event stream.
///
/// One SSE frame per event the tap sees: `{ "threadId", "at", "event" }`. The
/// stream lasts as long as the connection, and a subscriber that falls behind is
/// told how many events it missed (`event: lagged`) before the flow resumes.
pub async fn events(State(agent): State<Arc<CopilotAgent>>) -> Response {
    let rx = agent.debug.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        // One pull per event: `Lagged` keeps the receiver (it resumes at the
        // oldest event still held), `Closed` ends the stream.
        match rx.recv().await {
            Ok(observed) => Some((Ok::<_, std::convert::Infallible>(frame(&observed)), rx)),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                let line = format!("event: lagged\ndata: {{\"skipped\":{skipped}}}\n\n");
                Some((Ok(line), rx))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    });

    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Encode one observed event as an SSE frame.
fn frame(observed: &DebugEvent) -> String {
    match serde_json::to_string(observed) {
        Ok(json) => format!("data: {json}\n\n"),
        // An AG-UI event is plain data, so this is unreachable in practice;
        // reporting it beats dropping the event silently.
        Err(e) => format!(
            "event: error\ndata: {}\n\n",
            serde_json::json!({ "error": e.to_string() })
        ),
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::engine::RecordingSink;

    #[test]
    fn a_subscriber_sees_each_event_the_run_emits() {
        let bus = DebugBus::new();
        let mut subscriber = bus.subscribe();
        let mut inner = RecordingSink::new();
        let mut tap = DebugTap::wrap(&mut inner, &bus, "t1");

        tap.emit(Event::step_started("model")).unwrap();

        // The run is unaffected, and the subscriber gets the same event.
        assert_eq!(inner.names(), vec!["STEP_STARTED"]);
        let observed = subscriber.try_recv().unwrap();
        assert_eq!(observed.thread_id, "t1");
        assert!(observed.at > 0);
        assert_eq!(observed.event, Event::step_started("model"));
    }

    #[test]
    fn with_no_subscriber_the_tap_is_invisible() {
        let bus = DebugBus::new();
        let mut inner = RecordingSink::new();
        let mut tap = DebugTap::wrap(&mut inner, &bus, "t1");

        tap.emit(Event::step_started("model")).unwrap();

        assert_eq!(inner.names(), vec!["STEP_STARTED"]);
        // Nothing was published, so a subscriber that arrives later starts empty.
        let mut late = bus.subscribe();
        assert!(late.try_recv().is_err());
    }
}
