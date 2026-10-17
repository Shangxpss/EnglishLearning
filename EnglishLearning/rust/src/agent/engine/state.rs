//! Conversation state — a durable per-thread transcript log, and the **source
//! of record** for what the model remembers.
//!
//! AG-UI is stateless per run: the client re-sends the whole conversation with
//! every request. That payload is a *claim* about the thread, not the truth. A
//! client may forward only its last message (trimming history it no longer needs
//! to carry — see CopilotKit's message-history guide), and a server that
//! replaced its copy with each payload would erase everything earlier. So the
//! direction is inverted here: the model context is built from what is stored,
//! and the forwarded conversation is used only to locate the turn the store has
//! not recorded yet.
//!
//! Two write shapes, both driven by [`crate::agent::engine::run_turn`]:
//!
//! * [`ThreadStore::merge`] appends the *unrecorded* part of the forwarded
//!   conversation and returns the whole transcript — the memory handed to the
//!   model. A client that forwards the full history does not duplicate it, and a
//!   client that forwards a single message cannot erase what came before.
//! * [`ThreadStore::append`] adds messages to the end — how the assistant's own
//!   reply (which the client has not echoed back yet) is captured mid-run.
//!
//! Reference: CopilotKit's runtime keeps a per-thread snapshot of the agent's
//! messages and replays it to a client joining a thread
//! (`@copilotkit/runtime/dist/v2/runtime/runner/in-memory.mjs` — `appendRun`
//! replaces `store.messagesSnapshot`, `connect` replays, `getThreadMessages`
//! reads it back). Same idea, one store per thread instead of one per process.
//!
//! Storage is SQLite through the crate's `bundled` feature, so nothing extra is
//! installed on the end user's machine. [`ThreadStore::new`] uses an in-memory
//! database (what the tests get); the running service opens a file next to
//! `sessions.db` via [`ThreadStore::open`].

use crate::agent::llm::{ChatMessage, Role};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Threads and the messages recorded against them.
pub struct ThreadStore {
    conn: Mutex<Connection>,
}

/// A thread as reported by `GET /copilotkit/threads`.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub messages: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Default for ThreadStore {
    fn default() -> Self {
        ThreadStore::new()
    }
}

impl ThreadStore {
    /// A store backed by an in-memory database.
    pub fn new() -> Self {
        Self::from_connection(Connection::open_in_memory().expect("in-memory SQLite"))
    }

    /// A store backed by `path`, creating the parent directory if needed.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create thread dir {}: {e}", parent.display()))?;
        }
        let conn =
            Connection::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5)).ok();
        Ok(Self::from_connection(conn))
    }

    fn from_connection(conn: Connection) -> Self {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS threads (
                 id         TEXT PRIMARY KEY,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                 thread_id TEXT    NOT NULL,
                 seq       INTEGER NOT NULL,
                 role      TEXT    NOT NULL,
                 content   TEXT    NOT NULL,
                 PRIMARY KEY (thread_id, seq)
             );",
        )
        .expect("initialize the thread database");
        ThreadStore {
            conn: Mutex::new(conn),
        }
    }

    /// Record the part of `forwarded` this thread does not hold yet, and return
    /// the whole transcript — the history the model should be given.
    ///
    /// The read and the write share one lock, so two turns cannot interleave. On
    /// a storage failure the stored transcript is returned unchanged: losing a
    /// new turn matters less than failing the user's chat over.
    pub fn merge(&self, thread_id: &str, forwarded: &[ChatMessage]) -> Vec<ChatMessage> {
        let now = now_ms();
        let mut conn = self.lock();
        let mut history = read_history(&conn, thread_id);

        let pending = pending_turn(&history, forwarded);
        if pending.is_empty() {
            return history;
        }

        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                report("begin transcript", e);
                return history;
            }
        };
        let result = (|| -> rusqlite::Result<()> {
            touch(&tx, thread_id, now)?;
            let start: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM messages WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )?;
            for (offset, message) in pending.iter().enumerate() {
                insert_message(&tx, thread_id, start + offset as i64, message)?;
            }
            Ok(())
        })();
        match result.and_then(|()| tx.commit()) {
            Ok(()) => history.extend(pending),
            Err(e) => report("merge transcript", e),
        }
        history
    }

    /// Add `messages` to the end of `thread_id`, creating the thread if needed.
    pub fn append(&self, thread_id: &str, messages: &[ChatMessage]) {
        if messages.is_empty() {
            return;
        }
        let now = now_ms();
        let mut conn = self.lock();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => return report("begin transcript", e),
        };
        let result = (|| -> rusqlite::Result<()> {
            touch(&tx, thread_id, now)?;
            let start: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq) + 1, 0) FROM messages WHERE thread_id = ?1",
                params![thread_id],
                |row| row.get(0),
            )?;
            for (offset, message) in messages.iter().enumerate() {
                insert_message(&tx, thread_id, start + offset as i64, message)?;
            }
            Ok(())
        })();
        match result.and_then(|()| tx.commit()) {
            Ok(()) => {}
            Err(e) => report("append transcript", e),
        }
    }

    /// A copy of the recorded transcript, oldest first.
    pub fn history(&self, thread_id: &str) -> Vec<ChatMessage> {
        read_history(&self.lock(), thread_id)
    }

    /// Forget a thread. Returns whether it existed.
    pub fn clear(&self, thread_id: &str) -> bool {
        let mut conn = self.lock();
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                report("begin delete", e);
                return false;
            }
        };
        let existed = tx
            .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
            .unwrap_or(0)
            > 0;
        if let Err(e) = tx.execute(
            "DELETE FROM messages WHERE thread_id = ?1",
            params![thread_id],
        ) {
            report("delete transcript", e);
        }
        if let Err(e) = tx.commit() {
            report("delete thread", e);
        }
        existed
    }

    /// Every known thread, sorted by id for stable output.
    pub fn summaries(&self) -> Vec<ThreadSummary> {
        let conn = self.lock();
        let mut stmt = match conn.prepare(
            "SELECT t.id, t.created_at, t.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.thread_id = t.id)
             FROM threads t ORDER BY t.id",
        ) {
            Ok(stmt) => stmt,
            Err(e) => {
                report("list threads", e);
                return Vec::new();
            }
        };
        let rows = stmt.query_map([], |row| {
            Ok(ThreadSummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                updated_at: row.get(2)?,
                messages: row.get::<_, i64>(3)? as usize,
            })
        });
        rows.map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Number of known threads.
    pub fn len(&self) -> usize {
        let conn = self.lock();
        conn.query_row("SELECT COUNT(*) FROM threads", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|n| n as usize)
        .unwrap_or(0)
    }

    /// Whether no thread has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read the connection, recovering from a poisoned lock instead of
    /// panicking: a panic in one request must not take the whole service down.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Insert (or create) the thread row and bump its `updated_at`.
fn touch(conn: &Connection, thread_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO threads (id, created_at, updated_at) VALUES (?1, ?2, ?2)
         ON CONFLICT(id) DO UPDATE SET updated_at = ?2",
        params![thread_id, now],
    )?;
    Ok(())
}

/// Read a thread's stored transcript, oldest first.
fn read_history(conn: &Connection, thread_id: &str) -> Vec<ChatMessage> {
    let mut stmt = match conn
        .prepare("SELECT role, content FROM messages WHERE thread_id = ?1 ORDER BY seq")
    {
        Ok(stmt) => stmt,
        Err(e) => {
            report("read transcript", e);
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![thread_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let mut out = Vec::new();
    if let Ok(rows) = rows {
        for (role, content) in rows.flatten() {
            if let Some(message) = chat_from_row(&role, &content) {
                out.push(message);
            }
        }
    }
    out
}

/// The part of `forwarded` this thread has not recorded yet.
///
/// The client may send the whole conversation or only its last message, so the
/// new turn is *located* rather than assumed:
///
/// * messages the two copies agree on from the front are already recorded —
///   `agreed` is how far they match;
/// * the turn starts at the first user message from there on, so an assistant
///   reply the client echoes back (whose text can differ from ours by a
///   whitespace and would otherwise be recorded twice) is skipped;
/// * a candidate the store already ends with is dropped, which is what makes a
///   re-sent turn and a client that repeats its last message idempotent.
///
/// Known limit: a client that forwards two unheard turns at once records only
/// the trailing one. One run carries one new turn, so this cannot happen in
/// normal use — it would take a silently lost run.
fn pending_turn(stored: &[ChatMessage], forwarded: &[ChatMessage]) -> Vec<ChatMessage> {
    if forwarded.is_empty() {
        return Vec::new();
    }
    // Nothing recorded yet: the forwarded conversation is all there is, so adopt
    // it whole rather than guessing where its turn starts.
    if stored.is_empty() {
        return forwarded.to_vec();
    }

    let agreed = stored
        .iter()
        .zip(forwarded)
        .take_while(|(stored, forwarded)| same_message(stored, forwarded))
        .count();
    let start = forwarded[agreed..]
        .iter()
        .position(|message| message.role == Role::User)
        .map_or(agreed, |offset| agreed + offset);
    let candidate = &forwarded[start..];

    if ends_with(stored, candidate) {
        return Vec::new();
    }
    candidate.to_vec()
}

/// Whether two messages carry the same role and text. Tool traffic never
/// reaches this store, so those fields are not compared.
fn same_message(a: &ChatMessage, b: &ChatMessage) -> bool {
    a.role == b.role && a.content == b.content
}

/// Whether `haystack` ends with every message in `needle`.
fn ends_with(haystack: &[ChatMessage], needle: &[ChatMessage]) -> bool {
    haystack.len() >= needle.len()
        && haystack[haystack.len() - needle.len()..]
            .iter()
            .zip(needle)
            .all(|(a, b)| same_message(a, b))
}

/// Store one message. Messages with no text (an assistant turn that only calls
/// tools) are skipped: the history is a readable transcript, not a protocol log.
fn insert_message(
    conn: &Connection,
    thread_id: &str,
    seq: i64,
    message: &ChatMessage,
) -> rusqlite::Result<()> {
    let Some(content) = message.content.as_deref() else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO messages (thread_id, seq, role, content) VALUES (?1, ?2, ?3, ?4)",
        params![thread_id, seq, role_text(message.role), content],
    )?;
    Ok(())
}

/// The lowercase role name stored in the `role` column.
fn role_text(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Rebuild a message from its stored role + content.
fn chat_from_row(role: &str, content: &str) -> Option<ChatMessage> {
    match role {
        "system" => Some(ChatMessage::system(content)),
        "user" => Some(ChatMessage::user(content)),
        "assistant" => Some(ChatMessage::assistant(content)),
        _ => None,
    }
}

/// Milliseconds since the Unix epoch, or `0` if the clock is before it.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Report a storage failure without taking the run down — losing a transcript
/// entry is not worth failing the user's chat over.
fn report(action: &str, error: rusqlite::Error) {
    eprintln!("sentence-video: cannot {action}: {error}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::user(text)
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::assistant(text)
    }

    #[test]
    fn records_and_reads_back_a_transcript() {
        let store = ThreadStore::new();
        assert!(store.is_empty());

        store.append("t1", &[ChatMessage::user("hi")]);
        store.append("t1", &[ChatMessage::assistant("hello")]);

        let history = store.history("t1");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content.as_deref(), Some("hi"));
        assert_eq!(history[1].content.as_deref(), Some("hello"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn appending_nothing_is_a_no_op() {
        let store = ThreadStore::new();
        store.append("t1", &[]);
        assert!(store.history("t1").is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn merge_records_only_the_turn_the_store_has_not_seen() {
        let store = ThreadStore::new();
        assert_eq!(store.merge("t1", &[user("one")]).len(), 1);
        // The assistant's reply is captured when the run finishes.
        store.append("t1", &[assistant("two")]);

        // Second turn: the client re-sends its cumulative history.
        let history = store.merge("t1", &[user("one"), assistant("two"), user("three")]);
        assert_eq!(history.len(), 3);
        assert_eq!(history[2].content.as_deref(), Some("three"));
        assert_eq!(store.history("t1").len(), 3);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn merge_keeps_earlier_turns_when_the_client_forwards_one_message() {
        let store = ThreadStore::new();
        store.append("t1", &[user("one"), assistant("two")]);
        store.append("t1", &[user("three"), assistant("four")]);

        // A client that trims what it forwards sends only the pending message.
        // The model must still get everything the server recorded before it.
        let history = store.merge("t1", &[user("five")]);
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].content.as_deref(), Some("one"));
        assert_eq!(history[4].content.as_deref(), Some("five"));
    }

    #[test]
    fn merge_does_not_duplicate_a_turn_the_client_repeats() {
        let store = ThreadStore::new();
        store.append("t1", &[user("one"), assistant("two")]);

        // A re-run of the same turn, and a client that forwards only its last
        // message: both add nothing the store does not already hold.
        assert_eq!(store.merge("t1", &[user("one"), assistant("two")]).len(), 2);
        assert_eq!(store.merge("t1", &[assistant("two")]).len(), 2);
    }

    #[test]
    fn merge_seeds_an_empty_store_from_the_forwarded_conversation() {
        let store = ThreadStore::new();
        // A restart leaves no record, but the client still has the thread.
        let history = store.merge("t1", &[user("one"), assistant("two"), user("three")]);
        assert_eq!(history.len(), 3);
        assert_eq!(store.history("t1").len(), 3);
    }

    #[test]
    fn an_echoed_reply_that_drifted_is_not_recorded_twice() {
        let stored = vec![user("one"), assistant("two")];
        // The client's copy of the reply differs from ours by a trailing space.
        let pending = pending_turn(&stored, &[user("one"), assistant("two "), user("three")]);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].content.as_deref(), Some("three"));
    }

    #[test]
    fn unknown_threads_are_empty_and_clear_reports_existence() {
        let store = ThreadStore::new();
        assert!(store.history("missing").is_empty());
        assert!(!store.clear("missing"));

        store.append("t1", &[ChatMessage::user("hi")]);
        assert!(store.clear("t1"));
        assert!(store.history("t1").is_empty());
        assert!(store.is_empty());
    }

    #[test]
    fn summaries_are_sorted_by_id() {
        let store = ThreadStore::new();
        store.append("b", &[ChatMessage::user("1")]);
        store.append("a", &[ChatMessage::user("1"), ChatMessage::assistant("2")]);

        let summaries = store.summaries();
        assert_eq!(summaries[0].id, "a");
        assert_eq!(summaries[0].messages, 2);
        assert_eq!(summaries[1].id, "b");
    }
}
