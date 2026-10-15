/**
 * The chat thread the floating assistant is pinned to.
 *
 * AG-UI is stateless per run and the v2 client generates a fresh `threadId`
 * every time an agent is constructed, so without pinning, each page load would
 * start a new conversation (and `threads.db` would fill with one-turn threads).
 * Keeping the id in `localStorage` makes a reload resume the same thread — which
 * the server can then replay, because it persists each thread's transcript.
 *
 * "New chat" clears the id, so the next load mints a fresh one.
 */

const STORAGE_KEY = "english-learning.chat-thread";

/** A fresh, collision-free id prefixed to mark it as browser-owned. */
function newThreadId(): string {
  const uuid =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  return `web-${uuid}`;
}

/** The pinned thread id, creating and storing one on first use. */
export function getChatThreadId(): string {
  try {
    const existing = window.localStorage.getItem(STORAGE_KEY);
    if (existing) return existing;
    const id = newThreadId();
    window.localStorage.setItem(STORAGE_KEY, id);
    return id;
  } catch {
    // Private mode / storage disabled: fall back to a per-load id.
    return newThreadId();
  }
}

/** Forget the pinned thread so the next load starts a new conversation. */
export function resetChatThread(): void {
  try {
    window.localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* nothing to do — the id was never persisted */
  }
}

/** Fired after [`startNewChatThread`] so a mounted popup re-pins immediately. */
export const NEW_CHAT_THREAD_EVENT = "chat-thread:new";

/**
 * Start a fresh conversation: drop the pinned id, mint a new one, and tell the
 * mounted `<CopilotPopup>` (which is not remounted by navigation) to switch.
 */
export function startNewChatThread(): string {
  resetChatThread();
  const id = getChatThreadId();
  window.dispatchEvent(new Event(NEW_CHAT_THREAD_EVENT));
  return id;
}
