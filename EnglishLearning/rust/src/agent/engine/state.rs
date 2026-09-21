//! In-memory conversation state — the replacement for LangGraph's `MemorySaver`
//! and the runtime's `InMemoryAgentRunner`.
//!
//! Scope is deliberately small: this is a local, single-user desktop service, so
//! a `RwLock<HashMap>` is enough. Nothing is persisted across restarts, which
//! matches the old `MemorySaver`/`InMemoryAgentRunner` behaviour
//! (`AI-Demo/technical_architecture.md` §7.3 lists durable storage as future
//! work).
//!
//! The store is a **transcript log**. The model context for a run comes from the
//! messages the client sends (AG-UI is stateless per run), so nothing here can
//! drift out of sync with the frontend.

use crate::agent::llm::ChatMessage;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::RwLock;

/// Threads and the messages recorded against them.
#[derive(Default)]
pub struct ThreadStore {
    threads: RwLock<HashMap<String, Vec<ChatMessage>>>,
}

/// A thread as reported by `GET /copilotkit/threads`.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub messages: usize,
}

impl ThreadStore {
    /// An empty store.
    pub fn new() -> Self {
        ThreadStore { threads: RwLock::new(HashMap::new()) }
    }

    /// Record `messages` against `thread_id`, creating the thread if needed.
    pub fn append(&self, thread_id: &str, messages: &[ChatMessage]) {
        if messages.is_empty() {
            return;
        }
        let mut guard = self.write();
        guard
            .entry(thread_id.to_string())
            .or_default()
            .extend_from_slice(messages);
    }

    /// A copy of the recorded transcript.
    pub fn history(&self, thread_id: &str) -> Vec<ChatMessage> {
        self.read().get(thread_id).cloned().unwrap_or_default()
    }

    /// Forget a thread. Returns whether it existed.
    pub fn clear(&self, thread_id: &str) -> bool {
        self.write().remove(thread_id).is_some()
    }

    /// Every known thread, sorted by id for stable output.
    pub fn summaries(&self) -> Vec<ThreadSummary> {
        let mut out: Vec<ThreadSummary> = self
            .read()
            .iter()
            .map(|(id, messages)| ThreadSummary { id: id.clone(), messages: messages.len() })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Number of known threads.
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no thread has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read the map, recovering from a poisoned lock instead of panicking:
    /// a panic in one request must not take the whole service down.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Vec<ChatMessage>>> {
        self.threads.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write the map, recovering from a poisoned lock.
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Vec<ChatMessage>>> {
        self.threads.write().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reads_back_a_transcript() {
        let store = ThreadStore::new();
        assert!(store.is_empty());

        store.append("t1", &[ChatMessage::user("hi")]);
        store.append("t1", &[ChatMessage::assistant("hello")]);

        let history = store.history("t1");
        assert_eq!(history.len(), 2);
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
    fn unknown_threads_are_empty_and_clear_reports_existence() {
        let store = ThreadStore::new();
        assert!(store.history("missing").is_empty());
        assert!(!store.clear("missing"));

        store.append("t1", &[ChatMessage::user("hi")]);
        assert!(store.clear("t1"));
        assert!(store.history("t1").is_empty());
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
