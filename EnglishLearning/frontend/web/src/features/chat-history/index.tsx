import { useCallback, useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { startNewChatThread } from "@/lib/chat-thread";
import {
  getChatMessages,
  listChatThreads,
  type ChatMessage,
  type ChatThread,
} from "@/lib/chat-api";

/**
 * Chat History — past conversations with the floating CopilotKit assistant.
 *
 * The agent runtime records every turn into `threads.db`, so this page is a
 * read-only view of that store: the conversations on the left, the selected
 * one's transcript on the right.
 *
 * CopilotKit ships a `<CopilotThreadsDrawer>`, but it is gated behind the paid
 * "CopilotKit Intelligence" license and does not fetch against a self-hosted
 * runtime — hence this page.
 */

/** A short, human-friendly time for the thread list. */
function fmtTime(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString();
}

/** A thread id is machine-generated; show a tail so rows stay recognisable. */
function shortId(id: string): string {
  return id.length > 18 ? `…${id.slice(-16)}` : id;
}

function MessageBubble({ message }: { message: ChatMessage }) {
  const text = message.content ?? "";

  if (message.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[80%] whitespace-pre-wrap rounded-2xl rounded-br-sm bg-primary px-3.5 py-2 text-sm text-primary-foreground">
          {text}
        </div>
      </div>
    );
  }

  if (message.role === "assistant") {
    return (
      <div className="flex justify-start">
        <div className="max-w-[80%] whitespace-pre-wrap rounded-2xl rounded-bl-sm bg-muted px-3.5 py-2 text-sm">
          {text}
        </div>
      </div>
    );
  }

  // system / tool rows are not part of the stored transcript in practice.
  return (
    <div className="text-center text-xs italic text-muted-foreground">
      [{message.role}] {text}
    </div>
  );
}

export function ChatHistoryPage() {
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [loadingThreads, setLoadingThreads] = useState(true);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadThreads = useCallback(async () => {
    setLoadingThreads(true);
    setError(null);
    try {
      const list = await listChatThreads();
      setThreads(list);
      // Keep the current selection if it still exists, else open the newest.
      setSelected((cur) =>
        cur && list.some((t) => t.id === cur) ? cur : (list[0]?.id ?? null),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingThreads(false);
    }
  }, []);

  useEffect(() => {
    void loadThreads();
  }, [loadThreads]);

  useEffect(() => {
    if (!selected) {
      setMessages([]);
      return;
    }
    let cancelled = false;
    setLoadingMessages(true);
    getChatMessages(selected)
      .then((m) => {
        if (!cancelled) setMessages(m);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoadingMessages(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selected]);

  const active = threads.find((t) => t.id === selected);

  return (
    <div className="flex h-[calc(100vh-8rem)] gap-4">
      {/* Conversation list */}
      <aside className="flex w-72 shrink-0 flex-col rounded-lg border bg-card">
        <div className="flex items-center justify-between border-b px-3 py-2">
          <h1 className="text-sm font-semibold">Chat History</h1>
          <div className="flex items-center gap-1">
            <Button
              size="sm"
              variant="ghost"
              title="Pin a fresh thread for the floating assistant"
              onClick={() => {
                startNewChatThread();
                void loadThreads();
              }}
            >
              New chat
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => void loadThreads()}
              disabled={loadingThreads}
            >
              Refresh
            </Button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto">
          {loadingThreads ? (
            <p className="px-3 py-6 text-center text-sm text-muted-foreground">Loading…</p>
          ) : threads.length === 0 ? (
            <p className="px-3 py-6 text-center text-sm text-muted-foreground">
              No conversations yet. Chat with the assistant (bottom-right) and they will appear
              here.
            </p>
          ) : (
            <ul className="p-1">
              {threads.map((t) => (
                <li key={t.id}>
                  <button
                    type="button"
                    onClick={() => setSelected(t.id)}
                    title={t.id}
                    className={cn(
                      "w-full rounded-md px-2.5 py-2 text-left transition-colors",
                      t.id === selected ? "bg-primary/10" : "hover:bg-muted",
                    )}
                  >
                    <div className="truncate font-mono text-xs">{shortId(t.id)}</div>
                    <div className="mt-0.5 flex items-center justify-between text-[11px] text-muted-foreground">
                      <span>{t.messages} messages</span>
                      <span>{fmtTime(t.updated_at)}</span>
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </aside>

      {/* Transcript */}
      <section className="flex min-w-0 flex-1 flex-col rounded-lg border bg-card">
        <div className="border-b px-4 py-2">
          <h2 className="truncate font-mono text-xs text-muted-foreground">
            {active ? active.id : "—"}
          </h2>
        </div>

        <div className="flex-1 space-y-3 overflow-y-auto p-4">
          {error ? (
            <p className="text-sm text-destructive">{error}</p>
          ) : !selected ? (
            <p className="text-center text-sm text-muted-foreground">
              Select a conversation to read it.
            </p>
          ) : loadingMessages ? (
            <p className="text-center text-sm text-muted-foreground">Loading…</p>
          ) : messages.length === 0 ? (
            <p className="text-center text-sm text-muted-foreground">This conversation is empty.</p>
          ) : (
            messages.map((m, i) => <MessageBubble key={i} message={m} />)
          )}
        </div>
      </section>
    </div>
  );
}
