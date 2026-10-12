import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router';
import { cn } from '@/lib/utils';
import {
  alignSession,
  cancelBuild,
  createSession,
  deleteSession,
  generatePlatformSubtitle,
  getSession,
  listBuilds,
  listSessions,
  muxVideo,
  openMuxedDir,
  openPlatformDir,
  pickFileOnServer,
  retranscode,
  startDub,
  type BuildProgress,
  type SessionSummary,
  type SessionDetail,
  type VideoStatus,
} from '@/lib/server-api';

/**
 * Manage page — CRUD over the videos the Rust `sentence-video` server has
 * processed, plus the processing itself.
 *
 * The Player is for *watching*; this page is for *managing*. Each row reports
 * what the server knows: how long the media is, how many sentences it has, how
 * far the user got through it, whether the video can be played in a browser,
 * and how far dubbing has got. Actions: add a video, cancel a video still being
 * processed, start/resume dubbing, mux the dubbed voice back into the video,
 * reveal that muxed file's folder in the server's file manager, export a
 * short-cue `.srt` for a video platform beside the video, reveal that subtitle's
 * folder, re-transcode, open in the player, delete.
 *
 * Processing a fresh video blocks its own request for minutes, so the panel above
 * the table is the only sign it is happening at all — and the only place it can be
 * stopped, since a build has no session id to act on until it ends.
 *
 * Dubbing runs in the background on the server, so a run is *polled*: the row
 * shows `done/total` while it runs and lists the sentences that failed once it
 * stops. Resuming reuses every sentence already synthesized — only the failures
 * are retried — which is the point of the checkpointing on the server. Muxing is
 * the opposite: it is a stream copy plus one audio encode, so it finishes inside
 * the request and the row just flips to "muxed".
 *
 * The platform-subtitle export sits between those two. On an undubbed video it is
 * a pure text split — the timings are already right — and returns at once; on a
 * dubbed one the server re-transcribes the replacement voice track to recover
 * correct word timings, so it is polled like dubbing.
 */

function basename(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function fmtDuration(sec: number): string {
  if (!isFinite(sec) || sec <= 0) return '--';
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec - h * 3600) / 60);
  const s = Math.floor(sec - h * 3600 - m * 60);
  return h > 0
    ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
    : `${m}:${String(s).padStart(2, '0')}`;
}

/** A neutral `state` chip: colour carries the meaning. */
function stateChip(tone: 'ok' | 'warn' | 'bad' | 'busy' | 'idle'): string {
  return cn(
    'inline-flex items-center rounded-full px-2 py-0.5 text-[11px] font-medium',
    tone === 'ok' && 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400',
    tone === 'warn' && 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
    tone === 'bad' && 'bg-destructive/15 text-destructive',
    tone === 'busy' && 'bg-primary/15 text-primary',
    tone === 'idle' && 'bg-muted text-muted-foreground'
  );
}

/**
 * What the server is doing to a video that has no session yet.
 *
 * Transcribing and aligning a fresh video takes minutes and happens *before* the
 * session row exists, so this panel is the only thing that distinguishes "busy"
 * from "nothing happened".
 */
function buildLabel(b: BuildProgress): string {
  switch (b.stage) {
    case 'decode':
      return 'decoding audio…';
    case 'transcribe':
      return 'transcribing speech…';
    case 'align':
      return b.total > 0
        ? `aligning word timings · window ${b.done}/${b.total}`
        : 'aligning word timings…';
  }
}

/** Why the stage takes as long as it does — the question "is it stuck?". */
function buildHint(b: BuildProgress): string {
  switch (b.stage) {
    case 'decode':
      return 'Decoding the audio to 16 kHz mono for the model.';
    case 'transcribe':
      return 'Whisper transcribes the whole file in a single pass, so it reports no sub-progress. Expect a few minutes for a long video.';
    case 'align':
      return 'wav2vec2 forced alignment works through 30-second windows on the CPU — the slowest step by far, minutes per window in a debug build. The console is quiet between windows, which is normal.';
  }
}

/**
 * A table row: the `GET /api/sessions` summary, plus the two fields only
 * `GET /api/session/{id}` reports. Keeping the summary as the row source means
 * the "watched" and "cue count" columns come from the cheap list call.
 */
interface Row extends SessionSummary {
  video?: VideoStatus;
  sourceFormat?: string;
}

export function ManagePage() {
  const [rows, setRows] = useState<Row[]>([]);
  const [builds, setBuilds] = useState<BuildProgress[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  /** Session id whose action is in flight, or `'add'`. */
  const [busy, setBusy] = useState<string | null>(null);
  /** Media path of a build the user asked to stop, until it actually stops. */
  const [cancellingFor, setCancellingFor] = useState<string | null>(null);
  /** Session whose alignment is being recomputed — minutes, so it is labelled. */
  const [aligningFor, setAligningFor] = useState<string | null>(null);

  // Add-video options: the same choices the Player's "Open a session" box has.
  const [addMedia, setAddMedia] = useState('');
  const [addAlign, setAddAlign] = useState(false);
  const [addDub, setAddDub] = useState(false);
  const [addEngine, setAddEngine] = useState('edge-tts');

  /**
   * Load every session, then its detail.
   *
   * `GET /api/sessions` carries the dub state but not the video status, and the
   * video status is an ffprobe per file — too expensive to fold into a list the
   * Player also calls. Fetching each detail here also *opens* each session,
   * which is what starts the server's H.264 re-encode of an unplayable source.
   */
  const refresh = useCallback(async (showSpinner: boolean) => {
    if (showSpinner) setLoading(true);
    try {
      // Both are polled together: a video being transcribed has no session yet,
      // so the list alone cannot show that anything is happening.
      const [summaries, inFlight] = await Promise.all([
        listSessions(),
        // An older server has no `/api/builds`; that must not break the list.
        listBuilds().catch(() => [] as BuildProgress[]),
      ]);
      const details = await Promise.all(
        summaries.map((s) => getSession(s.session_id).catch(() => null))
      );
      const byId = new Map(
        details
          .filter((d): d is SessionDetail => d !== null)
          .map((d) => [d.id, d] as const)
      );
      setRows(
        summaries.map((s) => {
          const detail = byId.get(s.session_id);
          return { ...s, video: detail?.video, sourceFormat: detail?.source_format };
        })
      );
      setBuilds(inFlight);
      // A cancelled build keeps being reported until its window in flight ends,
      // so the row is what clears the "Cancelling…" state — not the click.
      setCancellingFor((prev) =>
        prev && inFlight.some((b) => b.media_path === prev) ? prev : null
      );
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (showSpinner) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh(true);
  }, [refresh]);

  const anyRunning = useMemo(
    () =>
      rows.some(
        (r) =>
          r.dubbing?.state === 'running' ||
          r.video?.transcode === 'running' ||
          r.platform_subtitle?.state === 'running'
      ),
    [rows]
  );

  // Poll while the server has work in flight, so progress and the final state
  // arrive without the user reloading.
  //
  // `busy === 'add'` is the important one: processing a video blocks that POST
  // for minutes, and a build has no session row, so without polling the
  // "Processing" panel would never appear until the work was already over.
  const watching = busy === 'add' || builds.length > 0 || anyRunning;
  useEffect(() => {
    if (!watching) return;
    const t = window.setInterval(() => {
      void refresh(false);
    }, 3000);
    return () => window.clearInterval(t);
  }, [watching, refresh]);

  /** Process one already-chosen path and open it once ready. */
  const addVideo = useCallback(
    async (mediaPath: string, subtitlePath?: string) => {
      await createSession({
        media_path: mediaPath,
        ...(subtitlePath ? { subtitle_path: subtitlePath } : {}),
        ...(addAlign ? { align: true } : {}),
        ...(addDub ? { dub: true, dub_engine: addEngine } : {}),
      });
      await refresh(false);
    },
    [addAlign, addDub, addEngine, refresh]
  );

  /** The native dialog runs on the server machine; the browser cannot pick a path. */
  const chooseVideo = useCallback(async () => {
    setBusy('add');
    setError(null);
    try {
      const path = await pickFileOnServer('media');
      if (path) await addVideo(path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [addVideo]);

  const useTypedPath = useCallback(async () => {
    const path = addMedia.trim();
    if (!path) return;
    setBusy('add');
    setError(null);
    try {
      await addVideo(path);
      setAddMedia('');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [addMedia, addVideo]);

  /**
   * Ask the server to stop processing one video.
   *
   * No refresh afterwards: the answer is "flag raised", not "stopped" — the build
   * is still in the list until its current window finishes, and the poll is what
   * shows it going away.
   */
  const cancelProcessing = useCallback(async (mediaPath: string) => {
    setError(null);
    setCancellingFor(mediaPath);
    try {
      await cancelBuild(mediaPath);
    } catch (e) {
      // Usually a 404: it finished between the poll that showed it and the click.
      setCancellingFor(null);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const runDubbing = useCallback(
    async (sessionId: string, retryFailed: boolean) => {
      setBusy(sessionId);
      setError(null);
      try {
        await startDub(sessionId, retryFailed ? { retryFailed: true } : {});
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [refresh]
  );

  const reTranscode = useCallback(
    async (sessionId: string) => {
      setBusy(sessionId);
      setError(null);
      try {
        await retranscode(sessionId);
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [refresh]
  );

  /**
   * Mux the voice track back into the video, into the server's `muxed/` folder.
   *
   * Blocking on purpose — unlike dubbing this is seconds — but the row still
   * needs a refresh afterwards, because the file it reports on only exists once
   * the call returns.
   */
  const runMux = useCallback(
    async (sessionId: string) => {
      setBusy(sessionId);
      setError(null);
      try {
        await muxVideo(sessionId);
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [refresh]
  );

  /**
   * Reveal the muxed video's folder in the file manager of the machine running
   * the server — the same machine as this browser in the default setup.
   *
   * Nothing comes back but a status, so the row is not refreshed: the result is
   * a window that opened on the desktop, not a change in the server's state.
   */
  const openMuxedFolder = useCallback(async (sessionId: string) => {
    setBusy(sessionId);
    setError(null);
    try {
      await openMuxedDir(sessionId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, []);

  /**
   * Recompute this session's word timings with wav2vec2 forced alignment.
   *
   * The POST blocks for minutes, like adding a video, so the row is left in the
   * "Aligning…" state and the page's own polling shows the window counter in the
   * Processing panel. Windows aligned by an earlier, interrupted run are reused,
   * which is why this is the way to finish an alignment that was stopped.
   */
  const runAlign = useCallback(
    async (sessionId: string) => {
      setBusy(sessionId);
      setAligningFor(sessionId);
      setError(null);
      try {
        await alignSession(sessionId);
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setAligningFor(null);
        setBusy(null);
      }
    },
    [refresh]
  );

  /**
   * Export the short-cue `<stem>.srt` a video platform accepts, beside the
   * source video.
   *
   * Two branches on the server decide the cost. An *undubbed* video already has
   * correct timings, so the export is a pure text split and returns at once. A
   * *dubbed* one is re-timed from a fresh transcription of the replacement voice
   * track — the new speech does not sit where the original did — so that takes
   * minutes and the POST hands back `running`. Unlike the mux, this is polled:
   * the page's own refresh picks up the ending state, exactly like dubbing.
   */
  const runPlatformSubtitle = useCallback(
    async (sessionId: string) => {
      setBusy(sessionId);
      setError(null);
      try {
        await generatePlatformSubtitle(sessionId);
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [refresh]
  );

  /**
   * Reveal the exported subtitle's folder — the source video's own folder — in
   * the file manager of the server machine. Like the muxed folder, nothing comes
   * back but a status, so the row is not refreshed.
   */
  const openSubtitleFolder = useCallback(async (sessionId: string) => {
    setBusy(sessionId);
    setError(null);
    try {
      await openPlatformDir(sessionId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, []);

  const remove = useCallback(
    async (sessionId: string) => {
      setBusy(sessionId);
      setError(null);
      try {
        await deleteSession(sessionId);
        await refresh(false);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [refresh]
  );

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h1 className="text-3xl font-bold tracking-tight">🗂 Manage videos</h1>
        <p className="text-muted-foreground">
          Everything the Rust <code className="font-mono text-xs">sentence-video</code> server
          has processed: add a video, watch its processing, resume a run that failed, or delete
          it. To watch one, open it in the{' '}
          <Link to="/player" className="underline hover:text-foreground">
            Player
          </Link>
          .
        </p>
      </div>

      {error && (
        <div
          className={cn(
            'rounded-md border px-4 py-3 text-sm',
            // A cancel surfaces here as the 409 the blocked POST finally returns.
            // The user pressed the button, so it is not a failure to shout about.
            /cancel/i.test(error)
              ? 'border-primary/40 bg-primary/5 text-foreground'
              : 'border-destructive/40 bg-destructive/10 text-destructive'
          )}
        >
          {error}
        </div>
      )}

      <div className="space-y-3 rounded-lg border bg-card p-4">
        <h2 className="text-sm font-semibold">Add a video</h2>
        <div className="flex flex-wrap gap-2">
          <button
            onClick={() => void chooseVideo()}
            disabled={busy !== null}
            className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-opacity disabled:opacity-50"
          >
            {busy === 'add' ? 'Working…' : '📂 Choose video…'}
          </button>
          <label
            title="Refine the word timings with wav2vec2 forced alignment. Only applies when the audio is transcribed — an existing <name>_full_sentence.srt is used as-is."
            className="flex cursor-pointer items-center gap-2 rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted has-disabled:opacity-50"
          >
            <input
              type="checkbox"
              checked={addAlign}
              onChange={(e) => setAddAlign(e.target.checked)}
              disabled={busy !== null}
              className="h-4 w-4 accent-primary"
            />
            Align timings
          </label>
          <label
            title="Also synthesize a replacement voice track. It keeps running in the background after this returns — watch the progress in the table below."
            className="flex cursor-pointer items-center gap-2 rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted has-disabled:opacity-50"
          >
            <input
              type="checkbox"
              checked={addDub}
              onChange={(e) => setAddDub(e.target.checked)}
              disabled={busy !== null}
              className="h-4 w-4 accent-primary"
            />
            Dub audio
          </label>
          <select
            value={addEngine}
            onChange={(e) => setAddEngine(e.target.value)}
            disabled={!addDub || busy !== null}
            className="rounded-md border bg-background px-3 py-2 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            <option value="edge-tts">edge-tts (neural, en-GB Maisie)</option>
            <option value="espeak-ng">espeak-ng (offline)</option>
          </select>
        </div>
        <div className="flex flex-col gap-2 lg:flex-row">
          <input
            value={addMedia}
            onChange={(e) => setAddMedia(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void useTypedPath();
            }}
            placeholder="/absolute/path/to/video.mp4"
            className="flex-1 rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
            disabled={busy !== null}
          />
          <button
            onClick={() => void useTypedPath()}
            disabled={busy !== null || !addMedia.trim()}
            className="rounded-md border px-4 py-2 text-sm font-medium transition-colors hover:bg-muted disabled:opacity-50"
          >
            Process path
          </button>
        </div>
        <p className="text-xs text-muted-foreground">
          “Choose video…” opens a file dialog on the machine running the server. Processing looks
          for <code className="font-mono">&lt;name&gt;_full_sentence.srt</code> beside the video and
          transcribes the audio only when it is missing, so re-adding is quick.
        </p>
      </div>

      {builds.length > 0 && (
        <div className="space-y-4 rounded-lg border border-primary/40 bg-primary/5 p-4">
          <h2 className="text-sm font-semibold">⏳ Processing</h2>
          {builds.map((b) => {
            const pct = b.total > 0 ? Math.round((b.done / b.total) * 100) : 0;
            return (
              <div key={b.media_path} className="space-y-1.5">
                <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
                  <span className="truncate text-sm font-medium">{basename(b.media_path)}</span>
                  <span className="flex items-center gap-3">
                    <span className="font-mono text-xs text-muted-foreground">
                      {fmtDuration(b.elapsed_secs)} elapsed
                    </span>
                    <button
                      onClick={() => void cancelProcessing(b.media_path)}
                      disabled={cancellingFor === b.media_path}
                      title="Stop processing this video. The alignment window already running has to finish first; its result is kept, so adding the video again resumes from there."
                      className="rounded-md border px-2.5 py-1 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                    >
                      {cancellingFor === b.media_path ? 'Cancelling…' : 'Cancel'}
                    </button>
                  </span>
                </div>
                <div className="flex items-center gap-3">
                  <span className={stateChip('busy')}>{buildLabel(b)}</span>
                  {b.total > 0 && (
                    <div className="h-1.5 w-40 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-primary transition-[width]"
                        style={{ width: `${pct}%` }}
                      />
                    </div>
                  )}
                </div>
                <p className="max-w-prose text-xs text-muted-foreground">{buildHint(b)}</p>
                {cancellingFor === b.media_path && (
                  <p className="max-w-prose text-xs text-primary">
                    Stopping — the alignment window already running has to finish first, which can
                    take minutes on a long video. Every window aligned so far is kept, so adding this
                    video again resumes from there instead of starting over.
                  </p>
                )}
              </div>
            );
          })}
          <p className="border-t border-primary/20 pt-3 text-xs text-muted-foreground">
            This video joins the table below once processing ends — a session is only stored when
            every sentence is known.
          </p>
        </div>
      )}

      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold">
          {loading ? 'Loading…' : `${rows.length} video${rows.length === 1 ? '' : 's'}`}
        </h2>
        <button
          onClick={() => void refresh(true)}
          disabled={loading}
          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
        >
          Refresh
        </button>
      </div>

      {!loading && rows.length === 0 ? (
        <p className="rounded-lg border border-dashed py-10 text-center text-sm text-muted-foreground">
          Nothing processed yet. Add a video above.
        </p>
      ) : (
        <div className="overflow-x-auto rounded-lg border">
          <table className="w-full text-sm">
            <thead className="border-b bg-muted/40 text-left text-xs text-muted-foreground">
              <tr>
                <th className="px-4 py-2 font-medium">Video</th>
                <th className="px-4 py-2 font-medium">Sentences</th>
                <th className="px-4 py-2 font-medium">Watched</th>
                <th className="px-4 py-2 font-medium">Playback</th>
                <th className="px-4 py-2 font-medium">Dubbing</th>
                <th className="px-4 py-2 text-right font-medium">Actions</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                const dub = row.dubbing;
                const dubState = dub?.state ?? 'idle';
                const transcodeState = row.video?.transcode ?? 'none';
                const muxed = row.muxed?.state === 'done';
                const platform = row.platform_subtitle;
                const platformState = platform?.state ?? 'idle';
                const disabled = busy !== null;
                /** Watching progress: how far through the sentences the user got. */
                const watchedPct =
                  row.cue_count > 0 && row.last_index >= 0
                    ? Math.round(((row.last_index + 1) / row.cue_count) * 100)
                    : 0;
                return (
                  <tr key={row.session_id} className="border-b last:border-b-0 align-top">
                    <td className="max-w-[22rem] px-4 py-3">
                      <div className="truncate font-medium">{basename(row.media_path)}</div>
                      <div className="truncate font-mono text-[11px] text-muted-foreground">
                        {row.media_path}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        {fmtDuration(row.media_duration)} · {row.sourceFormat ?? 'asr'}
                      </div>
                    </td>

                    <td className="px-4 py-3 text-xs text-muted-foreground">
                      {row.cue_count} cue{row.cue_count === 1 ? '' : 's'}
                    </td>

                    <td className="px-4 py-3">
                      {row.last_index >= 0 ? (
                        <div className="space-y-1">
                          <span className="text-xs">
                            #{row.last_index + 1} of {row.cue_count} · {watchedPct}%
                          </span>
                          <div className="h-1.5 w-24 overflow-hidden rounded-full bg-muted">
                            <div
                              className="h-full rounded-full bg-primary"
                              style={{ width: `${watchedPct}%` }}
                            />
                          </div>
                        </div>
                      ) : (
                        <span className="text-xs text-muted-foreground">not started</span>
                      )}
                    </td>

                    <td className="px-4 py-3">
                      {transcodeState === 'none' && (
                        <span className={stateChip('ok')}>
                          {row.video?.codec ? row.video.codec.toUpperCase() : 'audio only'}
                        </span>
                      )}
                      {transcodeState === 'idle' && <span className={stateChip('idle')}>idle</span>}
                      {transcodeState === 'running' && (
                        <span className={stateChip('busy')}>re-encoding H.264…</span>
                      )}
                      {transcodeState === 'done' && (
                        <span className={stateChip('ok')}>H.264 ready</span>
                      )}
                      {transcodeState === 'failed' && (
                        <span className={stateChip('bad')} title={row.video?.error}>
                          transcode failed
                        </span>
                      )}
                    </td>

                    <td className="px-4 py-3">
                      {dubState === 'idle' && (
                        <span className={stateChip('idle')}>
                          {dub && dub.done > 0 ? `${dub.done}/${dub.total} ready` : 'not dubbed'}
                        </span>
                      )}
                      {dubState === 'running' && (
                        <div className="space-y-1">
                          <span className={stateChip('busy')}>
                            {dub?.stage === 'stitch' ? 'assembling' : 'dubbing'} {dub?.done ?? 0}/
                            {dub?.total ?? row.cue_count}
                          </span>
                          <div className="h-1.5 w-24 overflow-hidden rounded-full bg-muted">
                            <div
                              className="h-full rounded-full bg-primary transition-[width]"
                              style={{
                                width: `${
                                  (dub?.total ?? row.cue_count) > 0
                                    ? Math.round(
                                        ((dub?.done ?? 0) / (dub?.total ?? row.cue_count)) * 100
                                      )
                                    : 0
                                }%`,
                              }}
                            />
                          </div>
                        </div>
                      )}
                      {dubState === 'done' && <span className={stateChip('ok')}>🔉 dubbed</span>}
                      {dubState === 'failed' && (
                        <div className="space-y-1">
                          <span className={stateChip('bad')}>
                            {dub?.failed?.length
                              ? `${dub.failed.length} sentence${
                                  dub.failed.length === 1 ? '' : 's'
                                } failed`
                              : 'failed'}
                          </span>
                          {dub?.error && (
                            <div className="max-w-[18rem] truncate text-[11px] text-muted-foreground">
                              {dub.error}
                            </div>
                          )}
                          {dub?.failed && dub.failed.length > 0 && (
                            <details className="max-w-[18rem] text-[11px]">
                              <summary className="cursor-pointer text-muted-foreground">
                                show failures
                              </summary>
                              <ul className="mt-1 space-y-1">
                                {dub.failed.map((f) => (
                                  <li key={f.index} className="text-muted-foreground">
                                    <span className="font-mono">#{f.index + 1}</span>{' '}
                                    <span className="text-foreground">{f.text}</span>
                                    <br />
                                    <span className="text-destructive">{f.error}</span>
                                  </li>
                                ))}
                              </ul>
                            </details>
                          )}
                        </div>
                      )}
                    </td>

                    <td className="px-4 py-3">
                      <div className="flex flex-wrap justify-end gap-2">
                        <button
                          onClick={() => void runDubbing(row.session_id, dubState !== 'idle')}
                          disabled={disabled || dubState === 'running'}
                          title={
                            dubState === 'failed'
                              ? 'Retry only the sentences that failed — the rest are reused from disk.'
                              : dubState === 'done'
                                ? 'Re-synthesize, reusing every sentence already on disk.'
                                : 'Synthesize a replacement voice track.'
                          }
                          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                        >
                          {dubState === 'running'
                            ? 'Dubbing…'
                            : dubState === 'failed'
                              ? 'Resume dubbing'
                              : dubState === 'done'
                                ? 'Re-dub'
                                : 'Dub'}
                        </button>
                        <button
                          onClick={() => void runMux(row.session_id)}
                          disabled={disabled || !row.dubbed}
                          title={
                            !row.dubbed
                              ? 'Dub this session first — there is no voice track to mux.'
                              : muxed
                                ? `Write a fresh mp4 over ${row.muxed?.path}.`
                                : 'Write the video and its dubbed voice track into one mp4, in the server’s muxed/ folder.'
                          }
                          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                        >
                          {muxed ? 'Re-mux' : 'Mux'}
                        </button>
                        {muxed && (
                          <button
                            onClick={() => void openMuxedFolder(row.session_id)}
                            disabled={disabled}
                            title={`Open the server folder holding ${row.muxed?.path} in the file manager.`}
                            className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                          >
                            Open directory
                          </button>
                        )}
                        <button
                          onClick={() => void runAlign(row.session_id)}
                          disabled={disabled}
                          title={
                            aligningFor === row.session_id
                              ? 'Aligning window by window — the Processing panel above shows how far it has got. Stopping the server or cancelling keeps every window already aligned.'
                              : 'Recompute the word timings with wav2vec2 forced alignment and replace this session with the refined ones. Windows aligned by an earlier run are reused, so this finishes an interrupted alignment instead of restarting it. Needs the cached transcript — a session built from a hand-made subtitle has none.'
                          }
                          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                        >
                          {aligningFor === row.session_id ? 'Aligning…' : 'Align'}
                        </button>
                        <button
                          onClick={() => void runPlatformSubtitle(row.session_id)}
                          disabled={disabled || platformState === 'running'}
                          title={
                            platformState === 'running'
                              ? 'Reading the dubbed voice track to re-time every line — minutes on a long video.'
                              : platformState === 'failed'
                                ? platform?.error ?? 'Retry the export.'
                                : platformState === 'done'
                                  ? `Rewrite ${platform?.path} with a fresh export.`
                                  : row.dubbed
                                    ? 'Export a short-cue .srt beside the video, re-timed from the dubbed voice track.'
                                    : 'Export a short-cue .srt beside the video — the timings are already correct, so no transcription is needed.'
                          }
                          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                        >
                          {platformState === 'running'
                            ? 'Subtitle…'
                            : platformState === 'failed'
                              ? 'Retry subtitle'
                              : platformState === 'done'
                                ? 'Re-subtitle'
                                : 'Subtitle'}
                        </button>
                        {platformState === 'done' && (
                          <button
                            onClick={() => void openSubtitleFolder(row.session_id)}
                            disabled={disabled}
                            title={`Open the server folder holding ${platform?.path} in the file manager.`}
                            className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                          >
                            Open subtitle folder
                          </button>
                        )}
                        {transcodeState !== 'none' && (
                          <button
                            onClick={() => void reTranscode(row.session_id)}
                            disabled={disabled || transcodeState === 'running'}
                            title="Discard the cached H.264 copy and re-encode it."
                            className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted disabled:opacity-50"
                          >
                            Re-transcode
                          </button>
                        )}
                        <Link
                          to={`/player?session=${encodeURIComponent(row.session_id)}`}
                          className="rounded-md border px-3 py-1.5 text-xs font-medium transition-colors hover:bg-muted"
                        >
                          Open in player
                        </Link>
                        <button
                          onClick={() => void remove(row.session_id)}
                          disabled={disabled}
                          title="Delete the session, its voice track and its checkpoints."
                          className="rounded-md border border-destructive/40 px-3 py-1.5 text-xs font-medium text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-50"
                        >
                          Delete
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}