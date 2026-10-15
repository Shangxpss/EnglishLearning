import { readError } from "@/lib/server-api";

/**
 * Client for the agent runtime's durable chat history.
 *
 * The floating CopilotKit assistant talks to `sentence-video agent` (proxied at
 * `/copilotkit` by vite.config.ts), which persists every turn into `threads.db`.
 * These two endpoints are that store's read side: the ordered list of threads
 * and one thread's transcript. They are plain `fetch` calls for the same reason
 * the media-server client is — a tiny surface, no library needed.
 */

/** One entry of `GET /copilotkit/threads`. */
export interface ChatThread {
  id: string;
  /** Number of stored messages (user + assistant). */
  messages: number;
  /** Unix epoch milliseconds. */
  created_at: number;
  updated_at: number;
}

/** One stored message from `GET /copilotkit/threads/{id}/messages`. */
export interface ChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content?: string | null;
}

/** `GET /copilotkit/threads` — every recorded conversation, newest activity first. */
export async function listChatThreads(): Promise<ChatThread[]> {
  const resp = await fetch("/copilotkit/threads");
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { threads?: ChatThread[] } = await resp.json();
  const threads = Array.isArray(data.threads) ? data.threads : [];
  // The server sorts by id for stable output; the page wants recent first.
  return [...threads].sort((a, b) => b.updated_at - a.updated_at);
}

/** `GET /copilotkit/threads/{id}/messages` — one thread's transcript, oldest first. */
export async function getChatMessages(threadId: string): Promise<ChatMessage[]> {
  const resp = await fetch(`/copilotkit/threads/${encodeURIComponent(threadId)}/messages`);
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { messages?: ChatMessage[] } = await resp.json();
  return Array.isArray(data.messages) ? data.messages : [];
}
