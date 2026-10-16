//! The worker pool: fixed OS threads that claim jobs and run them.
//!
//! These are plain `std::thread`s on purpose, not tokio tasks: every expensive
//! step (FFmpeg decode, whisper.cpp, the wav2vec2 aligner) is blocking, CPU-bound
//! work with no await points at all, and running it on a tokio worker thread
//! would block the executor. A fixed pool *is* the scheduler here.
//!
//! Concurrency defaults to **1** and is configurable, because alignment alone
//! takes minutes per clip and parallelism multiplies memory (whisper model +
//! weights) while making the machine unusable. The queue's job is to **bound**
//! concurrency, not to inflate it.

use super::{Job, JobEvent, JobId, JobProgress, JobQueue, Notifier, Retry};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often an idle worker looks for work. A submitted job therefore starts
/// within this window; small enough to feel instant, cheap enough to not spin.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// How often a running job is heartbeated and its stage mirrored. Frequent
/// enough that the progress bar moves, rare enough to not contend on the DB.
const MONITOR_POLL: Duration = Duration::from_millis(500);

/// A snapshot of where a running build is, for the job row's `stage/done/total`.
#[derive(Clone, Debug, Serialize)]
pub struct JobStage {
    pub stage: String,
    pub done: i64,
    pub total: i64,
}

/// Why a job did not finish.
#[derive(Clone, Debug)]
pub enum JobFailure {
    /// The user asked for it to stop — a clean terminal state, not an error.
    Cancelled,
    /// A real failure; the queue decides retry vs dead-letter.
    Failed(String),
}

/// The blocking work a job performs.
///
/// Implemented by the app (`impl JobRunner for AppState`), so this module never
/// depends on the app and the worker stays swappable/testable.
pub trait JobRunner: Send + Sync {
    /// Run the job to completion; `Ok(Some(session_id))` when it produced one.
    fn run(&self, job: &Job) -> Result<Option<String>, JobFailure>;
    /// Best-effort: ask the in-flight work for this media to stop. The build
    /// notices at its next stage boundary.
    fn request_cancel(&self, media_path: &str);
    /// Where the media's build currently is, if one is registered.
    fn stage(&self, media_path: &str) -> Option<JobStage>;
}

pub struct WorkerPool {
    stop: Arc<AtomicBool>,
}

impl WorkerPool {
    /// Start `count` worker threads (clamped to at least one). The threads are
    /// detached; dropping the pool only asks them to leave once their current
    /// job finishes.
    pub fn start(
        count: usize,
        queue: Arc<dyn JobQueue>,
        runner: Arc<dyn JobRunner>,
        notify: Arc<dyn Notifier>,
        lease: Duration,
    ) -> WorkerPool {
        let stop = Arc::new(AtomicBool::new(false));
        for index in 0..count.max(1) {
            let (queue, runner, notify, stop) = (
                Arc::clone(&queue),
                Arc::clone(&runner),
                Arc::clone(&notify),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || work_loop(index, queue, runner, notify, lease, stop));
        }
        WorkerPool { stop }
    }
}

/// Ask every worker to stop after its current job. The threads are detached, so
/// this is the only shutdown signal they ever see; the process exiting does the
/// rest.
impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn work_loop(
    index: usize,
    queue: Arc<dyn JobQueue>,
    runner: Arc<dyn JobRunner>,
    notify: Arc<dyn Notifier>,
    lease: Duration,
    stop: Arc<AtomicBool>,
) {
    crate::logs::info("queue", "worker started")
        .detail(format!("worker #{index}"))
        .send();
    while !stop.load(Ordering::Relaxed) {
        let job = match queue.claim(lease) {
            Ok(Some(job)) => job,
            Ok(None) => {
                std::thread::sleep(IDLE_POLL);
                continue;
            }
            Err(e) => {
                crate::logs::warn("queue", "could not claim a job")
                    .detail(e)
                    .send();
                std::thread::sleep(IDLE_POLL);
                continue;
            }
        };
        execute(
            &job,
            Arc::clone(&queue),
            Arc::clone(&runner),
            Arc::clone(&notify),
            lease,
        );
    }
}

/// Run one claimed job: announce it, monitor it (heartbeat + live progress +
/// cancel bridge), then record its terminal state and notify.
fn execute(
    job: &Job,
    queue: Arc<dyn JobQueue>,
    runner: Arc<dyn JobRunner>,
    notify: Arc<dyn Notifier>,
    lease: Duration,
) {
    publish(&*notify, &*queue, &job.id, None);
    crate::logs::info("queue", "job started")
        .media(&job.media_path)
        .detail(format!(
            "kind={} attempt={}/{}",
            job.kind, job.attempts, job.max_attempts
        ))
        .send();
    let started = std::time::Instant::now();

    // The monitor is what makes a running job observable *and* cancellable while
    // the worker thread is blocked inside the pipeline: it extends the lease so
    // the job is never wrongly reclaimed, mirrors the build's live stage into the
    // row, and flips the pipeline's cancel flag the moment the row asks for it.
    let done_monitor = Arc::new(AtomicBool::new(false));
    let monitor = {
        let (flag, id, media, task, work) = (
            Arc::clone(&done_monitor),
            job.id.clone(),
            job.media_path.clone(),
            Arc::clone(&queue),
            Arc::clone(&runner),
        );
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if task.cancel_requested(&id) {
                    work.request_cancel(&media);
                }
                if let Some(stage) = work.stage(&media) {
                    let _ = task.progress(
                        &id,
                        JobProgress {
                            stage: Some(&stage.stage),
                            done: stage.done,
                            total: stage.total,
                        },
                    );
                }
                let _ = task.heartbeat(&id, lease);
                std::thread::sleep(MONITOR_POLL);
            }
        })
    };

    let outcome = runner.run(job);
    done_monitor.store(true, Ordering::Relaxed);
    let _ = monitor.join();

    match outcome {
        Ok(session_id) => {
            let _ = queue.complete(&job.id, session_id.as_deref());
            publish(&*notify, &*queue, &job.id, None);
            crate::logs::info("queue", "job finished")
                .media(&job.media_path)
                .detail(format!(
                    "kind={} · session={}",
                    job.kind,
                    session_id.as_deref().unwrap_or("-")
                ))
                .duration_ms(started.elapsed().as_millis() as i64)
                .send();
        }
        Err(JobFailure::Cancelled) => {
            let _ = queue.cancelled(&job.id);
            publish(&*notify, &*queue, &job.id, None);
            crate::logs::warn("queue", "job cancelled")
                .media(&job.media_path)
                .detail(format!("kind={}", job.kind))
                .send();
        }
        Err(JobFailure::Failed(err)) => match queue.fail(&job.id, &err) {
            Ok(Retry::Requeued { delay_secs }) => {
                publish(&*notify, &*queue, &job.id, Some(err.clone()));
                crate::logs::warn("queue", "job failed; will retry")
                    .media(&job.media_path)
                    .detail(format!(
                        "{err} · retry in {delay_secs}s (attempt {}/{})",
                        job.attempts, job.max_attempts
                    ))
                    .send();
            }
            _ => {
                publish(&*notify, &*queue, &job.id, Some(err.clone()));
                crate::logs::error("queue", "job failed permanently")
                    .media(&job.media_path)
                    .detail(format!(
                        "{err} · attempt {}/{}",
                        job.attempts, job.max_attempts
                    ))
                    .send();
            }
        },
    }
}

/// Publish the row's current state (the queue is the source of truth, so the
/// event always matches what `GET /api/jobs` would return).
fn publish(notify: &dyn Notifier, queue: &dyn JobQueue, id: &JobId, error: Option<String>) {
    if let Some(job) = queue.get(id) {
        let _ = notify.publish(&JobEvent::from_job(&job, error));
    }
}
