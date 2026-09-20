import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { cn } from '@/lib/utils';

/**
 * Sentence-segmented player — the page that talks to the Rust
 * `sentence-video` backend (see rust/src/app.rs). It uses ONLY the endpoints
 * the Rust binary actually implements:
 *
 *   GET  /api/sessions                             → session summaries
 *   POST /api/session                              { media_path, subtitle_path? }
 *   GET  /api/session/{id}                         → { id, media_path, media_duration, cues[] }
 *   GET  /api/session/{id}/cues                    → sentence list
 *   GET  /api/session/{id}/media                   → Range-streamed source media
 *   GET  /api/session/{id}/audio?start=&end=       → exact WAV slice of a time range
 *
 * Every other page in this app targets the legacy Python/FastAPI backend, so
 * those routes are hidden from the top nav (but still reachable by URL).
 */

interface Word {
  text: string;
  start: number;
  end: number;
  score: number;
}

interface Cue {
  index: number;
  start: number;
  end: number;
  text: string;
  words?: Word[];
}

interface Session {
  id: string;
  media_path: string;
  media_duration: number;
  source_format: string;
  cues: Cue[];
}

interface SessionSummary {
  session_id: string;
  media_path: string;
  media_duration: number;
  cue_count: number;
  /** Last sentence played, or -1 when the session was never opened. */
  last_index: number;
  /** Last playback position in seconds. */
  position: number;
  updated_at: number;
}

/** Persisted "where the user left off" record for a session. */
interface Progress {
  last_index: number;
  position: number;
}

function fmtTime(sec: number): string {
  if (!isFinite(sec) || sec < 0) return '--:--.---';
  const m = Math.floor(sec / 60);
  const s = sec - m * 60;
  return `${String(m).padStart(2, '0')}:${s.toFixed(3).padStart(6, '0')}`;
}

function basename(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

/** The Rust server reports failures as `{ "error": "..." }`. */
async function readError(resp: Response): Promise<string> {
  try {
    const data = await resp.json();
    return data?.error || data?.message || `HTTP ${resp.status}`;
  } catch {
    return `HTTP ${resp.status}`;
  }
}

/**
 * Ask the Rust server to open the native OS file dialog on the machine it runs
 * on and hand back the chosen absolute path (browsers hide `input[type=file]`
 * paths for security, so the server must produce the path itself).
 * Returns `null` when the user cancels the dialog.
 */
async function pickFileOnServer(kind: 'media' | 'subtitle'): Promise<string | null> {
  const resp = await fetch(`/api/pick-file?kind=${kind}`, { method: 'POST' });
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { path: string | null } = await resp.json();
  return data.path ?? null;
}

function chipClass(active: boolean): string {
  return cn(
    'rounded-full border px-3 py-1 text-xs font-medium transition-colors',
    active ? 'border-primary bg-primary text-primary-foreground' : 'border-border hover:bg-muted'
  );
}

export function PlayerPage() {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [session, setSession] = useState<Session | null>(null);
  const [cues, setCues] = useState<Cue[]>([]);
  const [activeIdx, setActiveIdx] = useState(-1);
  const [loopMode, setLoopMode] = useState(false);
  const [playThrough, setPlayThrough] = useState(false);
  const [filter, setFilter] = useState('');
  const [mediaPath, setMediaPath] = useState('');
  const [subtitlePath, setSubtitlePath] = useState('');
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [picking, setPicking] = useState<'media' | 'subtitle' | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mediaRef = useRef<HTMLVideoElement | null>(null);
  const listRef = useRef<HTMLOListElement | null>(null);
  const activeRef = useRef(-1);
  const loopRef = useRef(false);
  const throughRef = useRef(false);
  const cuesRef = useRef<Cue[]>([]);
  const sessionIdRef = useRef<string | null>(null);
  /** Position to apply once the media element has loaded its metadata. */
  const pendingPositionRef = useRef<number | null>(null);

  // Keep the async interval/keyboard handlers reading current values.
  cuesRef.current = cues;
  loopRef.current = loopMode;
  throughRef.current = playThrough;

  /** Apply a restored position as soon as the media can accept a seek. */
  const applyPendingPosition = useCallback(() => {
    const m = mediaRef.current;
    const pos = pendingPositionRef.current;
    if (!m || pos == null || m.readyState < 1) return;
    try {
      m.currentTime = pos;
      pendingPositionRef.current = null;
    } catch {
      /* media not seekable yet — retried on the next metadata event */
    }
  }, []);

  const saveProgress = useCallback((index: number, position: number) => {
    const id = sessionIdRef.current;
    if (!id) return;
    void fetch(`/api/session/${encodeURIComponent(id)}/progress`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ last_index: index, position }),
    }).catch(() => {
      /* progress saving is best-effort */
    });
  }, []);

  const loadSession = useCallback(
    async (id: string) => {
      try {
        const resp = await fetch(`/api/session/${encodeURIComponent(id)}`);
        if (!resp.ok) throw new Error(await readError(resp));
        const data: Session = await resp.json();
        const loadedCues = data.cues ?? [];
        setSession(data);
        setCues(loadedCues);
        sessionIdRef.current = id;
        setError(null);

        // Restore where this session was left off last time.
        let progress: Progress = { last_index: -1, position: 0 };
        try {
          const pr = await fetch(`/api/session/${encodeURIComponent(id)}/progress`);
          if (pr.ok) progress = await pr.json();
        } catch {
          /* progress is best-effort */
        }
        if (progress.last_index >= 0 && progress.last_index < loadedCues.length) {
          activeRef.current = progress.last_index;
          setActiveIdx(progress.last_index);
          pendingPositionRef.current = progress.position;
          applyPendingPosition();
        } else {
          activeRef.current = -1;
          setActiveIdx(-1);
          pendingPositionRef.current = null;
        }
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [applyPendingPosition]
  );

  const refreshSessions = useCallback(
    async (autoLoad: boolean) => {
      try {
        const resp = await fetch('/api/sessions');
        if (!resp.ok) throw new Error(await readError(resp));
        const data: SessionSummary[] = await resp.json();
        const list = Array.isArray(data) ? data : [];
        setSessions(list);
        if (autoLoad && list.length > 0) await loadSession(list[0].session_id);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [loadSession]
  );

  useEffect(() => {
    void refreshSessions(true);
  }, [refreshSessions]);

  const playIndex = useCallback(
    (i: number) => {
      const m = mediaRef.current;
      const cs = cuesRef.current;
      if (!m || i < 0 || i >= cs.length) return;
      activeRef.current = i;
      setActiveIdx(i);
      m.currentTime = cs[i].start;
      void m.play().catch(() => {});
      // Record the sentence as the resume point immediately.
      saveProgress(i, cs[i].start);
    },
    [saveProgress]
  );

  // Persist playback progress every few seconds while playing.
  useEffect(() => {
    const timer = window.setInterval(() => {
      const m = mediaRef.current;
      if (!m || m.paused || activeRef.current < 0) return;
      saveProgress(activeRef.current, m.currentTime);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [saveProgress]);

  // Drive single-segment stop, loop, and play-through from the media clock.
  useEffect(() => {
    const timer = window.setInterval(() => {
      const m = mediaRef.current;
      const cs = cuesRef.current;
      const i = activeRef.current;
      if (!m || i < 0 || i >= cs.length) return;
      const cue = cs[i];
      if (m.currentTime >= cue.end - 0.02) {
        if (loopRef.current) {
          m.currentTime = cue.start;
          void m.play().catch(() => {});
        } else if (throughRef.current && i + 1 < cs.length) {
          playIndex(i + 1);
        } else {
          m.pause();
        }
      }
    }, 60);
    return () => window.clearInterval(timer);
  }, [playIndex]);

  const createSession = useCallback(
    async (overrideMedia?: string, overrideSubtitle?: string) => {
      const media = (overrideMedia ?? mediaPath).trim();
      const subtitle = (overrideSubtitle ?? subtitlePath).trim();
      if (!media) {
        setError('Choose a video or enter a media path first.');
        return;
      }
      setCreating(true);
      setError(null);
      try {
        const body: Record<string, string> = { media_path: media };
        if (subtitle) body.subtitle_path = subtitle;
        const resp = await fetch('/api/session', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        if (!resp.ok) throw new Error(await readError(resp));
        const data: { session_id: string } = await resp.json();
        setMediaPath('');
        setSubtitlePath('');
        await refreshSessions(false);
        await loadSession(data.session_id);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setCreating(false);
      }
    },
    [mediaPath, subtitlePath, refreshSessions, loadSession]
  );

  /**
   * Pick a video through the server's native dialog and process it right
   * away: the returned path is one the Rust server can actually read, so it
   * is sent straight to `POST /api/session`.
   */
  const chooseVideo = useCallback(async () => {
    setError(null);
    setPicking('media');
    try {
      const path = await pickFileOnServer('media');
      if (!path) return; // cancelled
      setMediaPath(path);
      await createSession(path, subtitlePath);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(null);
    }
  }, [createSession, subtitlePath]);

  /** Optionally narrow the subtitle the server auto-detects next to the video. */
  const chooseSubtitle = useCallback(async () => {
    setError(null);
    setPicking('subtitle');
    try {
      const path = await pickFileOnServer('subtitle');
      if (path) setSubtitlePath(path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(null);
    }
  }, []);

  /**
   * Play one sentence as a server-decoded audio clip. Unlike normal playback
   * (which seeks the original media in the browser), the Rust server extracts
   * exactly `[cue.start, cue.end]`, so the boundaries are sample-accurate.
   */
  const playClip = useCallback(
    (i: number) => {
      const s = session;
      const cs = cuesRef.current;
      if (!s || i < 0 || i >= cs.length) return;
      const cue = cs[i];
      const url = `/api/session/${encodeURIComponent(s.id)}/audio?start=${cue.start}&end=${cue.end}`;
      void new Audio(url).play().catch(() => {});
    },
    [session]
  );

  // Auto-scroll the active sentence into view.
  useEffect(() => {
    if (activeIdx < 0 || !listRef.current) return;
    const el = listRef.current.querySelector<HTMLElement>(`[data-idx="${activeIdx}"]`);
    el?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }, [activeIdx]);

  // Keyboard shortcuts: ↑/↓ prev/next, Space play/pause, L loop, Enter replay.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (document.activeElement?.tagName || '').toUpperCase();
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      const m = mediaRef.current;
      const cs = cuesRef.current;
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          playIndex(Math.min(activeRef.current + 1, cs.length - 1));
          break;
        case 'ArrowUp':
          e.preventDefault();
          playIndex(Math.max(activeRef.current - 1, 0));
          break;
        case ' ':
          e.preventDefault();
          if (m) {
            if (m.paused) void m.play().catch(() => {});
            else m.pause();
          }
          break;
        case 'l':
        case 'L':
          setLoopMode((v) => !v);
          break;
        case 'Enter':
          if (activeRef.current >= 0) playIndex(activeRef.current);
          break;
        default:
          break;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [playIndex]);

  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return cues
      .map((cue, i) => ({ cue, i }))
      .filter(({ cue }) => !q || cue.text.toLowerCase().includes(q));
  }, [cues, filter]);

  const mediaSrc = session ? `/api/session/${encodeURIComponent(session.id)}/media` : undefined;
  const activeCue = activeIdx >= 0 ? cues[activeIdx] : undefined;

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h1 className="text-3xl font-bold tracking-tight">🎬 Sentence Player</h1>
        <p className="text-muted-foreground">
          Served by the Rust <code className="font-mono text-xs">sentence-video</code> backend.
          Click a sentence to play exactly that slice — with loop and play-through modes.
        </p>
      </div>

      {error && (
        <div className="rounded-md border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {error}
        </div>
      )}

      <div className="space-y-3 rounded-lg border bg-card p-4">
        <h2 className="text-sm font-semibold">Open a session</h2>
        <div className="flex flex-wrap gap-2">
          <button
            onClick={() => void chooseVideo()}
            disabled={picking !== null || creating}
            className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-opacity disabled:opacity-50"
          >
            {picking === 'media' ? 'Waiting for the file dialog…' : '📂 Choose video…'}
          </button>
          <button
            onClick={() => void chooseSubtitle()}
            disabled={picking !== null || creating}
            className="rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            {picking === 'subtitle' ? 'Waiting for the file dialog…' : 'Subtitle…'}
          </button>
          <button
            onClick={() => void createSession()}
            disabled={creating || !mediaPath.trim()}
            className="rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            {creating ? 'Creating…' : 'Use entered path'}
          </button>
        </div>
        <div className="flex flex-col gap-2 lg:flex-row">
          <input
            value={mediaPath}
            onChange={(e) => setMediaPath(e.target.value)}
            placeholder="/absolute/path/to/video.mp4"
            className="flex-1 rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
            disabled={creating || picking !== null}
          />
          <input
            value={subtitlePath}
            onChange={(e) => setSubtitlePath(e.target.value)}
            placeholder="optional subtitle .srt/.vtt"
            className="flex-1 rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
            disabled={creating || picking !== null}
          />
        </div>
        <p className="text-xs text-muted-foreground">
          “Choose video…” opens a file dialog on the machine running the Rust server and
          immediately processes the file it returns. If a{' '}
          <code className="font-mono">.srt</code>/<code className="font-mono">.vtt</code> sits
          next to the media it is used; otherwise the audio is transcribed on the spot by the
          built-in speech-to-text model (no internet needed). You can also type an absolute
          path manually.
        </p>

        {loading ? (
          <p className="text-sm text-muted-foreground">Loading sessions…</p>
        ) : sessions.length > 0 ? (
          <div className="flex flex-wrap gap-2">
            {sessions.map((s) => (
              <button
                key={s.session_id}
                onClick={() => void loadSession(s.session_id)}
                className={cn(
                  'rounded-full border px-3 py-1 text-xs transition-colors',
                  session?.id === s.session_id
                    ? 'border-primary bg-primary/10 font-medium'
                    : 'hover:bg-muted'
                )}
              >
                {basename(s.media_path)} · {s.cue_count} cues
                {s.last_index >= 0 ? ` · resume #${s.last_index + 1}` : ''}
              </button>
            ))}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No saved sessions yet. Choose a video above — it is stored in the local SQLite
            database and reopened (at your last sentence) next time.
          </p>
        )}
      </div>

      {session && (
        <div className="grid gap-6 lg:grid-cols-2">
          <div className="space-y-3">
            <video
              ref={mediaRef}
              src={mediaSrc}
              controls
              preload="metadata"
              onLoadedMetadata={applyPendingPosition}
              onPause={() => {
                const m = mediaRef.current;
                if (m) saveProgress(activeRef.current, m.currentTime);
              }}
              onEnded={() => {
                const m = mediaRef.current;
                if (m) saveProgress(activeRef.current, m.currentTime);
              }}
              className="aspect-video w-full rounded-lg border bg-black"
            />
            <div className="flex flex-wrap items-center gap-2">
              <button onClick={() => setLoopMode((v) => !v)} className={chipClass(loopMode)}>
                Loop
              </button>
              <button
                onClick={() => setPlayThrough((v) => !v)}
                className={chipClass(playThrough)}
              >
                Play through
              </button>
              {activeIdx >= 0 && (
                <button onClick={() => playClip(activeIdx)} className={chipClass(false)}>
                  Sentence clip
                </button>
              )}
              <span className="ml-auto text-xs text-muted-foreground">
                {basename(session.media_path)} · {session.media_duration.toFixed(1)}s ·{' '}
                {cues.length} sentences
              </span>
            </div>
            <div className="min-h-10 rounded-md border bg-muted/40 px-3 py-2 text-sm">
              {activeCue ? `▶ ${activeCue.text}` : 'Click a sentence below to play it.'}
            </div>
            <p className="text-xs text-muted-foreground">
              Shortcuts: ↑/↓ previous/next · Space play/pause · L loop · Enter replay.
            </p>
          </div>

          <div className="space-y-3">
            <div className="flex items-center gap-3">
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                type="search"
                placeholder="Filter sentences…"
                className="flex-1 rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
              />
              <span className="whitespace-nowrap text-xs text-muted-foreground">
                {shown.length} / {cues.length}
              </span>
            </div>
            <ol ref={listRef} className="max-h-[60vh] space-y-1 overflow-auto rounded-lg border p-2">
              {shown.map(({ cue, i }) => (
                <li key={cue.index}>
                  <button
                    data-idx={i}
                    onClick={() => playIndex(i)}
                    className={cn(
                      'w-full rounded-md border px-3 py-2 text-left transition-colors',
                      i === activeIdx
                        ? 'border-primary bg-primary/10'
                        : 'border-transparent hover:bg-muted'
                    )}
                  >
                    <div className="flex items-center justify-between text-xs text-muted-foreground">
                      <span>#{cue.index + 1}</span>
                      <span className="font-mono">
                        {fmtTime(cue.start)} – {fmtTime(cue.end)}
                      </span>
                    </div>
                    <div className="text-sm">{cue.text}</div>
                  </button>
                </li>
              ))}
            </ol>
          </div>
        </div>
      )}
    </div>
  );
}
