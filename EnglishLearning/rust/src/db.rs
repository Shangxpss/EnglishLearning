//! SQLite persistence for processed sessions and playback progress.
//!
//! A processed source is stored once, keyed by its media path plus the media
//! and subtitle modification times. Re-opening the same video on a later run
//! therefore reuses the stored sentence list instead of re-parsing/segmenting,
//! and the per-session `progress` row lets playback resume where it stopped.

use crate::models::{Cue, Session, Word};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// Identifies a processed source for cache lookups.
#[derive(Debug, Clone)]
pub struct SourceKey {
    pub media_path: String,
    pub media_mtime: Option<i64>,
    pub subtitle_path: Option<String>,
    pub subtitle_mtime: Option<i64>,
}

/// Row shape returned by [`Db::list_sessions`].
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub media_path: String,
    pub media_duration: f64,
    pub cue_count: usize,
    /// Last sentence the user played, or `-1` when untouched.
    pub last_index: i64,
    /// Last playback position in seconds.
    pub position: f64,
    pub updated_at: i64,
}

/// Where the user left off inside a session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Progress {
    #[serde(default = "default_index")]
    pub last_index: i64,
    #[serde(default)]
    pub position: f64,
}

fn default_index() -> i64 {
    -1
}

pub struct Db {
    conn: Mutex<Connection>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Db {
    /// Open (creating if needed) the database at `path` and apply the schema.
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create data dir {}: {e}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|e| format!("cannot open database {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS sessions (
                 id             TEXT PRIMARY KEY,
                 media_path     TEXT NOT NULL,
                 media_duration REAL NOT NULL,
                 source_format  TEXT NOT NULL,
                 media_mtime    INTEGER,
                 subtitle_path  TEXT,
                 subtitle_mtime INTEGER,
                 created_at     INTEGER NOT NULL,
                 updated_at     INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS cues (
                 session_id  TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 idx         INTEGER NOT NULL,
                 start       REAL    NOT NULL,
                 end         REAL    NOT NULL,
                 text        TEXT    NOT NULL,
                 words       TEXT,
                 source_path TEXT,
                 PRIMARY KEY (session_id, idx)
             );
             CREATE TABLE IF NOT EXISTS progress (
                 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                 last_index INTEGER NOT NULL DEFAULT -1,
                 position   REAL    NOT NULL DEFAULT 0,
                 updated_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_source
                 ON sessions(media_path, media_mtime, subtitle_path, subtitle_mtime);",
        )
        .map_err(|e| format!("cannot initialize database: {e}"))?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// The most recent stored session for this exact source, if any.
    pub fn find_by_source(&self, key: &SourceKey) -> Option<Session> {
        let id: Option<String> = {
            let conn = self.conn.lock().ok()?;
            conn.query_row(
                "SELECT id FROM sessions
                 WHERE media_path = ?1 AND media_mtime IS ?2
                   AND subtitle_path IS ?3 AND subtitle_mtime IS ?4
                 ORDER BY updated_at DESC LIMIT 1",
                params![
                    key.media_path,
                    key.media_mtime,
                    key.subtitle_path,
                    key.subtitle_mtime
                ],
                |r| r.get(0),
            )
            .optional()
            .ok()?
        };
        self.get_session(&id?)
    }

    pub fn get_session(&self, id: &str) -> Option<Session> {
        let conn = self.conn.lock().ok()?;
        let (id, media_path, media_duration, source_format) = conn
            .query_row(
                "SELECT id, media_path, media_duration, source_format FROM sessions WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        let cues = load_cues(&conn, &id).ok()?;
        Some(Session {
            id,
            media_path,
            media_duration,
            source_format,
            cues,
        })
    }

    /// Persist a freshly built session (idempotent for an existing id).
    pub fn insert_session(&self, session: &Session, key: &SourceKey) -> Result<(), String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let now = now_secs();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO sessions
                 (id, media_path, media_duration, source_format,
                  media_mtime, subtitle_path, subtitle_mtime, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                     COALESCE((SELECT created_at FROM sessions WHERE id = ?1), ?8), ?8)",
            params![
                session.id,
                session.media_path,
                session.media_duration,
                session.source_format,
                key.media_mtime,
                key.subtitle_path,
                key.subtitle_mtime,
                now
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM cues WHERE session_id = ?1",
            params![session.id],
        )
        .map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO cues (session_id, idx, start, end, text, words, source_path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|e| e.to_string())?;
            for cue in &session.cues {
                let words = if cue.words.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&cue.words).map_err(|e| e.to_string())?)
                };
                stmt.execute(params![
                    session.id,
                    cue.index as i64,
                    cue.start,
                    cue.end,
                    cue.text,
                    words,
                    cue.source_path
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// All stored sessions, most recently used first.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let Ok(conn) = self.conn.lock() else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT s.id, s.media_path, s.media_duration,
                    (SELECT COUNT(*) FROM cues c WHERE c.session_id = s.id),
                    COALESCE(p.last_index, -1), COALESCE(p.position, 0.0), s.updated_at
             FROM sessions s LEFT JOIN progress p ON p.session_id = s.id
             ORDER BY s.updated_at DESC",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok(SessionSummary {
                session_id: r.get(0)?,
                media_path: r.get(1)?,
                media_duration: r.get(2)?,
                cue_count: r.get::<_, i64>(3)? as usize,
                last_index: r.get(4)?,
                position: r.get(5)?,
                updated_at: r.get(6)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok).collect()
    }

    pub fn get_progress(&self, session_id: &str) -> Option<Progress> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT last_index, position FROM progress WHERE session_id = ?1",
            params![session_id],
            |r| {
                Ok(Progress {
                    last_index: r.get(0)?,
                    position: r.get(1)?,
                })
            },
        )
        .optional()
        .ok()?
    }

    pub fn set_progress(&self, session_id: &str, p: Progress) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let now = now_secs();
        conn.execute(
            "INSERT INTO progress (session_id, last_index, position, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
                 last_index = excluded.last_index,
                 position   = excluded.position,
                 updated_at = excluded.updated_at",
            params![session_id, p.last_index, p.position, now],
        )
        .map_err(|e| e.to_string())?;
        // Touch the session so "most recently used" ordering reflects activity.
        conn.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![session_id, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn load_cues(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<Cue>> {
    let mut stmt = conn.prepare(
        "SELECT idx, start, end, text, words, source_path
         FROM cues WHERE session_id = ?1 ORDER BY idx",
    )?;
    let rows = stmt.query_map(params![session_id], |r| {
        let words_json: Option<String> = r.get(4)?;
        let words: Vec<Word> = words_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        Ok(Cue {
            index: r.get::<_, i64>(0)? as usize,
            start: r.get(1)?,
            end: r.get(2)?,
            text: r.get(3)?,
            words,
            source_path: r.get(5)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh database in its own directory so parallel tests cannot collide.
    fn tmp_db() -> Db {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-db-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        Db::open(&dir.join("test.db")).expect("open db")
    }

    fn sample(id: &str, mtime: i64) -> (Session, SourceKey) {
        let session = Session {
            id: id.to_string(),
            media_path: "/media/a.mp4".to_string(),
            media_duration: 12.5,
            source_format: "srt".to_string(),
            cues: vec![
                Cue {
                    index: 0,
                    start: 0.5,
                    end: 2.0,
                    text: "Hello.".to_string(),
                    words: vec![Word {
                        text: "Hello".to_string(),
                        start: 0.5,
                        end: 1.0,
                        score: 1.0,
                    }],
                    source_path: Some("/media/a.srt".to_string()),
                },
                Cue {
                    index: 1,
                    start: 2.5,
                    end: 4.0,
                    text: "World.".to_string(),
                    words: Vec::new(),
                    source_path: None,
                },
            ],
        };
        let key = SourceKey {
            media_path: "/media/a.mp4".to_string(),
            media_mtime: Some(mtime),
            subtitle_path: Some("/media/a.srt".to_string()),
            subtitle_mtime: Some(mtime),
        };
        (session, key)
    }

    #[test]
    fn round_trips_session_and_cues() {
        let db = tmp_db();
        let (session, key) = sample("s1", 100);
        db.insert_session(&session, &key).unwrap();

        let found = db.find_by_source(&key).expect("cache hit");
        assert_eq!(found.id, "s1");
        assert_eq!(found.cues.len(), 2);
        assert_eq!(found.cues[0].words.len(), 1);
        assert_eq!(found.cues[0].source_path.as_deref(), Some("/media/a.srt"));

        let loaded = db.get_session("s1").expect("by id");
        assert_eq!(loaded.media_duration, 12.5);
        assert_eq!(loaded.cues[1].text, "World.");
    }

    #[test]
    fn changing_mtime_misses_the_cache() {
        let db = tmp_db();
        let (session, key) = sample("s1", 100);
        db.insert_session(&session, &key).unwrap();

        let mut other = key.clone();
        other.media_mtime = Some(999);
        assert!(db.find_by_source(&other).is_none());
    }

    #[test]
    fn progress_is_upserted_and_listed() {
        let db = tmp_db();
        let (session, key) = sample("s1", 100);
        db.insert_session(&session, &key).unwrap();

        db.set_progress(
            "s1",
            Progress {
                last_index: 1,
                position: 3.25,
            },
        )
        .unwrap();
        let p = db.get_progress("s1").unwrap();
        assert_eq!(p.last_index, 1);
        assert_eq!(p.position, 3.25);

        let list = db.list_sessions();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, "s1");
        assert_eq!(list[0].cue_count, 2);
        assert_eq!(list[0].last_index, 1);
    }
}
