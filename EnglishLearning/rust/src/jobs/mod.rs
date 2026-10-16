//! Durable, ordered, at-least-once work queue + completion notification.
//!
//! The two ports here are deliberately **synchronous** traits, handed out as
//! `Arc<dyn JobQueue>` / `Arc<dyn Notifier>` — exactly like [`crate::db::Store`].
//! `async fn` in a trait is not `dyn`-compatible, so the async web layer bridges
//! with `spawn_blocking` at the axum boundary instead (see [`crate::web`]).
//!
//! - [`JobQueue`] — implemented locally by [`SqliteQueue`]: a `jobs` table in the
//!   same bundled SQLite the app already ships, with a lease (visibility
//!   timeout), attempts with exponential backoff, a dead-letter terminal state
//!   and crash recovery ([`JobQueue::reclaim_expired`]). A broker adapter
//!   (`RabbitMQ`/`lapin`) can implement the same trait for a multi-node deploy
//!   without any application code changing.
//! - [`Notifier`] — implemented locally by [`SseHub`] over a
//!   `tokio::sync::broadcast` channel; an MQTT publisher (`rumqttc`) can
//!   implement the same trait later.
//!
//! The design rationale (why SQLite now and never Kafka for this) is in
//! `batch-job-queue-and-notification-stack.md`.

mod notify;
mod sqlite;
mod worker;

pub use notify::{JobEvent, Notifier, SseHub};
pub use sqlite::SqliteQueue;
pub use worker::{JobFailure, JobRunner, JobStage, WorkerPool};

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The handle the API hands back for a submitted job.
pub type JobId = String;

/// Terminal and in-flight states a job moves through.
pub const STATE_QUEUED: &str = "queued";
pub const STATE_RUNNING: &str = "running";
pub const STATE_DONE: &str = "done";
pub const STATE_FAILED: &str = "failed";
pub const STATE_CANCELLED: &str = "cancelled";

/// Seconds since the Unix epoch — the clock every timestamp column uses.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// What a caller asks to be queued.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub media_path: String,
    /// Media path + size + mtime — the idempotency key. Re-submitting the same
    /// unchanged file while it is queued or running joins the existing job.
    pub cache_key: String,
    /// `build` | `transcode` | `dub` | `platform`. Workers dispatch on it.
    pub kind: String,
    pub max_attempts: i64,
    /// Kind-specific options as JSON (see [`JobPayload`]).
    pub payload: Option<String>,
}

/// One job row, as the API and the notifier see it.
#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: JobId,
    pub media_path: String,
    pub cache_key: String,
    pub kind: String,
    pub state: String,
    pub stage: Option<String>,
    pub done: i64,
    pub total: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    #[serde(skip_serializing)]
    pub payload: Option<String>,
    pub last_error: Option<String>,
    pub session_id: Option<String>,
    pub enqueued_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl Job {
    /// Seconds this job has been running (0 before it starts).
    pub fn elapsed_secs(&self) -> i64 {
        match self.started_at {
            Some(t) if self.state == STATE_RUNNING => now_secs() - t,
            Some(t) => self.finished_at.unwrap_or_else(now_secs) - t,
            None => 0,
        }
    }
}

/// Kind-specific job options, carried as JSON so one table serves every kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JobPayload {
    /// Per-job override of the process-wide wav2vec2 alignment setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<bool>,
    /// Explicit subtitle for this media (otherwise auto-located).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_path: Option<String>,
}

impl JobPayload {
    /// Parse a stored payload; a malformed one degrades to "all defaults"
    /// rather than failing the job.
    pub fn parse(raw: Option<&str>) -> JobPayload {
        raw.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default()
    }
}

/// Progress a worker reports as it goes.
#[derive(Clone, Copy, Debug, Default)]
pub struct JobProgress<'a> {
    pub stage: Option<&'a str>,
    pub done: i64,
    pub total: i64,
}

/// Filter for [`JobQueue::list`]. Empty means "everything, newest first".
#[derive(Clone, Debug, Default)]
pub struct JobFilter {
    pub state: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
}

/// What [`JobQueue::fail`] decided to do with a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// Put back on the queue after `delay_secs` with exponential backoff.
    Requeued { delay_secs: i64 },
    /// Attempts are spent (or it is not retryable): parked as `failed`.
    DeadLetter,
}

/// Durable, ordered, at-least-once work queue.
///
/// Synchronous on purpose: it is handed out as `Arc<dyn JobQueue>` and called
/// from the blocking worker threads and from `spawn_blocking` in the web layer.
pub trait JobQueue: Send + Sync {
    /// Append a job. If an equivalent job is already queued or running (same
    /// media + cache key + kind), its existing id is returned instead of
    /// duplicating the work — re-submitting a file is naturally idempotent.
    fn enqueue(&self, spec: JobSpec) -> Result<JobId, String>;
    /// Lease the oldest claimable job, setting a visibility timeout of `lease`.
    /// `None` means the queue is empty (or everything is backing off).
    fn claim(&self, lease: Duration) -> Result<Option<Job>, String>;
    /// Extend the lease of a running job so a long build is not reclaimed.
    fn heartbeat(&self, id: &JobId, lease: Duration) -> Result<(), String>;
    fn progress(&self, id: &JobId, p: JobProgress) -> Result<(), String>;
    /// Finish successfully. `session_id` records what the job produced.
    fn complete(&self, id: &JobId, session_id: Option<&str>) -> Result<(), String>;
    /// Record a failed attempt; the return value says retry or dead-letter.
    fn fail(&self, id: &JobId, err: &str) -> Result<Retry, String>;
    /// Mark a running job cancelled (the worker noticed the request).
    fn cancelled(&self, id: &JobId) -> Result<(), String>;
    /// Ask a job to stop. A still-queued job is cancelled outright; a running
    /// one is flagged and the worker notices at its next stage boundary.
    /// Returns whether there was anything to cancel.
    fn request_cancel(&self, id: &JobId) -> Result<bool, String>;
    /// Has cancellation been requested for this job? Cheap; polled by workers.
    fn cancel_requested(&self, id: &JobId) -> bool;
    fn get(&self, id: &JobId) -> Option<Job>;
    fn list(&self, filter: &JobFilter) -> Vec<Job>;
    /// Requeue rows whose lease expired (a worker died mid-job). Called at
    /// startup and on a timer — the recovery guarantee an in-memory map cannot
    /// give. Returns how many rows were reclaimed.
    fn reclaim_expired(&self) -> Result<usize, String>;
}
