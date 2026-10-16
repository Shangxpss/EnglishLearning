//! Push transport for job status.
//!
//! Whatever the transport, the same small versioned event is published — and
//! delivery is **at-least-once** (SSE replay, MQTT QoS 1), so the subscriber
//! must dedupe on `(job_id, state)`. That is exactly why `job_id` is in the
//! payload: the standard at-least-once → idempotent-consumer pairing.
//!
//! [`SseHub`] is the local adapter: a `tokio::sync::broadcast` channel fed to an
//! axum `Sse` route. A future MQTT adapter (`rumqttc`, publishing retained to
//! `sentence-video/jobs/{id}/status`) implements the same [`Notifier`] trait —
//! the application code above never learns which one is in use.

use super::{now_secs, Job};
use serde::Serialize;
use tokio::sync::broadcast;

/// One job status event. Keep it small: ids + a state — details come from
/// `GET /api/jobs`.
#[derive(Clone, Debug, Serialize)]
pub struct JobEvent {
    /// Payload version, so a future shape change is negotiable.
    pub v: u32,
    pub job_id: String,
    pub kind: String,
    pub state: String,
    pub media_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub done: i64,
    pub total: i64,
    pub attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub finished_at: i64,
}

impl JobEvent {
    pub const VERSION: u32 = 1;

    /// Snapshot a row as an event. `finished_at` falls back to "now" for a row
    /// that has not been updated with a terminal timestamp yet.
    pub fn from_job(job: &Job, error: Option<String>) -> JobEvent {
        JobEvent {
            v: Self::VERSION,
            job_id: job.id.clone(),
            kind: job.kind.clone(),
            state: job.state.clone(),
            media_path: job.media_path.clone(),
            session_id: job.session_id.clone(),
            stage: job.stage.clone(),
            done: job.done,
            total: job.total,
            attempts: job.attempts,
            error: error.or_else(|| job.last_error.clone()),
            finished_at: job.finished_at.unwrap_or_else(now_secs),
        }
    }
}

/// Push a status event to whoever is watching.
pub trait Notifier: Send + Sync {
    fn publish(&self, ev: &JobEvent) -> Result<(), String>;
}

/// The local (no-broker) notifier: a broadcast channel an axum `Sse` route
/// subscribes to. Lagging subscribers drop old events rather than blocking the
/// publisher — the client re-syncs from `GET /api/jobs`.
pub struct SseHub {
    tx: broadcast::Sender<JobEvent>,
}

impl SseHub {
    /// Buffer a generous burst so a slow tab does not force lag; the channel
    /// capacity is events, not bytes.
    pub fn new() -> SseHub {
        let (tx, _rx) = broadcast::channel(512);
        SseHub { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.tx.subscribe()
    }
}

impl Default for SseHub {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier for SseHub {
    fn publish(&self, ev: &JobEvent) -> Result<(), String> {
        // A send failure only means nobody is listening right now, which is the
        // normal case (nobody has the Manage page open). Not an error.
        let _ = self.tx.send(ev.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Job, STATE_DONE};

    fn sample() -> Job {
        Job {
            id: "j1".to_string(),
            media_path: "/a.mp4".to_string(),
            cache_key: "k".to_string(),
            kind: "build".to_string(),
            state: STATE_DONE.to_string(),
            stage: None,
            done: 0,
            total: 0,
            attempts: 1,
            max_attempts: 3,
            payload: None,
            last_error: None,
            session_id: Some("s1".to_string()),
            enqueued_at: 10,
            started_at: Some(11),
            finished_at: Some(12),
        }
    }

    #[test]
    fn event_carries_the_ids_needed_to_dedupe_and_answer() {
        let ev = JobEvent::from_job(&sample(), None);
        assert_eq!(ev.v, JobEvent::VERSION);
        assert_eq!(ev.job_id, "j1");
        assert_eq!(ev.state, "done");
        assert_eq!(ev.session_id.as_deref(), Some("s1"));
        assert_eq!(ev.finished_at, 12);
        assert!(ev.error.is_none());
    }

    #[test]
    fn a_published_event_reaches_a_subscriber() {
        let hub = SseHub::new();
        let mut rx = hub.subscribe();
        hub.publish(&JobEvent::from_job(&sample(), Some("boom".to_string())))
            .unwrap();
        let got = rx.try_recv().expect("event delivered");
        assert_eq!(got.error.as_deref(), Some("boom"));
    }
}
