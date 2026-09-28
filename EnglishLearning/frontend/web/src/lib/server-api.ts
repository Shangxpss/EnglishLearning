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
 * The file is the state — `done` means it exists — so there is nothing to poll:
 * the mux is synchronous and the ending state comes back from the POST.
 */
export interface MuxedState {
  state: "idle" | "done";
  /** Absolute path of the file on the server machine, or `null` until muxed. */
  path: string | null;
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
