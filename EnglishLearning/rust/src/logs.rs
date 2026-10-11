//! Persistent logging: a `logs.db` SQLite file plus a rotating text mirror.
//!
//! WHY THIS EXISTS
//!   The only evidence of a failure used to be whatever `println!`/`eprintln!`
//!   was still on the terminal that happened to be open. Nothing was persisted,
//!   so a stuck dubbing job, a mux that never produced a file, or a session that
//!   vanished could not be traced after the fact. This module records what the
//!   server did into its own database — and, because the user also wants to
//!   `tail` it outside the app, a plain-text mirror on disk.
//!
//! HOW IT WORKS
//!   [`init`] opens the database, applies the schema, and spawns ONE writer
//!   thread. Callers use the [`debug`]/[`info`]/[`warn`]/[`error`] builders,
//!   which push a record onto a channel and return immediately — logging never
//!   blocks a request or an ASR/alignment loop. The writer drains the channel,
//!   batches inserts into one transaction, appends the text mirror, and prunes
//!   old rows periodically.
//!
//!   The handle lives in a process-global [`OnceLock`]. Logging is genuinely
//!   cross-cutting: it is called from request handling, from background job
//!   threads, and from deep inside the transcription/alignment/synthesis loops,
//!   so threading a handle through every function would be all cost and no
//!   benefit. Reading the log (`query`/`stats`/`clear`) opens a short-lived
//!   connection of its own; with WAL that reader never waits on the writer.

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many rows to keep before pruning the oldest (retention policy).
pub const KEEP_ROWS: i64 = 50_000;
/// How many days of history to keep (retention policy).
pub const KEEP_DAYS: i64 = 30;
/// Rotate the text mirror once it grows past this many bytes.
const ROTATE_BYTES: u64 = 10 * 1024 * 1024;
/// How many rotated mirror files to keep (`sentence-video.log.1` … `.N`).
const ROTATE_KEEP: usize = 3;
/// How often the writer thread prunes, in seconds.
const PRUNE_INTERVAL_SECS: u64 = 120;
/// Largest batch pulled off the channel before a commit.
const BATCH_MAX: usize = 1000;

/// Severity, ordered so `level < min` filters lower-priority records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    pub fn from_str(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// One log row, as returned by [`query`] (the shape the API/UI read).
#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    pub id: i64,
    /// Unix epoch milliseconds.
    pub ts_ms: i64,
    pub level: String,
    pub category: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// A record on its way to the writer thread (no `id` yet — SQLite assigns it).
#[derive(Debug, Clone)]
struct Pending {
    ts_ms: i64,
    level: Level,
    category: String,
    message: String,
    session_id: Option<String>,
    media_path: Option<String>,
    detail: Option<String>,
    duration_ms: Option<i64>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Builder returned by [`debug`] & friends. Chain the optional fields, then
/// [`Entry::send`]. Nothing is written until `send`.
pub struct Entry {
    level: Level,
    category: String,
    message: String,
    session_id: Option<String>,
    media_path: Option<String>,
    detail: Option<String>,
    duration_ms: Option<i64>,
}

impl Entry {
    /// Attach the session this record is about.
    pub fn session(mut self, id: impl std::fmt::Display) -> Self {
        self.session_id = Some(id.to_string());
        self
    }

    /// Attach the media file this record is about.
    pub fn media(mut self, path: impl std::fmt::Display) -> Self {
        self.media_path = Some(path.to_string());
        self
    }

    /// Attach free-form context (an error message, an ffmpeg command, a list).
    pub fn detail(mut self, detail: impl std::fmt::Display) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    /// Attach how long the operation took, in milliseconds.
    pub fn duration_ms(mut self, ms: i64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    /// Hand the record to the writer thread (non-blocking).
    pub fn send(self) {
        emit(Pending {
            ts_ms: now_ms(),
            level: self.level,
            category: self.category,
            message: self.message,
            session_id: self.session_id,
            media_path: self.media_path,
            detail: self.detail,
            duration_ms: self.duration_ms,
        });
    }
}

pub fn log(level: Level, category: &str, message: impl Into<String>) -> Entry {
    Entry {
        level,
        category: category.to_string(),
        message: message.into(),
        session_id: None,
        media_path: None,
        detail: None,
        duration_ms: None,
    }
}

pub fn debug(category: &str, message: impl Into<String>) -> Entry {
    log(Level::Debug, category, message)
}

pub fn info(category: &str, message: impl Into<String>) -> Entry {
    log(Level::Info, category, message)
}

pub fn warn(category: &str, message: impl Into<String>) -> Entry {
    log(Level::Warn, category, message)
}

pub fn error(category: &str, message: impl Into<String>) -> Entry {
    log(Level::Error, category, message)
}

/// The process-global sink. Set once by [`init`].
struct Sink {
    tx: Mutex<Sender<Pending>>,
    db_path: PathBuf,
    /// Text-mirror path, kept so [`clear`] can truncate it alongside the table.
    mirror: Option<PathBuf>,
    min_level: Level,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Push a record onto the channel. Before [`init`] runs (or if the writer died)
/// the record is mirrored to stderr so nothing is silently lost, and a logging
/// call can never panic or block the caller.
fn emit(p: Pending) {
    match SINK.get() {
        Some(sink) => {
            if p.level < sink.min_level {
                return;
            }
            if let Ok(tx) = sink.tx.lock() {
                let _ = tx.send(p);
            }
        }
        None => {
            eprintln!(
                "sentence-video: [{}] {} {}",
                p.level.as_str(),
                p.category,
                p.message
            );
        }
    }
}

/// Open (creating if needed) the log database at `db_path`, apply the schema,
/// and start the writer thread. `mirror` is the optional text-mirror path.
///
/// `min_level` drops records below it at the call site, so `--log-level info`
/// keeps debug chatter out of the channel entirely.
pub fn init(db_path: &Path, mirror: Option<PathBuf>, min_level: Level) -> Result<(), String> {
    if SINK.get().is_some() {
        return Ok(()); // already initialized (e.g. a second call in tests)
    }
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create log dir {}: {e}", parent.display()))?;
    }
    // Fail fast on a broken path rather than losing every record later.
    let conn = open_db(db_path)?;
    drop(conn);

    if let Some(dir) = mirror.as_ref().and_then(|p| p.parent()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create log dir {}: {e}", dir.display()))?;
    }

    let (tx, rx) = mpsc::channel::<Pending>();
    let writer_db = db_path.to_path_buf();
    let writer_mirror = mirror.clone();
    std::thread::Builder::new()
        .name("logs-writer".to_string())
        .spawn(move || writer_loop(writer_db, writer_mirror, rx))
        .map_err(|e| format!("cannot start the log writer: {e}"))?;

    let _ = SINK.set(Sink {
        tx: Mutex::new(tx),
        db_path: db_path.to_path_buf(),
        mirror,
        min_level,
    });
    Ok(())
}

/// Open a connection with the pragmas this database wants. Used by both the
/// writer and the short-lived read/clear connections.
fn open_db(path: &Path) -> Result<Connection, String> {
    let conn =
        Connection::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(5)).ok();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS logs (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             ts_ms       INTEGER NOT NULL,
             level       TEXT    NOT NULL,
             category    TEXT    NOT NULL,
             message     TEXT    NOT NULL,
             session_id  TEXT,
             media_path  TEXT,
             detail      TEXT,
             duration_ms INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_logs_ts       ON logs(ts_ms);
         CREATE INDEX IF NOT EXISTS idx_logs_level    ON logs(level);
         CREATE INDEX IF NOT EXISTS idx_logs_category ON logs(category);
         CREATE INDEX IF NOT EXISTS idx_logs_session  ON logs(session_id);",
    )
    .map_err(|e| format!("cannot initialize the log database: {e}"))?;
    Ok(conn)
}

/// The writer thread: drain the channel, batch-insert, mirror to the file, and
/// prune on a timer.
fn writer_loop(db_path: PathBuf, mirror: Option<PathBuf>, rx: Receiver<Pending>) {
    let conn = match open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sentence-video: log writer cannot start: {e}");
            return;
        }
    };
    let mut conn = conn;
    prune(&conn, KEEP_ROWS, KEEP_DAYS);
    let mut last_prune = Instant::now();
    let mut mirror_file = mirror.as_deref().and_then(open_mirror);

    loop {
        match rx.recv_timeout(Duration::from_secs(PRUNE_INTERVAL_SECS)) {
            Ok(first) => {
                let mut batch = Vec::with_capacity(64);
                batch.push(first);
                while batch.len() < BATCH_MAX {
                    match rx.try_recv() {
                        Ok(more) => batch.push(more),
                        Err(_) => break,
                    }
                }
                if let Err(e) = insert_batch(&mut conn, &batch) {
                    eprintln!("sentence-video: cannot write log rows: {e}");
                }
                if let (Some(file), Some(path)) = (mirror_file.as_mut(), mirror.as_deref()) {
                    write_mirror(file, path, &batch);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_prune.elapsed() >= Duration::from_secs(PRUNE_INTERVAL_SECS) {
            prune(&conn, KEEP_ROWS, KEEP_DAYS);
            last_prune = Instant::now();
        }
    }
}

fn insert_batch(conn: &mut Connection, batch: &[Pending]) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO logs
                     (ts_ms, level, category, message, session_id, media_path, detail, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .map_err(|e| e.to_string())?;
        for r in batch {
            stmt.execute(rusqlite::params![
                r.ts_ms,
                r.level.as_str(),
                r.category,
                r.message,
                r.session_id,
                r.media_path,
                r.detail,
                r.duration_ms,
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Drop rows older than `keep_days`, then everything but the newest `keep_rows`.
fn prune(conn: &Connection, keep_rows: i64, keep_days: i64) {
    let cutoff = now_ms() - keep_days * 24 * 60 * 60 * 1000;
    if let Err(e) = conn.execute("DELETE FROM logs WHERE ts_ms < ?1", [cutoff]) {
        eprintln!("sentence-video: log prune (age) failed: {e}");
    }
    if let Err(e) = conn.execute(
        "DELETE FROM logs WHERE id NOT IN (SELECT id FROM logs ORDER BY id DESC LIMIT ?1)",
        [keep_rows],
    ) {
        eprintln!("sentence-video: log prune (count) failed: {e}");
    }
}

fn open_mirror(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            eprintln!(
                "sentence-video: cannot open log mirror {}: {e}",
                path.display()
            )
        })
        .ok()
}

/// Append `batch` to the mirror, rotating first if the file has grown too large.
fn write_mirror(file: &mut std::fs::File, path: &Path, batch: &[Pending]) {
    use std::io::Write;
    if let Ok(meta) = file.metadata() {
        if meta.len() >= ROTATE_BYTES {
            rotate_mirror(path);
            if let Some(fresh) = open_mirror(path) {
                *file = fresh;
            }
        }
    }
    let mut buf = String::new();
    for r in batch {
        buf.push_str(&format!(
            "{} [{}] {} {}",
            r.ts_ms,
            r.level.as_str(),
            r.category,
            r.message
        ));
        if let Some(s) = &r.session_id {
            buf.push_str(&format!(" · session={s}"));
        }
        if let Some(m) = &r.media_path {
            buf.push_str(&format!(" · media={m}"));
        }
        if let Some(d) = &r.duration_ms {
            buf.push_str(&format!(" · {d}ms"));
        }
        if let Some(det) = &r.detail {
            buf.push_str(&format!(" · {det}"));
        }
        buf.push('\n');
    }
    if let Err(e) = file.write_all(buf.as_bytes()) {
        eprintln!("sentence-video: cannot write the log mirror: {e}");
    }
}

/// Shift `sentence-video.log` → `.1` → `.2` … up to [`ROTATE_KEEP`].
fn rotate_mirror(path: &Path) {
    for n in (1..ROTATE_KEEP).rev() {
        let from = path.with_extension(format!("log.{n}"));
        let to = path.with_extension(format!("log.{}", n + 1));
        let _ = std::fs::rename(from, to);
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

/// Filters for [`query`]. All are optional and combine with AND.
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub level: Option<Level>,
    pub category: Option<String>,
    pub session_id: Option<String>,
    /// Substring match against `media_path`.
    pub media: Option<String>,
    /// Substring match against `message` or `detail`.
    pub q: Option<String>,
    /// Only rows at or after this epoch-ms.
    pub since_ms: Option<i64>,
    /// Only rows with `id` below this (for "load older" pagination).
    pub before_id: Option<i64>,
    /// Newest-first page size, clamped to 1..=5000.
    pub limit: i64,
}

/// Read records, newest first.
pub fn query(q: &Query) -> Result<Vec<LogRecord>, String> {
    let conn = read_conn()?;
    query_conn(&conn, q)
}

fn read_conn() -> Result<Connection, String> {
    let sink = SINK
        .get()
        .ok_or_else(|| "logging is not initialized".to_string())?;
    open_db(&sink.db_path)
}

fn query_conn(conn: &Connection, q: &Query) -> Result<Vec<LogRecord>, String> {
    let mut sql = String::from(
        "SELECT id, ts_ms, level, category, message, session_id, media_path, detail, duration_ms
         FROM logs WHERE 1=1",
    );
    let mut args: Vec<Value> = Vec::new();
    if let Some(l) = q.level {
        sql.push_str(" AND level = ?");
        args.push(Value::Text(l.as_str().to_string()));
    }
    if let Some(c) = &q.category {
        sql.push_str(" AND category = ?");
        args.push(Value::Text(c.clone()));
    }
    if let Some(s) = &q.session_id {
        sql.push_str(" AND session_id = ?");
        args.push(Value::Text(s.clone()));
    }
    if let Some(m) = &q.media {
        sql.push_str(" AND media_path LIKE ?");
        args.push(Value::Text(format!("%{m}%")));
    }
    if let Some(t) = &q.q {
        sql.push_str(" AND (message LIKE ? OR detail LIKE ?)");
        let like = format!("%{t}%");
        args.push(Value::Text(like.clone()));
        args.push(Value::Text(like));
    }
    if let Some(s) = q.since_ms {
        sql.push_str(" AND ts_ms >= ?");
        args.push(Value::Integer(s));
    }
    if let Some(b) = q.before_id {
        sql.push_str(" AND id < ?");
        args.push(Value::Integer(b));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    args.push(Value::Integer(q.limit.clamp(1, 5000)));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(args.iter()), |r| {
            Ok(LogRecord {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                level: r.get(2)?,
                category: r.get(3)?,
                message: r.get(4)?,
                session_id: r.get(5)?,
                media_path: r.get(6)?,
                detail: r.get(7)?,
                duration_ms: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// Aggregate counts for the Logs page header: total, per level, per category,
/// and the oldest/newest timestamp.
pub fn stats() -> Result<serde_json::Value, String> {
    let conn = read_conn()?;
    stats_conn(&conn)
}

fn stats_conn(conn: &Connection) -> Result<serde_json::Value, String> {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let (oldest, newest): (Option<i64>, Option<i64>) = conn
        .query_row("SELECT MIN(ts_ms), MAX(ts_ms) FROM logs", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| e.to_string())?;

    let mut by_level = serde_json::Map::new();
    {
        let mut stmt = conn
            .prepare("SELECT level, COUNT(*) FROM logs GROUP BY level")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            by_level.insert(row.0, serde_json::Value::from(row.1));
        }
    }

    let mut by_category = serde_json::Map::new();
    {
        let mut stmt = conn
            .prepare("SELECT category, COUNT(*) FROM logs GROUP BY category ORDER BY COUNT(*) DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            by_category.insert(row.0, serde_json::Value::from(row.1));
        }
    }

    Ok(serde_json::json!({
        "total": total,
        "oldest_ms": oldest,
        "newest_ms": newest,
        "by_level": by_level,
        "by_category": by_category,
    }))
}

/// Delete every log row and truncate the text mirror; returns how many rows went.
pub fn clear() -> Result<usize, String> {
    let sink = SINK
        .get()
        .ok_or_else(|| "logging is not initialized".to_string())?;
    let conn = open_db(&sink.db_path)?;
    let removed = conn
        .execute("DELETE FROM logs", [])
        .map_err(|e| e.to_string())?;
    // The writer holds its own append handle, and `O_APPEND` always seeks to the
    // end, so truncating to zero here is safe: the next append starts at 0.
    if let Some(path) = sink.mirror.as_deref() {
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
        {
            eprintln!("sentence-video: cannot truncate the log mirror: {e}");
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp_path() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-logs-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("logs.db")
    }

    fn pending(level: Level, category: &str, message: &str) -> Pending {
        Pending {
            ts_ms: now_ms(),
            level,
            category: category.to_string(),
            message: message.to_string(),
            session_id: None,
            media_path: None,
            detail: None,
            duration_ms: None,
        }
    }

    #[test]
    fn inserts_filters_and_counts() {
        let path = tmp_path();
        let mut conn = open_db(&path).unwrap();
        let mut a = pending(Level::Info, "http", "GET /api/sessions → 200");
        a.session_id = Some("s1".into());
        let b = pending(Level::Error, "dub", "edge-tts stalled");
        insert_batch(&mut conn, &[a, b]).unwrap();

        assert_eq!(
            query_conn(
                &conn,
                &Query {
                    limit: 10,
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            2
        );

        let errors = query_conn(
            &conn,
            &Query {
                level: Some(Level::Error),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].category, "dub");

        let by_session = query_conn(
            &conn,
            &Query {
                session_id: Some("s1".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_session.len(), 1);

        let search = query_conn(
            &conn,
            &Query {
                q: Some("stalled".into()),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(search.len(), 1);

        let stats = stats_conn(&conn).unwrap();
        assert_eq!(stats["total"], 2);
        assert_eq!(stats["by_level"]["error"], 1);
    }

    #[test]
    fn prunes_by_count_and_age() {
        let path = tmp_path();
        let mut conn = open_db(&path).unwrap();
        let mut old = pending(Level::Info, "http", "ancient");
        old.ts_ms = now_ms() - 40 * 24 * 60 * 60 * 1000; // 40 days ago
        insert_batch(&mut conn, &[old]).unwrap();
        for i in 0..5 {
            insert_batch(
                &mut conn,
                &[pending(Level::Info, "http", &format!("row {i}"))],
            )
            .unwrap();
        }

        // Age prune drops the 40-day-old row; count prune keeps the newest 2.
        prune(&conn, 2, KEEP_DAYS);
        let rows = query_conn(
            &conn,
            &Query {
                limit: 100,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.message.starts_with("row")));
    }
}
