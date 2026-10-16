/**
 * Thin client for the Rust `sentence-video` server (rust/src/app.rs).
 *
 * Only the endpoints the Rust binary actually implements belong here. The
 * helpers shared by more than one page live in this module rather than being
 * copied into each feature.
 */

/** One sentence that never produced usable audio. */
export interface DubFailure {
  index: number;
  text: string;
  error: string;
}

/**
 * Dubbing progress. `state` is `idle` until a run is asked for, `done` once a
 * complete track exists, and `failed` when some — or all — sentences failed.
 * A `failed` run can always be resumed: the sentences already on disk are not
 * sent to the engine again.
 */
export interface DubbingState {
  state: "idle" | "running" | "done" | "failed";
  stage: "synth" | "stitch";
  /** Sentences usable so far — reused from disk, or synthesized this run. */
  done: number;
  total: number;
  failed: DubFailure[];
  error: string | null;
}

/** How the source video stream fares in a browser. */
export interface VideoStatus {
  codec: string | null;
  /** False while the server re-encodes a codec this browser cannot decode. */
  playable: boolean;
  transcode: "none" | "idle" | "running" | "done" | "failed";
  error?: string;
}

/**
 * The muxed video: this session's video stream with the replacement voice track
 * as its audio, written to its own folder on the server.
 *
 * A finished mux leaves a file, so `done` survives a restart with nothing else
 * to persist. Every finished dub muxes itself in the background, so a session
 * reports `running` for a few seconds after dubbing ends and the player keeps
 * polling until it settles.
 */
export interface MuxedState {
  state: "idle" | "running" | "done" | "failed";
  /** Absolute path of the file on the server machine, or `null` until muxed. */
  path: string | null;
  /** Why the auto-mux failed, when `state` is `failed`. */
  error?: string;
}

/**
 * The exported "video-platform" subtitle: short cues written as `<stem>.srt`
 * beside the source video, separate from the `_full_sentence` transcript the
 * app segments with.
 *
 * The file is the state — `done` means it exists — but a dubbed export needs a
 * fresh transcription of the dubbed audio, which takes minutes, so the POST
 * returns `running` and the caller polls `GET` until it leaves that state.
 */
export interface PlatformSubtitleState {
  state: "idle" | "running" | "done" | "failed";
  /** Absolute path of the `.srt` on the server machine, or `null` unless done. */
  path: string | null;
  /** Why the export failed, when `state` is `failed`. */
  error?: string;
}

/**
 * A media file the server is processing right now (`GET /api/builds`).
 *
 * Adding a video is synchronous: the session row is only written when
 * transcription *and* alignment are done, which for a subtitle-free video is the
 * entire cost of the pipeline. So during a build this is the only record that
 * the video was submitted at all.
 */
export interface BuildProgress {
  media_path: string;
  /** `decode` → `transcribe` → `align`. */
  stage: "decode" | "transcribe" | "align";
  /** Alignment windows done. Always 0 of 0 for the two earlier stages. */
  done: number;
  total: number;
  /** Seconds since the build started, so "working" can be told from "stuck". */
  elapsed_secs: number;
}

/** `GET /api/sessions` — every stored session, newest first. */
export interface SessionSummary {
  session_id: string;
  media_path: string;
  media_duration: number;
  cue_count: number;
  /** Last sentence played, or -1 when the session was never opened. */
  last_index: number;
  /** Last playback position in seconds. */
  position: number;
  updated_at: number;
  /** A replacement voice track exists for this session. */
  dubbed?: boolean;
  dubbing?: DubbingState;
  /** Whether the video has already been muxed with that voice track. */
  muxed?: MuxedState;
  /** Whether the short-cue `.srt` for a video platform has been exported. */
  platform_subtitle?: PlatformSubtitleState;
}

/** `GET /api/session/{id}` — the full session: sentences, video, dub state. */
export interface SessionDetail {
  id: string;
  media_path: string;
  media_duration: number;
  source_format: string;
  cues: { index: number; start: number; end: number; text: string }[];
  dubbed?: boolean;
  dubbing?: DubbingState;
  video?: VideoStatus;
  muxed?: MuxedState;
  platform_subtitle?: PlatformSubtitleState;
}

/** The Rust server reports failures as `{ "error": "..." }`. */
export async function readError(resp: Response): Promise<string> {
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
export async function pickFileOnServer(
  kind: "media" | "subtitle",
): Promise<string | null> {
  const resp = await fetch(`/api/pick-file?kind=${kind}`, { method: "POST" });
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { path: string | null } = await resp.json();
  return data.path ?? null;
}

/** `GET /api/session/{id}/dub` — the current state of the dubbing job. */
export async function getDub(sessionId: string): Promise<DubbingState> {
  const resp = await fetch(`/api/session/${encodeURIComponent(sessionId)}/dub`);
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/**
 * `POST /api/session/{id}/dub` — start, or resume, the voice track.
 *
 * Idempotent while a run is in flight, and restartable after one failed: the
 * server reuses every sentence already checkpointed, so a resume only redoes
 * what did not work. `retryFailed` forces a redo even when a complete track
 * already exists.
 */
export async function startDub(
  sessionId: string,
  opts: { engine?: string; retryFailed?: boolean } = {},
): Promise<DubbingState> {
  const body: Record<string, string | boolean> = {};
  if (opts.engine) body.engine = opts.engine;
  if (opts.retryFailed) body.retry_failed = true;
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/dub`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  );
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `GET /api/session/{id}` — the session with its sentences and video status. */
export async function getSession(sessionId: string): Promise<SessionDetail> {
  const resp = await fetch(`/api/session/${encodeURIComponent(sessionId)}`);
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `GET /api/sessions` — every stored session, newest first. */
export async function listSessions(): Promise<SessionSummary[]> {
  const resp = await fetch("/api/sessions");
  if (!resp.ok) throw new Error(await readError(resp));
  const data: SessionSummary[] = await resp.json();
  return Array.isArray(data) ? data : [];
}

/**
 * `GET /api/builds` — the media being processed right now, longest first.
 *
 * Safe to poll: it reads an in-memory registry of builds, not the filesystem.
 */
export async function listBuilds(): Promise<BuildProgress[]> {
  const resp = await fetch("/api/builds");
  if (!resp.ok) throw new Error(await readError(resp));
  const data: BuildProgress[] = await resp.json();
  return Array.isArray(data) ? data : [];
}

export interface CreateSessionBody {
  media_path: string;
  subtitle_path?: string;
  align?: boolean;
  dub?: boolean;
  dub_engine?: string;
}

/**
 * `POST /api/session` — process a media file (transcribe if needed) and store
 * it. Dubbing, when requested, keeps running on the server afterwards; poll
 * `/api/session/{id}/dub` rather than waiting for this call.
 */
export async function createSession(
  body: CreateSessionBody,
): Promise<{ session_id: string }> {
  const resp = await fetch("/api/session", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `DELETE /api/session/{id}` — remove one session and its generated files. */
export async function deleteSession(sessionId: string): Promise<void> {
  const resp = await fetch(`/api/session/${encodeURIComponent(sessionId)}`, {
    method: "DELETE",
  });
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * `POST /api/session/{id}/retranscode` — throw away the cached H.264 copy and
 * re-encode it. Without this a bad transcode would be permanent, because the
 * server only ever re-encodes when no copy exists.
 */
export async function retranscode(sessionId: string): Promise<void> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/retranscode`,
    {
      method: "POST",
    },
  );
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * `POST /api/session/{id}/align` — re-run wav2vec2 forced alignment for this
 * session and replace it with the refined word timings.
 *
 * Synchronous and minutes long, like adding a video: the alignment windows
 * already computed are reused, so this resumes an interrupted run instead of
 * restarting it, and the server registers it as a build — poll `/api/builds`
 * for the window counter while the call is in flight. A 409 means there is no
 * cached transcript to resume from.
 */
export async function alignSession(sessionId: string): Promise<void> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/align`,
    {
      method: "POST",
    },
  );
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * `POST /api/session/{id}/mux` — write the video out with the replacement voice
 * track as its audio, into the server's `muxed/` folder, and return where it
 * landed.
 *
 * Unlike dubbing this does not return before the work is done — the video is
 * copied and only the audio is encoded, so it is seconds, not minutes.
 */
export async function muxVideo(sessionId: string): Promise<MuxedState> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/mux`,
    {
      method: "POST",
    },
  );
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/**
 * `POST /api/builds/cancel` — ask the server to stop processing one media file.
 *
 * Keyed by media path: a build has no session id until it ends, which is also
 * why the Processing panel is the only place it can be cancelled from. The
 * answer is immediate but the work is not — the build stops at its next stage
 * boundary, because a window of alignment already running cannot be cut short.
 * A 404 means it finished before the click landed.
 */
export async function cancelBuild(mediaPath: string): Promise<void> {
  const resp = await fetch("/api/builds/cancel", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ media_path: mediaPath }),
  });
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * `POST /api/session/{id}/open-muxed-dir` — reveal the folder holding this
 * session's muxed video in the server machine's file manager.
 *
 * The server and the browser are on the same machine in the default setup, so
 * this is what "show me where the file went" means: the browser cannot open a
 * local folder itself. A 404 means there is no muxed video to reveal yet.
 *
 * The muxed file itself is still served at `GET …/muxed`; nothing in the UI
 * downloads it any more, so only the folder is reachable from here.
 */
export async function openMuxedDir(sessionId: string): Promise<void> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/open-muxed-dir`,
    { method: "POST" },
  );
  if (!resp.ok) throw new Error(await readError(resp));
}

/**
 * `POST /api/session/{id}/platform-subtitle` — start (or redo) the export of the
 * short-cue `.srt` for a video platform. For a dubbed session this transcribes
 * the dubbed audio first, so the returned state is `running`; poll
 * [`getPlatformSubtitle`] until it leaves that state.
 */
export async function generatePlatformSubtitle(
  sessionId: string,
): Promise<PlatformSubtitleState> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/platform-subtitle`,
    { method: "POST" },
  );
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `GET /api/session/{id}/platform-subtitle` — the export's current state. */
export async function getPlatformSubtitle(
  sessionId: string,
): Promise<PlatformSubtitleState> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/platform-subtitle`,
  );
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/**
 * `POST /api/session/{id}/open-subtitle-dir` — reveal the folder holding the
 * exported platform subtitle (the source video's own folder) in the server
 * machine's file manager. Same reasoning and guard as `openMuxedDir`.
 */
export async function openPlatformDir(sessionId: string): Promise<void> {
  const resp = await fetch(
    `/api/session/${encodeURIComponent(sessionId)}/open-subtitle-dir`,
    { method: "POST" },
  );
  if (!resp.ok) throw new Error(await readError(resp));
}

// ── Log database (logs.db) ────────────────────────────────────────────────
//
// Every request the server handles and every background job it runs is written
// to a separate `logs.db`, so a problem noticed later can still be traced. The
// Logs page reads it through these three calls; nothing here is required for
// playback, so a server built before the logging system simply 404s.

export type LogLevel = "debug" | "info" | "warn" | "error";

/** One persisted log line (`GET /api/logs`). */
export interface LogRecord {
  id: number;
  /** Unix epoch milliseconds. */
  ts_ms: number;
  level: LogLevel;
  /** Short subsystem name: `http`, `build`, `asr`, `align`, `dub`, `mux`, … */
  category: string;
  message: string;
  session_id?: string;
  media_path?: string;
  /** Extra context — an error body, a window span, a per-sentence duration. */
  detail?: string;
  duration_ms?: number;
}

/** Aggregate counts for the Logs page header (`GET /api/logs/stats`). */
export interface LogStats {
  total: number;
  oldest_ms: number | null;
  newest_ms: number | null;
  by_level: Record<string, number>;
  by_category: Record<string, number>;
}

/** Filters for `GET /api/logs`. Every field is optional. */
export interface LogQuery {
  level?: string;
  category?: string;
  /** Match `session_id` exactly. */
  session?: string;
  /** Substring of `media_path`. */
  media?: string;
  /** Substring of message / detail. */
  q?: string;
  /** Only records at or after this epoch-ms (used to tail new lines). */
  since_ms?: number;
  limit?: number;
}

/** One page of logs plus the header counts, from a single round trip. */
export interface LogPage {
  records: LogRecord[];
  total: number;
  stats: LogStats;
}

/** `GET /api/logs` — newest first, filtered by any of [`LogQuery`]. */
export async function getLogs(query: LogQuery = {}): Promise<LogPage> {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== null && value !== "") {
      params.set(key, String(value));
    }
  }
  const qs = params.toString();
  const resp = await fetch(`/api/logs${qs ? `?${qs}` : ""}`);
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `GET /api/logs/stats` — counts per level and per category. */
export async function getLogStats(): Promise<LogStats> {
  const resp = await fetch("/api/logs/stats");
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `DELETE /api/logs` — wipe the log table; returns how many rows went. */
export async function clearLogs(): Promise<number> {
  const resp = await fetch("/api/logs", { method: "DELETE" });
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { cleared: number } = await resp.json();
  return data.cleared ?? 0;
}

// ── Job queue (rust/src/jobs) ─────────────────────────────────────────────
//
// Submitting a batch is asynchronous: the POST returns a handle per file in
// milliseconds, the work runs on bounded server threads, and the outcome
// arrives over `GET /api/jobs/events` as Server-Sent Events. That is why the
// queue page can push a folder of clips and walk away.
//
// The SSE feed is **at-least-once** (it replays the whole snapshot on connect
// and again after a reconnect), so the same `(job_id, state)` can be delivered
// more than once — consumers dedupe on that pair.

export type JobState = "queued" | "running" | "done" | "failed" | "cancelled";

/** A job that has finished, one way or another. */
export const JOB_TERMINAL_STATES: JobState[] = ["done", "failed", "cancelled"];

/** One row of `GET /api/jobs`. */
export interface Job {
  id: string;
  media_path: string;
  kind: string;
  state: JobState;
  /** Where a running build is: `decode` → `transcribe` → `align`. */
  stage: string | null;
  done: number;
  total: number;
  attempts: number;
  max_attempts: number;
  last_error: string | null;
  /** Set once the job produced a session, so a finished job is deep-linkable. */
  session_id: string | null;
  /** Unix epoch seconds. */
  enqueued_at: number;
  started_at: number | null;
  finished_at: number | null;
  elapsed_secs: number;
}

/** The payload the SSE feed pushes — ids + a state; details come from `GET /api/jobs`. */
export interface JobEvent {
  /** Payload version, so a shape change is negotiable. */
  v: number;
  job_id: string;
  kind: string;
  state: JobState;
  media_path: string;
  session_id?: string;
  stage?: string;
  done: number;
  total: number;
  attempts: number;
  error?: string;
  finished_at: number;
}

/**
 * Ask the server to open the native OS file dialog for **several** files at
 * once and hand back their absolute paths. Returns an empty list when the user
 * cancels.
 */
export async function pickMediaFilesOnServer(): Promise<string[]> {
  const resp = await fetch(`/api/pick-file?kind=media&multi=1`, {
    method: "POST",
  });
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { paths: string[] } = await resp.json();
  return data.paths ?? [];
}

/** What `POST /api/jobs` reports back per file. */
export interface SubmitJobsResult {
  /** Accepted files and the handle to watch them with. */
  jobs: { job_id: string; media_path: string }[];
  /**
   * Files refused individually rather than failing the whole batch — a path
   * that is missing, say. One bad file must not cost the other nine.
   */
  rejected: { media_path: string; error: string }[];
}

/**
 * `POST /api/jobs` — push a batch onto the queue. Returns `202` immediately, so
 * the caller polls or listens for the outcome instead of blocking.
 *
 * Re-submitting a file that is already queued or running returns that job's
 * existing id rather than duplicating it.
 */
export async function submitJobs(
  mediaPaths: string[],
): Promise<SubmitJobsResult> {
  const resp = await fetch("/api/jobs", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ media_paths: mediaPaths }),
  });
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/** `GET /api/jobs` — every job, newest first (`state` / `kind` / `limit` narrow it). */
export async function listJobs(
  filter: { state?: JobState | ""; kind?: string; limit?: number } = {},
): Promise<Job[]> {
  const params = new URLSearchParams();
  if (filter.state) params.set("state", filter.state);
  if (filter.kind) params.set("kind", filter.kind);
  if (filter.limit) params.set("limit", String(filter.limit));
  const qs = params.toString();
  const resp = await fetch(`/api/jobs${qs ? `?${qs}` : ""}`);
  if (!resp.ok) throw new Error(await readError(resp));
  const data: { jobs: Job[] } = await resp.json();
  return data.jobs ?? [];
}

/**
 * `POST /api/jobs/{id}/cancel` — a queued job stops outright; a running one
 * stops at its next stage boundary, because a whisper pass or an alignment
 * window cannot be cut in half. The state it is in right now is returned.
 */
export async function cancelJob(
  jobId: string,
): Promise<{ job_id: string; state: JobState }> {
  const resp = await fetch(`/api/jobs/${encodeURIComponent(jobId)}/cancel`, {
    method: "POST",
  });
  if (!resp.ok) throw new Error(await readError(resp));
  return resp.json();
}

/**
 * Subscribe to `GET /api/jobs/events`. Every open tab gets the current snapshot
 * first, then live updates, so no polling is needed to see a job change state.
 *
 * Returns the unsubscribe function; call it on unmount, or the server keeps a
 * stream open for a page nobody is looking at.
 */
export function subscribeJobEvents(handlers: {
  onEvent: (event: JobEvent) => void;
  onError?: (message: string) => void;
}): () => void {
  const source = new EventSource("/api/jobs/events");
  const listener = (msg: MessageEvent<string>) => {
    try {
      handlers.onEvent(JSON.parse(msg.data) as JobEvent);
    } catch {
      // One malformed frame is not worth tearing the stream down for.
    }
  };
  source.addEventListener("job", listener as EventListener);
  source.onerror = () => {
    // EventSource reconnects on its own after a transient drop; only a
    // permanently closed stream is worth reporting.
    if (source.readyState === EventSource.CLOSED) {
      handlers.onError?.("the job notification stream is closed");
    }
  };
  return () => {
    source.removeEventListener("job", listener as EventListener);
    source.close();
  };
}

