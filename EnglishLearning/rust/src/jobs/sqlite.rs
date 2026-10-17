//! The embedded [`JobQueue`]: a `jobs` table in the same bundled SQLite the app
//! already ships, driven by the same `rusqlite` already linked in.
//!
//! Why a table and not a broker: the queue is transactional with the session
//! rows, lives in the same file, in the same process, with zero new runtime
//! dependencies and zero new processes — and it survives a restart. It does
//! **not** survive multi-node, which is exactly why [`JobQueue`] is a port.
//!
//! Two facts worth stating, because they are the usual objections:
//!
//! * SQLite has no `FOR UPDATE SKIP LOCKED`. That is fine **here and only
//!   here**: this process is the single writer, so `BEGIN IMMEDIATE` (the
//!   global write lock, taken up front) serialises claims correctly.
//! * `lease_expires_at` is the visibility timeout. A worker that dies leaves a
//!   `running` row whose lease lapses; [`SqliteQueue::reclaim_expired`] puts it
//!   back on the queue. That is the crash-recovery guarantee an in-memory map
//!   can never provide.

use super::{
    now_secs, Job, JobFilter, JobId, JobProgress, JobQueue, JobSpec, Retry, STATE_CANCELLED,
    STATE_DONE, STATE_FAILED, STATE_QUEUED, STATE_RUNNING,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// Attempts get `2^attempts × 5 s`, capped at five minutes — enough to ride out
/// a transient ffmpeg/whisper failure without hammering a broken file.
fn backoff_secs(attempts: i64) -> i64 {
    let shift = attempts.clamp(0, 6) as u32;
    (5i64 << shift).min(300)
}

pub struct SqliteQueue {
    conn: Mutex<Connection>,
}

impl SqliteQueue {
    /// Open (creating if needed) the queue at `path` and apply the schema.
    ///
    /// The same file as the session store is the intended setup: one database,
    /// one `BEGIN IMMEDIATE` possible across a job transition and a session
    /// insert, no dual-write bug. WAL is on so a claim write does not block the
    /// read paths, and `busy_timeout` keeps the two connections from racing into
    /// `SQLITE_BUSY`.
    pub fn open(path: &Path) -> Result<SqliteQueue, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create data dir {}: {e}", parent.display()))?;
            }
        }
        let conn =
            Connection::open(path).map_err(|e| format!("cannot open job queue {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS jobs (
                 id                TEXT PRIMARY KEY,
                 media_path        TEXT NOT NULL,
                 cache_key         TEXT NOT NULL,
                 kind              TEXT NOT NULL,
                 state             TEXT NOT NULL,
                 stage             TEXT,
                 done              INTEGER NOT NULL DEFAULT 0,
                 total             INTEGER NOT NULL DEFAULT 0,
                 attempts          INTEGER NOT NULL DEFAULT 0,
                 max_attempts      INTEGER NOT NULL DEFAULT 3,
                 lease_expires_at  INTEGER,
                 cancel_requested  INTEGER NOT NULL DEFAULT 0,
                 payload           TEXT,
                 last_error        TEXT,
                 session_id        TEXT,
                 enqueued_at       INTEGER NOT NULL,
                 started_at        INTEGER,
                 finished_at       INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_jobs_claimable ON jobs(state, enqueued_at);
             -- One ACTIVE job per (media, cache identity, kind): re-submitting a
             -- file that is already queued or running joins that job instead of
             -- duplicating the work. Terminal rows are exempt, so a file can be
             -- queued again after it finished.
             CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_active
                 ON jobs(media_path, cache_key, kind) WHERE state IN ('queued', 'running');",
        )
        .map_err(|e| format!("cannot initialize the job queue: {e}"))?;
        Ok(SqliteQueue {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "job queue lock poisoned".to_string())
    }
}

/// `jobs` columns in the order [`load_job`] reads them.
const COLUMNS: &str = "id, media_path, cache_key, kind, state, stage, done, total, \
                       attempts, max_attempts, payload, last_error, session_id, \
                       enqueued_at, started_at, finished_at";

fn load_job(conn: &Connection, id: &str) -> Result<Option<Job>, rusqlite::Error> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
        params![id],
        row_to_job,
    )
    .optional()
}

fn row_to_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        media_path: r.get(1)?,
        cache_key: r.get(2)?,
        kind: r.get(3)?,
        state: r.get(4)?,
        stage: r.get(5)?,
        done: r.get(6)?,
        total: r.get(7)?,
        attempts: r.get(8)?,
        max_attempts: r.get(9)?,
        payload: r.get(10)?,
        last_error: r.get(11)?,
        session_id: r.get(12)?,
        enqueued_at: r.get(13)?,
        started_at: r.get(14)?,
        finished_at: r.get(15)?,
    })
}

/// A collision-resistant handle. Shaped like [`crate::app`]'s session ids
/// (nanos rotated, mixed with a counter) so it stays sortable and unique inside
/// one process.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{:016x}",
        (nanos as u64 ^ ((c as u64) << 40)) & 0xFFFFFFFFFFFFFFu64
    )
}

impl JobQueue for SqliteQueue {
    fn enqueue(&self, spec: JobSpec) -> Result<JobId, String> {
        let conn = self.lock()?;
        let id = new_id();
        let now = now_secs();
        let inserted = conn.execute(
            "INSERT INTO jobs
                 (id, media_path, cache_key, kind, state, payload, max_attempts, enqueued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                spec.media_path,
                spec.cache_key,
                spec.kind,
                STATE_QUEUED,
                spec.payload,
                spec.max_attempts,
                now
            ],
        );
        match inserted {
            Ok(_) => Ok(id),
            // The partial unique index fired: an equivalent job is already
            // active. Hand back its id so the caller sees one job, not two.
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                conn.query_row(
                    "SELECT id FROM jobs
                      WHERE media_path = ?1 AND cache_key = ?2 AND kind = ?3
                        AND state IN ('queued', 'running')
                      ORDER BY enqueued_at LIMIT 1",
                    params![spec.media_path, spec.cache_key, spec.kind],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn claim(&self, lease: Duration) -> Result<Option<Job>, String> {
        let mut conn = self.lock()?;
        let now = now_secs();
        let lease_until = now + lease.as_secs() as i64;
        // `IMMEDIATE` takes the write lock up front, so the select-then-update
        // below is atomic even though SQLite has no `SKIP LOCKED`.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let id: Option<String> = tx
            .query_row(
                // `enqueued_at <= now` is the backoff gate: a requeued job is
                // parked in the future and stays invisible until its delay lapses.
                "SELECT id FROM jobs WHERE state = ?1 AND enqueued_at <= ?2
                 ORDER BY enqueued_at LIMIT 1",
                params![STATE_QUEUED, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(id) = id else {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE jobs SET state = ?2, stage = 'starting', attempts = attempts + 1,
                    started_at = ?3, lease_expires_at = ?4, finished_at = NULL
              WHERE id = ?1 AND state = ?5",
            params![id, STATE_RUNNING, now, lease_until, STATE_QUEUED],
        )
        .map_err(|e| e.to_string())?;
        let job = load_job(&tx, &id).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(job)
    }

    fn heartbeat(&self, id: &JobId, lease: Duration) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE jobs SET lease_expires_at = ?2 WHERE id = ?1 AND state = ?3",
            params![id, now_secs() + lease.as_secs() as i64, STATE_RUNNING],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn progress(&self, id: &JobId, p: JobProgress<'_>) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE jobs SET stage = ?2, done = ?3, total = ?4 WHERE id = ?1 AND state = ?5",
            params![id, p.stage, p.done, p.total, STATE_RUNNING],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn complete(&self, id: &JobId, session_id: Option<&str>) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE jobs SET state = ?2, session_id = ?3, stage = NULL,
                    finished_at = ?4, lease_expires_at = NULL
              WHERE id = ?1",
            params![id, STATE_DONE, session_id, now_secs()],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn fail(&self, id: &JobId, err: &str) -> Result<Retry, String> {
        let mut conn = self.lock()?;
        let now = now_secs();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT attempts, max_attempts FROM jobs WHERE id = ?1 AND state = ?2",
                params![id, STATE_RUNNING],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((attempts, max_attempts)) = row else {
            // Not running: someone cancelled or finished it concurrently.
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(Retry::DeadLetter);
        };
        let outcome = if attempts >= max_attempts {
            tx.execute(
                "UPDATE jobs SET state = ?2, last_error = ?3, stage = NULL,
                        finished_at = ?4, lease_expires_at = NULL
                  WHERE id = ?1",
                params![id, STATE_FAILED, err, now],
            )
            .map_err(|e| e.to_string())?;
            Retry::DeadLetter
        } else {
            let delay = backoff_secs(attempts);
            tx.execute(
                "UPDATE jobs SET state = ?2, last_error = ?3, stage = NULL,
                        started_at = NULL, lease_expires_at = NULL, enqueued_at = ?4
                  WHERE id = ?1",
                params![id, STATE_QUEUED, err, now + delay],
            )
            .map_err(|e| e.to_string())?;
            Retry::Requeued { delay_secs: delay }
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(outcome)
    }

    fn cancelled(&self, id: &JobId) -> Result<(), String> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE jobs SET state = ?2, stage = NULL, finished_at = ?3, lease_expires_at = NULL
              WHERE id = ?1",
            params![id, STATE_CANCELLED, now_secs()],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn request_cancel(&self, id: &JobId) -> Result<bool, String> {
        let mut conn = self.lock()?;
        let now = now_secs();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let state: Option<String> = tx
            .query_row("SELECT state FROM jobs WHERE id = ?1", params![id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        let touched = match state.as_deref() {
            Some(STATE_QUEUED) => {
                // Nothing is running yet: cancel it outright, no worker involved.
                tx.execute(
                    "UPDATE jobs SET state = ?2, cancel_requested = 1, finished_at = ?3
                      WHERE id = ?1",
                    params![id, STATE_CANCELLED, now],
                )
                .map_err(|e| e.to_string())?;
                true
            }
            Some(STATE_RUNNING) => {
                // Only raise the flag; the worker stops at its next stage boundary.
                tx.execute(
                    "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
                true
            }
            _ => false,
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(touched)
    }

    fn cancel_requested(&self, id: &JobId) -> bool {
        let Ok(conn) = self.lock() else { return false };
        conn.query_row(
            "SELECT cancel_requested FROM jobs WHERE id = ?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0)
            != 0
    }

    fn get(&self, id: &JobId) -> Option<Job> {
        let conn = self.lock().ok()?;
        load_job(&conn, id).ok().flatten()
    }

    fn list(&self, filter: &JobFilter) -> Vec<Job> {
        let Ok(conn) = self.lock() else { return Vec::new() };
        let sql = format!(
            "SELECT {COLUMNS} FROM jobs
              WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR kind = ?2)
              ORDER BY enqueued_at DESC LIMIT ?3"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return Vec::new();
        };
        let limit = filter.limit.unwrap_or(200) as i64;
        let Ok(rows) = stmt.query_map(params![filter.state, filter.kind, limit], row_to_job) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    fn reclaim_expired(&self) -> Result<usize, String> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE jobs SET state = ?2, stage = NULL, started_at = NULL, lease_expires_at = NULL
              WHERE state = ?3 AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?1",
            params![now_secs(), STATE_QUEUED, STATE_RUNNING],
        )
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_queue() -> SqliteQueue {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-jobs-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        SqliteQueue::open(&dir.join("jobs.db")).expect("open queue")
    }

    fn spec(media: &str) -> JobSpec {
        JobSpec {
            media_path: media.to_string(),
            cache_key: format!("{media}|1|2"),
            kind: "build".to_string(),
            max_attempts: 3,
            payload: None,
        }
    }

    #[test]
    fn claims_oldest_first_and_marks_running() {
        let q = tmp_queue();
        let first = q.enqueue(spec("/a.mp4")).unwrap();
        let second = q.enqueue(spec("/b.mp4")).unwrap();

        let claimed = q.claim(Duration::from_secs(30)).unwrap().expect("a job");
        assert_eq!(claimed.id, first);
        assert_eq!(claimed.state, STATE_RUNNING);
        assert_eq!(claimed.attempts, 1);
        assert_eq!(q.claim(Duration::from_secs(30)).unwrap().unwrap().id, second);
        assert!(q.claim(Duration::from_secs(30)).unwrap().is_none());
    }

    #[test]
    fn resubmitting_an_active_job_returns_the_same_handle() {
        let q = tmp_queue();
        let a = q.enqueue(spec("/a.mp4")).unwrap();
        let b = q.enqueue(spec("/a.mp4")).unwrap();
        assert_eq!(a, b, "an active job must not be duplicated");
        // A different file is a different job.
        assert_ne!(a, q.enqueue(spec("/b.mp4")).unwrap());
    }

    #[test]
    fn done_and_cancelled_states_are_terminal_and_free_the_key() {
        let q = tmp_queue();
        let a = q.enqueue(spec("/a.mp4")).unwrap();
        q.claim(Duration::from_secs(30)).unwrap();
        q.complete(&a, Some("s1")).unwrap();
        let done = q.get(&a).unwrap();
        assert_eq!(done.state, STATE_DONE);
        assert_eq!(done.session_id.as_deref(), Some("s1"));
        assert!(done.finished_at.is_some());
        // The active-key slot is free again, so the file can be re-queued.
        assert_ne!(a, q.enqueue(spec("/a.mp4")).unwrap());

        let b = q.enqueue(spec("/b.mp4")).unwrap();
        assert!(q.request_cancel(&b).unwrap());
        assert_eq!(q.get(&b).unwrap().state, STATE_CANCELLED);
        assert!(!q.request_cancel(&b).unwrap(), "terminal job: nothing to cancel");
    }

    #[test]
    fn a_queued_cancel_is_immediate_a_running_one_is_flagged() {
        let q = tmp_queue();
        let queued = q.enqueue(spec("/a.mp4")).unwrap();
        assert!(q.request_cancel(&queued).unwrap());
        assert_eq!(q.get(&queued).unwrap().state, STATE_CANCELLED);

        let running = q.enqueue(spec("/b.mp4")).unwrap();
        q.claim(Duration::from_secs(30)).unwrap();
        assert!(q.request_cancel(&running).unwrap());
        assert_eq!(q.get(&running).unwrap().state, STATE_RUNNING);
        assert!(q.cancel_requested(&running));
    }

    #[test]
    fn failure_backs_off_then_dead_letters() {
        let q = tmp_queue();
        let id = q.enqueue(spec("/a.mp4")).unwrap();
        q.claim(Duration::from_secs(30)).unwrap();
        // attempt 1 fails -> requeued with backoff, invisible until the delay.
        assert!(matches!(q.fail(&id, "boom").unwrap(), Retry::Requeued { .. }));
        assert_eq!(q.get(&id).unwrap().state, STATE_QUEUED);
        assert!(q.claim(Duration::from_secs(30)).unwrap().is_none(), "backing off");
        // Exhaust attempts by claiming/failing until the cap.
        for _ in 0..2 {
            let job = q.claim(Duration::from_secs(30)).unwrap();
            if job.is_none() {
                // The delay is in the future; move it back to now by hand.
                let conn = q.lock().unwrap();
                conn.execute("UPDATE jobs SET enqueued_at = 0 WHERE id = ?1", params![id])
                    .unwrap();
            }
            let _ = q.claim(Duration::from_secs(30)).unwrap();
            let outcome = q.fail(&id, "boom").unwrap();
            if let Retry::Requeued { .. } = outcome {
                let conn = q.lock().unwrap();
                conn.execute("UPDATE jobs SET enqueued_at = 0 WHERE id = ?1", params![id])
                    .unwrap();
            } else {
                assert_eq!(outcome, Retry::DeadLetter);
                assert_eq!(q.get(&id).unwrap().state, STATE_FAILED);
                assert_eq!(q.get(&id).unwrap().last_error.as_deref(), Some("boom"));
                return;
            }
        }
        panic!("job should have dead-lettered before attempts ran out");
    }

    #[test]
    fn an_expired_lease_is_reclaimed() {
        let q = tmp_queue();
        let id = q.enqueue(spec("/a.mp4")).unwrap();
        q.claim(Duration::from_secs(0)).unwrap();
        assert_eq!(q.get(&id).unwrap().state, STATE_RUNNING);
        assert_eq!(q.reclaim_expired().unwrap(), 1);
        assert_eq!(q.get(&id).unwrap().state, STATE_QUEUED);
    }

    #[test]
    fn progress_and_heartbeat_touch_only_running_jobs() {
        let q = tmp_queue();
        let id = q.enqueue(spec("/a.mp4")).unwrap();
        // Not running yet: a stray progress write must not resurrect it.
        q.progress(&id, JobProgress { stage: Some("decode"), done: 3, total: 9 })
            .unwrap();
        assert_eq!(q.get(&id).unwrap().stage, None);
        q.claim(Duration::from_secs(30)).unwrap();
        q.progress(&id, JobProgress { stage: Some("align"), done: 3, total: 9 })
            .unwrap();
        let job = q.get(&id).unwrap();
        assert_eq!(job.stage.as_deref(), Some("align"));
        assert_eq!((job.done, job.total), (3, 9));
        q.heartbeat(&id, Duration::from_secs(120)).unwrap();
    }
}
