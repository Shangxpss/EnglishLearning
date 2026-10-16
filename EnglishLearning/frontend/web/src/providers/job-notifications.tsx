import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { Link } from 'react-router';
import { cn, fileName } from '@/lib/utils';
import {
  JOB_TERMINAL_STATES,
  subscribeJobEvents,
  type JobEvent,
  type JobState,
} from '@/lib/server-api';

/**
 * Job notifications — the "tell me when it's done" half of the queue.
 *
 * The queue page is where files are submitted, but a build takes minutes, so
 * the notice has to find the user wherever they went afterwards. This provider
 * therefore lives next to the app shell rather than inside the page: it holds
 * the single SSE subscription and renders the toast stack above every route.
 *
 * The feed is at-least-once — it replays the whole snapshot on connect, and
 * again after any reconnect — so the same job decision arrives more than once.
 * Two guards keep that from turning into a wall of toasts: `(job_id, state)` is
 * toasted at most once, and anything whose `finished_at` is not recent is
 * treated as history rather than news (otherwise every page load would
 * re-announce every job ever run).
 */

/** How long a finished-but-not-yet-seen notice stays up. */
const NOTICE_MS = 10_000;

/**
 * How recent a terminal event has to be to count as news. The SSE snapshot
 * arrives with no timestamp of its own, so `finished_at` is the only way to
 * tell a job that just ended from one that ended yesterday.
 */
const FRESH_SECS = 15;

interface Notice {
  /** `(job_id, state)` — the dedupe key the feed's contract promises. */
  key: string;
  state: JobState;
  media_path: string;
  session_id?: string;
  error?: string;
}

/** Tone per outcome: only a real failure is alarming; a stop was asked for. */
function noticeTone(state: JobState): string {
  switch (state) {
    case 'done':
      return 'border-emerald-500/40 bg-emerald-500/10';
    case 'failed':
      return 'border-destructive/40 bg-destructive/10';
    default:
      return 'border-border bg-muted/80';
  }
}

function noticeTitle(state: JobState): string {
  switch (state) {
    case 'done':
      return 'Ready to play';
    case 'failed':
      return 'Processing failed';
    default:
      return 'Processing cancelled';
  }
}

export function JobNotificationsProvider({ children }: { children: ReactNode }) {
  const [notices, setNotices] = useState<Notice[]>([]);

  // Survives re-renders and reconnects: a job toasted once is never toasted
  // again, even if the stream replays it.
  const seen = useRef(new Set<string>());

  const push = useCallback((notice: Notice) => {
    setNotices((prev) => [...prev, notice]);
    window.setTimeout(
      () => setNotices((prev) => prev.filter((n) => n.key !== notice.key)),
      NOTICE_MS
    );
  }, []);

  useEffect(() => {
    return subscribeJobEvents({
      onEvent: (event: JobEvent) => {
        if (!JOB_TERMINAL_STATES.includes(event.state)) return;
        const key = `${event.job_id}:${event.state}`;
        if (seen.current.has(key)) return;
        if (Math.floor(Date.now() / 1000) - event.finished_at > FRESH_SECS) {
          // The snapshot replay: already over when the page opened.
          seen.current.add(key);
          return;
        }
        seen.current.add(key);
        push({
          key,
          state: event.state,
          media_path: event.media_path,
          session_id: event.session_id,
          error: event.error,
        });
      },
    });
  }, [push]);

  return (
    <>
      {children}
      {/* Below the sticky header and above every page, so the notice is visible
          without covering the player controls or the Copilot button. */}
      <div className="pointer-events-none fixed top-16 right-4 z-[60] flex w-80 flex-col gap-2">
        {notices.map((notice) => (
          <div
            key={notice.key}
            className={cn(
              'pointer-events-auto rounded-lg border p-3 text-sm shadow-lg backdrop-blur',
              noticeTone(notice.state)
            )}
          >
            <div className="flex items-start gap-2">
              <span className="font-medium">{noticeTitle(notice.state)}</span>
              <button
                type="button"
                onClick={() =>
                  setNotices((prev) => prev.filter((n) => n.key !== notice.key))
                }
                className="ml-auto text-xs text-muted-foreground hover:text-foreground"
                aria-label="Dismiss"
              >
                ✕
              </button>
            </div>
            <div
              className="mt-1 truncate text-xs text-muted-foreground"
              title={notice.media_path}
            >
              {fileName(notice.media_path)}
            </div>
            {notice.error && (
              <div className="mt-1 text-xs text-destructive wrap-break-word">
                {notice.error}
              </div>
            )}
            {notice.state === 'done' && notice.session_id && (
              <Link
                to={`/player?session=${encodeURIComponent(notice.session_id)}`}
                className="mt-2 inline-block text-xs font-medium text-primary hover:underline"
              >
                Open in player →
              </Link>
            )}
          </div>
        ))}
      </div>
    </>
  );
}
