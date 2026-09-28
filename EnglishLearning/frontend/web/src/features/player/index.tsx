import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';
import { cn } from '@/lib/utils';
import {
  pickFileOnServer,
  readError,
  type DubbingState,
  type SessionSummary,
} from '@/lib/server-api';

/**
 * Sentence-segmented player — the page that talks to the Rust
 * `sentence-video` backend (see rust/src/app.rs). It uses ONLY the endpoints
 * the Rust binary actually implements:
 *
 *   GET  /api/sessions                             → session summaries
 *   POST /api/session                              { media_path, subtitle_path?, align?, dub?, dub_engine? }
 *   GET  /api/session/{id}                         → { id, media_path, media_duration, cues[], dubbed, video }
 *   GET  /api/session/{id}/cues                    → sentence list
 *   GET  /api/session/{id}/media                   → Range-streamed source media
 *   GET  /api/session/{id}/dubbed-audio            → Range-streamed replacement voice track
 *   GET  /api/session/{id}/audio?start=&end=       → exact WAV slice of a time range
 *
 * A session with `dubbed: true` carries a server-generated voice track that
 * replaces the original audio: the video is muted, the track is played
 * alongside it, and the two are kept in sync.
 *
 * A session whose source video is a codec browsers cannot decode (HEVC/H.265
 * is common in course exports) reports `video.playable: false`; the server
 * re-encodes it to H.264 once, and until that copy exists the player shows a
 * "preparing" placeholder instead of a silent black rectangle.
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
  /** The server generated a replacement voice track for this session. */
  dubbed?: boolean;
  /**
   * The source video stream. `playable: false` means the codec (typically
   * HEVC/H.265, which Chromium cannot decode) is being re-encoded to H.264 by
   * the server; the player shows a placeholder until then.
   */
  video?: {
    codec: string | null;
    playable: boolean;
    transcode: 'none' | 'idle' | 'running' | 'done' | 'failed';
    error?: string;
  };
  /** How far the server's dubbing job has got (`/dub`). */
  dubbing?: DubbingState;
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
  /** Refine the word timings with wav2vec2 forced alignment (`align` in the POST body). */
  const [align, setAlign] = useState(false);
  /** Replace the voice with TTS synthesis (`dub` / `dub_engine` in the POST body). */
  const [dub, setDub] = useState(false);
  const [dubEngine, setDubEngine] = useState('edge-tts');
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [picking, setPicking] = useState<'media' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [manageOpen, setManageOpen] = useState(false);
  /** Session id being deleted, or '*' while deleting everything. */
  const [deleting, setDeleting] = useState<string | null>(null);

  const mediaRef = useRef<HTMLVideoElement | null>(null);
  /** The replacement voice track, played alongside the muted video. */
  const dubbedRef = useRef<HTMLAudioElement | null>(null);
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

  /** True when the open session carries a server-generated voice track. */
  const dubbed = session?.dubbed === true;
  const sessionKey = session?.id;
  /** True once the source video can actually be decoded by this browser. */
  const videoPlayable = session?.video?.playable ?? true;
  const transcode = session?.video?.transcode ?? 'none';
  const videoFailed = transcode === 'failed';
  /** The server is still rendering sentences; `dubbed` flips when it finishes. */
  const dubbing = session?.dubbing;
  const dubbingRunning = dubbing?.state === 'running';

  /**
   * Point the replacement voice track at the open session. The track is a
   * sibling `Audio` element rather than the video's own audio, so the video is
   * muted below and the two are driven together.
   */
  useEffect(() => {
    if (!sessionKey || !dubbed) {
      dubbedRef.current?.pause();
      dubbedRef.current = null;
      return;
    }
    const track = new Audio(
      `/api/session/${encodeURIComponent(sessionKey)}/dubbed-audio`
    );
    track.preload = 'auto';
    dubbedRef.current = track;
    return () => {
      track.pause();
      dubbedRef.current = null;
    };
  }, [sessionKey, dubbed]);

  /** Silence the original audio while the dubbed track is playing. */
  useEffect(() => {
    const m = mediaRef.current;
    if (m) m.muted = dubbed;
  }, [dubbed, sessionKey]);

  /**
   * While there is server-side work in flight — an H.264 re-encode of a source
   * this browser cannot decode, or a dubbing run — re-read the session every
   * few seconds so the player can swap in the video and the voice track the
   * moment they are ready.
   */
  const transcodePending = !videoPlayable && !videoFailed;
  useEffect(() => {
    if (!sessionKey || (!transcodePending && !dubbingRunning)) return;
    const t = window.setInterval(() => {
      void fetch(`/api/session/${encodeURIComponent(sessionKey)}`)
        .then((resp) => (resp.ok ? resp.json() : null))
        .then((data: Session | null) => {
          if (data) setSession(data);
        })
        .catch(() => {
          /* keep polling — the server may just be busy re-encoding */
        });
    }, 3000);
    return () => window.clearInterval(t);
  }, [sessionKey, transcodePending, dubbingRunning]);

  /**
   * Mirror the video's transport onto the voice track. `hard` forces the
   * current time across (after a seek); otherwise only a drift beyond 150 ms
   * is corrected, so the track is never nudged audibly.
   */
  const syncDubbed = useCallback((hard: boolean) => {
    const m = mediaRef.current;
    const a = dubbedRef.current;
    if (!m || !a) return;
    if (hard || Math.abs(a.currentTime - m.currentTime) > 0.15) {
      a.currentTime = m.currentTime;
    }
    a.playbackRate = m.playbackRate;
    // The video is muted, so volume must come from the track itself.
    a.volume = m.volume;
    if (m.paused) a.pause();
    else void a.play().catch(() => {});
  }, []);

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

  /** `?session=<id>` lets the Manage page deep-link into one specific session. */
  const [searchParams] = useSearchParams();
  const requestedSession = searchParams.get('session');

  useEffect(() => {
    // `?session=<id>` (used by the Manage page's "Open in player") opens that
    // session; without it the most recently stored one is opened.
    void refreshSessions(!requestedSession).then(() => {
      if (requestedSession) void loadSession(requestedSession);
    });
  }, [refreshSessions, loadSession, requestedSession]);

  /** Drop the currently open session from the UI (after it was deleted). */
  const clearOpenSession = useCallback(() => {
    sessionIdRef.current = null;
    setSession(null);
    setCues([]);
    activeRef.current = -1;
    setActiveIdx(-1);
    pendingPositionRef.current = null;
  }, []);

  /** `DELETE /api/session/{id}` — remove one stored session. */
  const deleteSession = useCallback(
    async (id: string) => {
      setDeleting(id);
      setError(null);
      try {
        const resp = await fetch(`/api/session/${encodeURIComponent(id)}`, {
          method: 'DELETE',
        });
        if (!resp.ok) throw new Error(await readError(resp));
        if (sessionIdRef.current === id) clearOpenSession();
        await refreshSessions(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setDeleting(null);
      }
    },
    [refreshSessions, clearOpenSession]
  );

  /** `DELETE /api/sessions` — wipe every stored session. */
  const deleteAllSessions = useCallback(async () => {
    setDeleting('*');
    setError(null);
    try {
      const resp = await fetch('/api/sessions', { method: 'DELETE' });
      if (!resp.ok) throw new Error(await readError(resp));
      clearOpenSession();
      await refreshSessions(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeleting(null);
    }
  }, [refreshSessions, clearOpenSession]);

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
  // Polled at ~20 ms: a coarser timer lets playback run past `cue.end` into the
  // first word of the next sentence, which is audible as a clipped word.
  useEffect(() => {
    const timer = window.setInterval(() => {
      const m = mediaRef.current;
      // Keep the replacement voice track locked to the video it replaces.
      if (m && dubbedRef.current) syncDubbed(false);
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
    }, 20);
    return () => window.clearInterval(timer);
  }, [playIndex, syncDubbed]);

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
        const body: Record<string, string | boolean> = { media_path: media };
        if (subtitle) body.subtitle_path = subtitle;
        // Only ever opts IN: leaving the box unchecked keeps whatever the server
        // was started with (`--align` / `SENTENCE_VIDEO_ALIGN=1`).
        if (align) body.align = true;
        // Same for dubbing: the server synthesises a replacement voice track
        // and the player uses it instead of the video's own audio.
        if (dub) {
          body.dub = true;
          body.dub_engine = dubEngine;
        }
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
    [mediaPath, subtitlePath, align, dub, dubEngine, refreshSessions, loadSession]
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

  // What /media resolves to changes once the server's H.264 copy lands, so the
  // URL carries the recipe: the browser fetches the copy instead of reusing a
  // cached response for the original stream it could not decode.
  const mediaSrc = session
    ? `/api/session/${encodeURIComponent(session.id)}/media${
        transcode === 'done' ? '?v=h264' : ''
      }`
    : undefined;
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
          <label
            title="Refine the word timings with wav2vec2 forced alignment. Only applies when the audio is transcribed — an existing <name>_full_sentence.srt is used as-is."
            className="flex cursor-pointer items-center gap-2 rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted has-disabled:opacity-50"
          >
            <input
              type="checkbox"
              checked={align}
              onChange={(e) => setAlign(e.target.checked)}
              disabled={creating || picking !== null}
              className="h-4 w-4 accent-primary"
            />
            Align timings
          </label>
          <label
            title="Synthesise a replacement voice track for this video and play it instead of the original audio. One TTS call per sentence, placed at the sentence's start time and re-synthesised at a faster rate only when it would overlap the next sentence."
            className="flex cursor-pointer items-center gap-2 rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted has-disabled:opacity-50"
          >
            <input
              type="checkbox"
              checked={dub}
              onChange={(e) => setDub(e.target.checked)}
              disabled={creating || picking !== null}
              className="h-4 w-4 accent-primary"
            />
            Dub audio
          </label>
          <select
            value={dubEngine}
            onChange={(e) => setDubEngine(e.target.value)}
            disabled={!dub || creating || picking !== null}
            title="Which TTS engine synthesises the replacement voice."
            className="rounded-md border bg-background px-3 py-2 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            <option value="edge-tts">edge-tts (neural, en-GB Maisie)</option>
            <option value="espeak-ng">espeak-ng (offline)</option>
          </select>
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
          immediately processes the file it returns. It looks for{' '}
          <code className="font-mono">&lt;name&gt;_full_sentence.srt</code> next to the video:
          if that exists it is used as-is, otherwise the audio is transcribed by the built-in
          speech-to-text model and saved to that file (no internet needed). You can also type
          an absolute path manually. Tick <strong>Align timings</strong> to refine that
          transcription's word timings with wav2vec2 forced alignment — it only applies when
          the audio is transcribed (an existing <code className="font-mono">_full_sentence.srt</code>{' '}
          is used as-is) and only in a server built with the <code className="font-mono">asr-align</code> feature.
          Tick <strong>Dub audio</strong> to also synthesise a replacement voice track and play it
          instead of the original audio (the video is muted). The engine is chosen beside the box:
          <code className="font-mono">edge-tts</code> (the default) uses the English
          (United Kingdom) <strong>Maisie</strong> neural voice and needs to be installed and
          online; <code className="font-mono">espeak-ng</code> works offline. Dubbing keeps
          each sentence at its original start time and only speeds a sentence up when it would
          otherwise run into the next one.
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
            {videoPlayable ? (
              <video
                ref={mediaRef}
                src={mediaSrc}
                controls
                preload="metadata"
                onLoadedMetadata={applyPendingPosition}
                onPlay={() => syncDubbed(false)}
                onPause={() => {
                  dubbedRef.current?.pause();
                  const m = mediaRef.current;
                  if (m) saveProgress(activeRef.current, m.currentTime);
                }}
                onSeeked={() => syncDubbed(true)}
                onRateChange={() => syncDubbed(false)}
                onVolumeChange={() => syncDubbed(false)}
                onEnded={() => {
                  const m = mediaRef.current;
                  if (m) saveProgress(activeRef.current, m.currentTime);
                }}
                className="aspect-video w-full rounded-lg border bg-black"
              />
            ) : (
              <div className="flex aspect-video w-full flex-col items-center justify-center gap-2 rounded-lg border bg-black px-6 text-center">
                {videoFailed ? (
                  <>
                    <span className="text-sm text-destructive">
                      This video could not be prepared for playback.
                    </span>
                    <span className="font-mono text-xs text-muted-foreground">
                      {session.video?.error}
                    </span>
                  </>
                ) : (
                  <>
                    <span className="text-sm">Preparing video for playback…</span>
                    <span className="text-xs text-muted-foreground">
                      This browser cannot decode {(session.video?.codec ?? 'this').toUpperCase()},
                      so the server is re-encoding it to H.264. That happens once per file —
                      you can keep using the sentence list meanwhile.
                    </span>
                  </>
                )}
              </div>
            )}
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
                {cues.length} sentences{dubbed ? ' · 🔉 dubbed voice' : ''}
              </span>
            </div>
            {dubbingRunning && (
              <p className="text-xs text-muted-foreground">
                Dubbing… {dubbing?.done ?? 0}/{dubbing?.total ?? cues.length} sentences
                {dubbing?.stage === 'stitch' ? ' · assembling the voice track' : ''}
              </p>
            )}
            {dubbing?.state === 'failed' && (
              <p className="text-xs text-destructive">
                {(dubbing.failed?.length ?? 0) > 0
                  ? `${dubbing.failed.length} sentence(s) could not be dubbed and keep a silent slot — resume from the Manage page to retry just those.`
                  : dubbing.error}
              </p>
            )}
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

      {/* Floating button: manage the sessions stored in SQLite. */}
      <button
        onClick={() => setManageOpen(true)}
        title="Manage stored sessions"
        className="fixed right-6 bottom-6 z-40 flex items-center gap-2 rounded-full bg-primary px-4 py-3 text-sm font-medium text-primary-foreground shadow-lg transition-transform hover:scale-105 focus:ring-2 focus:ring-ring focus:outline-none"
      >
        🗂 Sessions
        {sessions.length > 0 && (
          <span className="rounded-full bg-primary-foreground/20 px-2 py-0.5 text-xs">
            {sessions.length}
          </span>
        )}
      </button>

      {manageOpen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
          onClick={() => setManageOpen(false)}
          role="dialog"
          aria-modal="true"
        >
          <div
            className="flex max-h-[80vh] w-full max-w-2xl flex-col rounded-xl border bg-card shadow-xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between gap-4 border-b px-5 py-3">
              <div>
                <h2 className="text-lg font-semibold">Stored sessions</h2>
                <p className="text-xs text-muted-foreground">
                  Saved in the local SQLite database. Deleting removes the record, its cues
                  and its resume point.
                </p>
              </div>
              <button
                onClick={() => setManageOpen(false)}
                aria-label="Close"
                className="rounded-md px-2 py-1 text-sm hover:bg-muted"
              >
                ✕
              </button>
            </div>

            <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
              {sessions.length === 0 ? (
                <p className="py-8 text-center text-sm text-muted-foreground">
                  No stored sessions.
                </p>
              ) : (
                <ul className="space-y-2">
                  {sessions.map((s) => (
                    <li
                      key={s.session_id}
                      className="flex items-center gap-3 rounded-lg border p-3"
                    >
                      <div className="min-w-0 flex-1">
                        <div className="truncate text-sm font-medium">
                          {basename(s.media_path)}
                        </div>
                        <div className="truncate font-mono text-[11px] text-muted-foreground">
                          {s.media_path}
                        </div>
                        <div className="text-xs text-muted-foreground">
                          {s.cue_count} cues · {s.media_duration.toFixed(1)}s
                          {s.last_index >= 0 ? ` · resume #${s.last_index + 1}` : ''}
                          {session?.id === s.session_id ? ' · open' : ''}
                        </div>
                      </div>
                      <button
                        onClick={() => void deleteSession(s.session_id)}
                        disabled={deleting !== null}
                        className="shrink-0 rounded-md border border-destructive/40 px-3 py-1.5 text-xs font-medium text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-50"
                      >
                        {deleting === s.session_id ? 'Deleting…' : 'Delete'}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <div className="flex items-center justify-between border-t px-5 py-3">
              <span className="text-xs text-muted-foreground">
                {sessions.length} record{sessions.length === 1 ? '' : 's'}
              </span>
              <button
                onClick={() => void deleteAllSessions()}
                disabled={deleting !== null || sessions.length === 0}
                className="rounded-md border border-destructive/40 px-3 py-1.5 text-xs font-medium text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-50"
              >
                {deleting === '*' ? 'Deleting…' : 'Delete all'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
