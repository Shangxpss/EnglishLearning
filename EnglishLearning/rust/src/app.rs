//! Application state + request routing.

use crate::db::{Progress, SourceKey, Store};
use crate::logs;
use crate::media;
use crate::models::{Cue, Session};
use crate::pipeline::checkpoint;
use crate::pipeline::{synthesize, transcribe};
use crate::server::{Handler, Request, Response};
use crate::subtitle;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLES_CSS: &str = include_str!("../assets/styles.css");

/// Bump when the H.264 transcode recipe changes: it is part of the cache key,
/// so copies made by an older recipe are re-made instead of reused.
const PLAYABLE_RECIPE: &str = "h264-crf23-v2";

/// File inside a session's checkpoint directory listing the sentences a run
/// could not synthesize. It lives with the checkpoints on purpose: a run that
/// finally succeeds deletes that whole directory, while a partial run keeps
/// both, so the failure is still there to report and to retry after a restart.
const FAILURES_FILE: &str = "failures.json";

/// One sentence that never produced usable audio, kept so the Manage page can
/// name it without looking up the cue again — and because the session may be
/// gone from the database by the time the job is read.
///
/// Serialized into the checkpoint directory when a run ends with failures, so
/// the failure survives a server restart: the in-memory job map does not, and a
/// track that silently omits a sentence must not pass for a complete one.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DubFailure {
    index: usize,
    text: String,
    error: String,
}

/// The state of one session's dubbing job.
///
/// Unlike [`AppState::playable_jobs`], which only records presence, this is a
/// real state machine: a `failed` job must stay readable **and** restartable, so
/// the guard is [`DubJob::is_running`], never `contains_key`.
enum DubJob {
    /// A run is in flight. `stage` is `"synth"` while sentences are being
    /// rendered and `"stitch"` while the track is being assembled.
    Running {
        stage: &'static str,
        done: usize,
        total: usize,
    },
    /// A finished run. `failed` is empty on a clean run; a partial run keeps the
    /// per-sentence failures and is reported as `"failed"`, which is exactly
    /// what makes the next run resume only those sentences.
    Finished {
        synthesized: usize,
        total: usize,
        failed: Vec<DubFailure>,
    },
    /// The run could not produce a track at all (e.g. every sentence failed).
    Failed { error: String },
}

impl DubJob {
    /// The only guard that may block a new run: a run already in flight.
    fn is_running(&self) -> bool {
        matches!(self, DubJob::Running { .. })
    }

    /// Whether this entry means "just report the state, do not start a run".
    ///
    /// Only a run in flight blocks. A run that *recorded failures* must stay
    /// restartable — that is the whole point of recording them — and so must a
    /// run that produced no track at all. A complete track is only redone when
    /// the caller explicitly asks for it (`retry_failed`).
    fn blocks_restart(&self, retry_failed: bool, track_exists: bool) -> bool {
        match self {
            DubJob::Running { .. } => true,
            DubJob::Finished { failed, .. } if !failed.is_empty() => false,
            DubJob::Failed { .. } => false,
            _ => !retry_failed && track_exists,
        }
    }

    /// The `GET /api/session/{id}/dub` payload. `total_hint` fills in the
    /// sentence count for the variants that do not carry one.
    fn to_json(&self, total_hint: usize) -> serde_json::Value {
        match self {
            DubJob::Running { stage, done, total } => serde_json::json!({
                "state": "running",
                "stage": stage,
                "done": done,
                "total": total,
                "failed": [],
                "error": serde_json::Value::Null,
            }),
            DubJob::Finished {
                synthesized,
                total,
                failed,
            } => {
                let state = if failed.is_empty() { "done" } else { "failed" };
                serde_json::json!({
                    "state": state,
                    "stage": "stitch",
                    "done": synthesized,
                    "total": total,
                    "failed": failed.iter().map(|f| serde_json::json!({
                        "index": f.index,
                        "text": f.text,
                        "error": f.error,
                    })).collect::<Vec<_>>(),
                    "error": serde_json::Value::Null,
                })
            }
            DubJob::Failed { error } => serde_json::json!({
                "state": "failed",
                "stage": "synth",
                "done": 0,
                "total": total_hint,
                "failed": [],
                "error": error,
            }),
        }
    }
}

/// A session build in flight.
///
/// A build is **synchronous**: `POST /api/session` only returns when the session
/// exists, and the session row only reaches the database at the very end. For a
/// video with no subtitle that means whisper plus wav2vec2 alignment run before
/// anything is visible anywhere — minutes of CPU with no session id to poll, so
/// the video looks like it was never submitted. This is what lets the Manage
/// page show the stage and the elapsed time instead of an empty table.
struct BuildProgress {
    media_path: String,
    stage: transcribe::BuildStage,
    /// When the build started, so the caller can tell "working" from "stuck"
    /// without knowing how long each stage is supposed to take.
    started: std::time::Instant,
    /// Set by `POST /api/builds/cancel`. The build reads it through its sink at
    /// every stage boundary — the sink is `Sync` and is called from the aligner's
    /// worker threads, so the flag has to be shared rather than borrowed.
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl BuildProgress {
    /// Whether the user asked this build to stop. `Relaxed` is enough: the flag
    /// carries no data, and a check a few windows late changes nothing.
    fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn to_json(&self) -> serde_json::Value {
        // Alignment is the only stage with a countable unit (windows); the other
        // two report no sub-progress, so they carry a zero total.
        let (stage, done, total) = match self.stage {
            transcribe::BuildStage::Decode => ("decode", 0, 0),
            transcribe::BuildStage::Transcribe => ("transcribe", 0, 0),
            transcribe::BuildStage::Align { done, total } => ("align", done, total),
        };
        serde_json::json!({
            "media_path": self.media_path,
            "stage": stage,
            "done": done,
            "total": total,
            "elapsed_secs": self.started.elapsed().as_secs(),
        })
    }
}

pub struct AppState {
    /// Hot cache of sessions; the SQLite database is the source of truth.
    sessions: Mutex<HashMap<String, Session>>,
    cache_dir: std::path::PathBuf,
    /// App data directory — where generated voice tracks are persisted.
    data_dir: std::path::PathBuf,
    /// In-flight H.264 transcodes, keyed by media path: `"running"` or the
    /// failure reason (`"failed: …"`). A missing entry with no cached file
    /// means "not started yet".
    playable_jobs: Arc<Mutex<HashMap<String, String>>>,
    /// Mux jobs, keyed by media path: `"running"` or the failure reason
    /// (`"failed: …"`). A missing entry with no muxed file means "not muxed".
    /// Every finished dub muxes itself, so the player can stream one file.
    mux_jobs: Arc<Mutex<HashMap<String, String>>>,
    /// Dubbing jobs, keyed by session id. A missing entry means "never asked".
    dub_jobs: Arc<Mutex<HashMap<String, DubJob>>>,
    /// Platform-subtitle exports in flight, keyed by session id: `"running"` or
    /// the failure reason (`"failed: …"`). A finished export needs no entry —
    /// the `.srt` file beside the video *is* the result.
    platform_jobs: Arc<Mutex<HashMap<String, String>>>,
    /// Session builds in flight, keyed by media path. A missing entry means "not
    /// being built" — this map is only ever populated *during* a build, because
    /// a build has no session id to be keyed by until it finishes.
    build_jobs: Arc<Mutex<HashMap<String, BuildProgress>>>,
    /// Injected persistence port; the concrete backend is chosen at startup.
    db: Arc<dyn Store>,
}

impl AppState {
    pub fn new(
        cache_dir: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        db: Arc<dyn Store>,
    ) -> Self {
        std::fs::create_dir_all(&cache_dir).ok();
        AppState {
            sessions: Mutex::new(HashMap::new()),
            cache_dir,
            data_dir,
            playable_jobs: Arc::new(Mutex::new(HashMap::new())),
            mux_jobs: Arc::new(Mutex::new(HashMap::new())),
            dub_jobs: Arc::new(Mutex::new(HashMap::new())),
            platform_jobs: Arc::new(Mutex::new(HashMap::new())),
            build_jobs: Arc::new(Mutex::new(HashMap::new())),
            db,
        }
    }

    /// Where the browser-playable (H.264) copy of this media lives, or `None`
    /// when no copy is needed — the video codec already plays in a browser, or
    /// the file has no video stream at all.
    ///
    /// Keyed by the media path plus its size and mtime, so replacing or editing
    /// the file yields a new entry rather than a stale picture.
    fn playable_path(&self, media_path: &str) -> Option<std::path::PathBuf> {
        playable_copy_path(&self.data_dir, media_path)
    }

    /// Where this media's transcription/alignment checkpoints live.
    ///
    /// Keyed exactly like [`Self::playable_path`] — media path plus size and
    /// mtime — so re-encoding or replacing the file starts from a clean
    /// directory instead of reusing another file's windows.
    fn work_dir(&self, media_path: &str) -> Option<std::path::PathBuf> {
        let key = media_cache_key(media_path)?;
        Some(
            self.data_dir
                .join("work")
                .join(format!("{:016x}", checkpoint::fnv1a(&key))),
        )
    }

    /// Register a build of `media_path` and return the sink that keeps its stage
    /// current and answers whether the build should keep going. Dropped by
    /// [`Self::end_build`] on every exit path.
    ///
    /// The entry starts at [`transcribe::BuildStage::Decode`] — the stage a build
    /// is always in immediately, since decoding is the first thing it does.
    fn begin_build(&self, media_path: &str) -> impl Fn(transcribe::BuildStage) -> bool + Sync {
        let jobs = Arc::clone(&self.build_jobs);
        let key = media_path.to_string();
        // One flag per build, handed both to the map (so the cancel endpoint can
        // reach it) and to the sink (so the pipeline can read it).
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        jobs.lock().unwrap().insert(
            key.clone(),
            BuildProgress {
                media_path: key.clone(),
                stage: transcribe::BuildStage::Decode,
                started: std::time::Instant::now(),
                cancel: Arc::clone(&cancel),
            },
        );
        move |stage| {
            // An entry that is gone means the build already finished, so a late
            // update from a worker thread is not an error — just ignored.
            if let Ok(mut map) = jobs.lock() {
                if let Some(entry) = map.get_mut(&key) {
                    entry.stage = stage;
                }
            }
            !cancel.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Stop reporting a build. Called on every exit path — including the error
    /// one — so a failed build never leaves a phantom row behind.
    fn end_build(&self, media_path: &str) {
        self.build_jobs.lock().unwrap().remove(media_path);
    }

    /// Where this session's per-sentence TTS checkpoints live.
    ///
    /// Keyed by session id, not by the media hash: editing the `.srt` does not
    /// change the media's mtime, but it does produce a new session id — so an
    /// edited sentence can never reuse the previous audio.
    fn tts_dir(&self, session_id: &str) -> std::path::PathBuf {
        self.data_dir
            .join("dubbed")
            .join(format!("{session_id}.tts"))
    }

    /// How many of this session's sentences already have a checkpoint on disk,
    /// i.e. how much of a run is already done. Reported by the idle `dubbing`
    /// state so the Manage page can show "17 of 23 ready" before a resume.
    fn cached_sentence_count(&self, session: &Session) -> usize {
        let dir = self.tts_dir(&session.id);
        session
            .cues
            .iter()
            .filter(|cue| {
                synthesize::sentence_checkpoint_path(&dir, cue.index, &cue.text).is_file()
            })
            .count()
    }

    /// Sentences a previous run could not synthesize. Read from the checkpoint
    /// directory rather than the job map so the answer survives a restart.
    fn recorded_failures(&self, session_id: &str) -> Vec<DubFailure> {
        checkpoint::read_json(&self.tts_dir(session_id), FAILURES_FILE).unwrap_or_default()
    }

    /// The `dubbing` object the player and Manage page poll.
    fn dub_status(&self, session: &Session) -> serde_json::Value {
        let existing = self.dub_jobs.lock().unwrap().get(&session.id).map(|j| {
            let json = j.to_json(session.cues.len());
            (j.is_running(), json)
        });
        match existing {
            Some((_running, json)) => json,
            // No job object: either never asked, or the server restarted. What
            // the run left on disk is the durable record — the failures first,
            // because a track that silently omits a sentence is not "done".
            None => {
                let total = session.cues.len();
                let failed = self.recorded_failures(&session.id);
                if !failed.is_empty() {
                    return serde_json::json!({
                        "state": "failed",
                        "stage": "stitch",
                        "done": total.saturating_sub(failed.len()),
                        "total": total,
                        "failed": failed.iter().map(|f| serde_json::json!({
                            "index": f.index,
                            "text": f.text,
                            "error": f.error,
                        })).collect::<Vec<_>>(),
                        "error": serde_json::Value::Null,
                    });
                }
                let dubbed = self.is_dubbed(&session.id);
                serde_json::json!({
                    "state": if dubbed { "done" } else { "idle" },
                    "stage": if dubbed { "stitch" } else { "synth" },
                    "done": if dubbed { total } else { self.cached_sentence_count(session) },
                    "total": total,
                    "failed": [],
                    "error": serde_json::Value::Null,
                })
            }
        }
    }

    /// What the player needs to know about this media's video stream, so it can
    /// show a "preparing" placeholder instead of a black rectangle.
    fn video_status(&self, media_path: &str) -> serde_json::Value {
        let codec = match media::video_codec(media_path) {
            Ok(c) => c,
            Err(_) => None,
        };
        match codec {
            None => serde_json::json!({ "codec": null, "playable": true, "transcode": "none" }),
            Some(name) if media::browser_playable_video_codec(&name) => {
                serde_json::json!({ "codec": name, "playable": true, "transcode": "none" })
            }
            Some(name) => {
                let cached = self.playable_path(media_path).is_some_and(|p| p.is_file());
                let job = self.playable_jobs.lock().unwrap().get(media_path).cloned();
                let (state, error) = match (&job, cached) {
                    (_, true) => ("done", None),
                    (Some(s), _) if s == "running" => ("running", None),
                    (Some(s), _) => ("failed", Some(s.clone())),
                    (None, _) => ("idle", None),
                };
                let mut value =
                    serde_json::json!({ "codec": name, "playable": cached, "transcode": state });
                if let Some(e) = error {
                    value["error"] = serde_json::Value::String(e);
                }
                value
            }
        }
    }

    /// Start the H.264 transcode in the background unless a copy exists or a
    /// job is already in flight. Never blocks the request: re-encoding a
    /// 1080p lecture takes about a minute, and the player shows its placeholder
    /// until `video.transcode` reports `"done"`.
    fn ensure_playable_async(&self, media_path: &str) {
        let Some(dst) = self.playable_path(media_path) else {
            return; // nothing to do — the browser can play this one
        };
        if dst.is_file() {
            return;
        }
        {
            let mut jobs = self.playable_jobs.lock().unwrap();
            if jobs.contains_key(media_path) {
                return; // already running, or already failed
            }
            jobs.insert(media_path.to_string(), "running".to_string());
        }
        if let Some(dir) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                let msg = format!("failed: cannot create {}: {e}", dir.display());
                self.playable_jobs
                    .lock()
                    .unwrap()
                    .insert(media_path.to_string(), msg);
                return;
            }
        }
        let jobs = Arc::clone(&self.playable_jobs);
        let key = media_path.to_string();
        let src = media_path.to_string();
        std::thread::spawn(move || {
            println!("sentence-video: re-encoding '{src}' to H.264 so the browser can show it…");
            logs::info("transcode", "re-encoding the source to H.264")
                .media(&src)
                .send();
            let started = std::time::Instant::now();
            let result = media::transcode_to_h264(&src, &dst.to_string_lossy(), None);
            let mut jobs = jobs.lock().unwrap();
            match result {
                Ok(()) => {
                    jobs.remove(&key);
                    println!("sentence-video: browser-playable copy → {}", dst.display());
                    logs::info("transcode", "browser-playable copy written")
                        .media(&src)
                        .detail(dst.display().to_string())
                        .duration_ms(started.elapsed().as_millis() as i64)
                        .send();
                }
                Err(e) => {
                    eprintln!("sentence-video: re-encoding '{src}' failed: {e}");
                    logs::error("transcode", "re-encoding to H.264 failed")
                        .media(&src)
                        .detail(&e)
                        .duration_ms(started.elapsed().as_millis() as i64)
                        .send();
                    jobs.insert(key, format!("failed: {e}"));
                }
            }
        });
    }

    /// Where this session's replacement voice track lives, if one was made.
    ///
    /// Keyed by session id (not by media path): a session is rebuilt under a
    /// new id whenever the media or its subtitle changes, so the track can
    /// never go stale.
    fn dubbed_track_path(&self, session_id: &str) -> std::path::PathBuf {
        self.data_dir
            .join("dubbed")
            .join(format!("{session_id}.wav"))
    }

    fn is_dubbed(&self, session_id: &str) -> bool {
        self.dubbed_track_path(session_id).is_file()
    }

    /// Where this session's muxed video — the video stream with the replacement
    /// voice track as its audio — is written.
    ///
    /// Its own `muxed/` folder, named after the **source** file, so the result
    /// sits under a name the user already recognises: `lecture.mp4` muxes to
    /// `muxed/lecture.mp4`. The container is always MP4 (the video is copied and
    /// the audio re-encoded to AAC), so a `.mkv` or `.mov` source keeps its stem
    /// and gains an `.mp4` extension rather than being written in a format the
    /// browser cannot stream.
    ///
    /// One media file maps to exactly one muxed file: a second mux — the
    /// "Re-mux" button, or another session over the same media — overwrites the
    /// first instead of accumulating session-suffixed copies.
    fn muxed_path(&self, media_path: &str) -> std::path::PathBuf {
        muxed_file_path(&self.data_dir, media_path)
    }

    /// The `muxed` object the Manage page reads. A finished mux leaves a file, so
    /// it survives a server restart with nothing else to persist; a mux still in
    /// flight reports `running` from [`Self::mux_jobs`].
    fn muxed_state(&self, media_path: &str) -> serde_json::Value {
        let path = muxed_file_path(&self.data_dir, media_path);
        if path.is_file() {
            return serde_json::json!({
                "state": "done",
                "path": path.to_string_lossy(),
            });
        }
        let job = self.mux_jobs.lock().unwrap().get(media_path).cloned();
        match job.as_deref() {
            Some("running") => {
                serde_json::json!({ "state": "running", "path": serde_json::Value::Null })
            }
            Some(reason) => serde_json::json!({
                "state": "failed",
                "path": serde_json::Value::Null,
                "error": reason,
            }),
            None => serde_json::json!({ "state": "idle", "path": serde_json::Value::Null }),
        }
    }

    /// Start the mux in the background when a dubbed session has no muxed file
    /// yet and no job is in flight.
    ///
    /// Dubbing muxes itself the moment it finishes, so this only matters for a
    /// session whose voice track predates auto-mux — opening it should still end
    /// with one file to play rather than leaving the player on per-sentence
    /// clips. Never blocks the request.
    fn ensure_muxed_async(&self, session: &Session) {
        if !self.is_dubbed(&session.id) {
            return;
        }
        if self.muxed_path(&session.media_path).is_file() {
            return;
        }
        if self
            .mux_jobs
            .lock()
            .unwrap()
            .contains_key(&session.media_path)
        {
            return;
        }
        let data_dir = self.data_dir.clone();
        let media_path = session.media_path.clone();
        let track = self.dubbed_track_path(&session.id);
        let jobs = Arc::clone(&self.mux_jobs);
        std::thread::spawn(move || auto_mux(&data_dir, &media_path, &track, &jobs));
    }

    /// The `platform_subtitle` object the Manage page reads. As with `muxed`,
    /// the written `.srt` is the state, so a finished export survives a restart
    /// with nothing to persist; only a run in flight lives in memory.
    fn platform_subtitle_state(&self, session_id: &str, media_path: &str) -> serde_json::Value {
        let path = crate::platform::platform_subtitle_path(media_path);
        if std::path::Path::new(&path).is_file() {
            return serde_json::json!({ "state": "done", "path": path });
        }
        match self.platform_jobs.lock().unwrap().get(session_id) {
            Some(job) if job == "running" => {
                serde_json::json!({ "state": "running", "path": serde_json::Value::Null })
            }
            Some(job) => serde_json::json!({
                "state": "failed",
                "path": serde_json::Value::Null,
                "error": job.trim_start_matches("failed: "),
            }),
            None => serde_json::json!({ "state": "idle", "path": serde_json::Value::Null }),
        }
    }

    /// Start a platform-subtitle export and report the state to poll.
    ///
    /// The work goes to a worker thread even though the undubbed case finishes
    /// instantly: re-transcribing a dubbed lecture takes as long as the session
    /// took to build, so the two paths must look the same to the caller.
    fn start_platform_subtitle(&self, session: &Session) -> serde_json::Value {
        {
            let mut jobs = self.platform_jobs.lock().unwrap();
            if jobs
                .get(&session.id)
                .map(|j| j == "running")
                .unwrap_or(false)
            {
                drop(jobs);
                return self.platform_subtitle_state(&session.id, &session.media_path);
            }
            jobs.insert(session.id.clone(), "running".to_string());
        }

        // Everything the worker needs is owned: `self` cannot cross the thread
        // boundary, and the export is pure once the audio has been read.
        let track = self
            .is_dubbed(&session.id)
            .then(|| self.dubbed_track_path(&session.id));
        let cues = session.cues.clone();
        let media_path = session.media_path.clone();
        let jobs = Arc::clone(&self.platform_jobs);
        let id = session.id.clone();
        logs::info("platform", "exporting the video-platform subtitle")
            .session(&session.id)
            .media(&session.media_path)
            .detail(if track.is_some() {
                "source=dubbed voice"
            } else {
                "source=original audio"
            })
            .send();
        std::thread::spawn(move || {
            let result = write_platform_subtitle(&cues, track.as_deref(), &media_path);
            let mut jobs = jobs.lock().unwrap();
            match result {
                Ok(path) => {
                    jobs.remove(&id);
                    println!("sentence-video: platform subtitle → {path}");
                    logs::info("platform", "platform subtitle written")
                        .session(&id)
                        .media(&media_path)
                        .detail(path)
                        .send();
                }
                Err(e) => {
                    eprintln!("sentence-video: platform subtitle failed: {e}");
                    logs::error("platform", "platform subtitle export failed")
                        .session(&id)
                        .media(&media_path)
                        .detail(&e)
                        .send();
                    jobs.insert(id, format!("failed: {e}"));
                }
            }
        });
        self.platform_subtitle_state(&session.id, &session.media_path)
    }

    /// Start — or resume — this session's replacement voice track, in the
    /// background, and return the job state to report.
    ///
    /// The engine is probed **synchronously**, so a missing binary is still a
    /// `422` on the request that asked for it, exactly as before. Everything
    /// after that runs on a worker thread: re-rendering a long lecture takes
    /// tens of seconds and callers poll `GET …/dub` meanwhile. Sentences already
    /// checkpointed are not re-sent to the engine, so a second call finishes
    /// only what the first one could not.
    fn start_dub(
        &self,
        session: &Session,
        engine_override: Option<&str>,
        retry_failed: bool,
    ) -> Result<serde_json::Value, (u16, String)> {
        // Only a *running* job, or a complete track the caller did not ask to
        // redo, is a no-op. A run that recorded failures stays restartable —
        // that is the point of recording them — so it is never short-circuited.
        let just_report = {
            let jobs = self.dub_jobs.lock().unwrap();
            let track_exists = self.is_dubbed(&session.id);
            match jobs.get(&session.id) {
                Some(job) => job.blocks_restart(retry_failed, track_exists),
                // No job object (first run, or the server restarted): a track
                // with sentences still recorded as failed is incomplete, so it
                // restarts on its own — same rule as a `Finished { failed }`.
                None => {
                    !retry_failed && track_exists && self.recorded_failures(&session.id).is_empty()
                }
            }
        };
        if just_report {
            return Ok(self.dub_status(session));
        }

        let cfg = crate::pipeline::dub_config();
        let engine = engine_override.unwrap_or(&cfg.engine).to_string();
        let voice = synthesize::effective_voice(&engine, cfg.voice.as_deref());
        let backend = synthesize::build_backend(
            &engine,
            cfg.voice.as_deref(),
            cfg.rate_words_per_min,
            cfg.proxy.as_deref(),
        )
        .map_err(|e| (422, e))?;

        // A different engine or voice would silently reuse the previous voice's
        // clips, so the recipe is part of the checkpoint directory's identity.
        let tts = checkpoint::RecipeDir::open(
            self.tts_dir(&session.id),
            &format!("{engine}|{}", voice.unwrap_or("engine default")),
        )
        .map_err(|e| (500, e))?;

        let track = self.dubbed_track_path(&session.id);
        if let Some(dir) = track.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| (500, format!("cannot create {}: {e}", dir.display())))?;
        }

        let total = session.cues.len();
        self.dub_jobs.lock().unwrap().insert(
            session.id.clone(),
            DubJob::Running {
                stage: "synth",
                done: self.cached_sentence_count(session),
                total,
            },
        );
        println!(
            "sentence-video: dubbing '{}' with {engine} · voice {} ({total} sentences)",
            session.media_path,
            voice.unwrap_or("engine default")
        );
        logs::info("dub", "dubbing started")
            .session(&session.id)
            .media(&session.media_path)
            .detail(format!(
                "engine={engine} · voice={} · sentences={total}",
                voice.unwrap_or("engine default")
            ))
            .send();

        let jobs = Arc::clone(&self.dub_jobs);
        let mux_jobs = Arc::clone(&self.mux_jobs);
        let data_dir = self.data_dir.clone();
        let media_path = session.media_path.clone();
        let id = session.id.clone();
        let cues = session.cues.clone();
        let duration = session.media_duration;
        let tts_dir = tts.dir().to_path_buf();
        std::thread::spawn(move || {
            let on_progress = {
                let jobs = Arc::clone(&jobs);
                let id = id.clone();
                move |p: synthesize::SynthProgress| {
                    if let Some(DubJob::Running { stage, done, total }) =
                        jobs.lock().unwrap().get_mut(&id)
                    {
                        *done = p.done;
                        *total = p.total;
                        if p.done >= p.total {
                            *stage = "stitch";
                        }
                    }
                }
            };
            // Assembled under a `.part` name and renamed on success, so a
            // half-written track is never served as a finished one.
            let tmp = format!("{}.part", track.display());
            let result = synthesize::synthesize_track_checkpointed(
                &*backend,
                &cues,
                duration,
                &tmp,
                Some(&tts_dir),
                on_progress,
            );
            let outcome = match result {
                Ok(failures) => match std::fs::rename(&tmp, &track) {
                    Err(e) => DubJob::Failed {
                        error: format!("cannot move the track into place: {e}"),
                    },
                    Ok(()) if failures.failed.is_empty() => {
                        println!("sentence-video: dubbed audio → {}", track.display());
                        // Mirrors the reference's `rmtree(tts_dir)`: the
                        // per-sentence clips have served their purpose.
                        let _ = std::fs::remove_dir_all(&tts_dir);
                        DubJob::Finished {
                            synthesized: failures.synthesized,
                            total: cues.len(),
                            failed: Vec::new(),
                        }
                    }
                    Ok(()) => {
                        eprintln!(
                            "sentence-video: {} sentence(s) could not be dubbed and keep a \
                             silent slot; the checkpoints are kept so a retry redoes only those",
                            failures.failed.len()
                        );
                        let failed: Vec<DubFailure> = failures
                            .failed
                            .iter()
                            .map(|(index, error)| DubFailure {
                                index: *index,
                                text: cues.get(*index).map(|c| c.text.clone()).unwrap_or_default(),
                                error: error.clone(),
                            })
                            .collect();
                        // Recorded next to the kept checkpoints so the state
                        // outlives this process: a restart must not turn a
                        // track with missing sentences into a clean "dubbed".
                        if let Err(e) = checkpoint::write_json(&tts_dir, FAILURES_FILE, &failed) {
                            eprintln!("sentence-video: cannot record the failed sentences: {e}");
                        }
                        DubJob::Finished {
                            synthesized: failures.synthesized,
                            total: cues.len(),
                            failed,
                        }
                    }
                },
                Err(e) => {
                    eprintln!("sentence-video: dubbing '{}' failed: {e}", track.display());
                    DubJob::Failed { error: e }
                }
            };
            // Record how the run ended before handing the state to the poller.
            match &outcome {
                DubJob::Finished {
                    synthesized,
                    total,
                    failed,
                } if failed.is_empty() => {
                    logs::info("dub", "dubbing finished")
                        .session(&id)
                        .media(&media_path)
                        .detail(format!("synthesized={synthesized}/{total}"))
                        .send();
                }
                DubJob::Finished {
                    synthesized,
                    total,
                    failed,
                } => {
                    logs::warn(
                        "dub",
                        "dubbing finished with sentences that could not be dubbed",
                    )
                    .session(&id)
                    .media(&media_path)
                    .detail(format!(
                        "synthesized={synthesized}/{total} · failed={} · {}",
                        failed.len(),
                        failed
                            .iter()
                            .map(|f| format!("#{} {}", f.index, f.error))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ))
                    .send();
                }
                DubJob::Failed { error } => {
                    logs::error("dub", "dubbing failed")
                        .session(&id)
                        .media(&media_path)
                        .detail(error)
                        .send();
                }
                DubJob::Running { .. } => {}
            }
            jobs.lock().unwrap().insert(id, outcome);

            // One file to play: mux the replacement voice into the video so the
            // player streams a single track instead of keeping a separate voice
            // track in sync. Best-effort and on this background thread — a
            // failure leaves the player on its per-sentence clips.
            if track.is_file() {
                auto_mux(&data_dir, &media_path, &track, &mux_jobs);
            }
        });

        Ok(self.dub_status(session))
    }
}

/// Where the browser-playable (H.264) copy of `media_path` lives, or `None`
/// when no copy is needed — the video codec already plays in a browser, or the
/// file has no video stream at all.
///
/// The free-function form exists so the dubbing thread can work out the path
/// without an `AppState`; [`AppState::playable_path`] delegates here.
fn playable_copy_path(data_dir: &Path, media_path: &str) -> Option<std::path::PathBuf> {
    let codec = media::video_codec(media_path).ok().flatten()?;
    if media::browser_playable_video_codec(&codec) {
        return None;
    }
    // The recipe tag is part of the key so a copy produced by an older ffmpeg
    // invocation is re-made rather than reused.
    let key = format!("{}|{PLAYABLE_RECIPE}", media_cache_key(media_path)?);
    Some(
        data_dir
            .join("h264")
            .join(format!("{:016x}.mp4", checkpoint::fnv1a(&key))),
    )
}

/// Where this media's muxed video — the video stream with the replacement voice
/// track as its audio — is written: `muxed/<stem>.mp4`.
///
/// Its own folder, named after the **source** file, so the result sits under a
/// name the user already recognises. One media file maps to exactly one muxed
/// file, so a re-mux overwrites rather than accumulating copies.
fn muxed_file_path(data_dir: &Path, media_path: &str) -> std::path::PathBuf {
    let name = Path::new(media_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_string());
    data_dir.join("muxed").join(format!("{name}.mp4"))
}

/// Copy `media_path`'s video stream and the replacement voice `track` into one
/// MP4 under `data_dir/muxed/`, re-encoding only the audio.
///
/// Synchronous on purpose: the video stream is copied, not re-encoded, so this
/// is seconds where dubbing is minutes. The browser-playable H.264 copy is the
/// video source when one exists, so the result plays in a browser too.
fn mux_video_into(
    data_dir: &Path,
    media_path: &str,
    track: &Path,
) -> Result<std::path::PathBuf, String> {
    let video = playable_copy_path(data_dir, media_path)
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| media_path.to_string());
    let dst = muxed_file_path(data_dir, media_path);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // The output format is guessed from the file name, so the temp file has to
    // keep an `.mp4` extension — a bare `.part` suffix makes the muxer refuse
    // the path outright ("Invalid argument") before any work starts. A unique
    // id keeps two concurrent muxes of the same media off one scratch path.
    let tmp = dst.with_file_name(format!(
        "{}.{}.part.mp4",
        dst.file_stem().unwrap_or_default().to_string_lossy(),
        gen_id()
    ));
    let tmp = tmp.to_string_lossy().into_owned();
    println!(
        "sentence-video: muxing '{video}' + '{}' → {}",
        track.display(),
        dst.display()
    );
    logs::info("mux", "muxing the video and the dubbed voice")
        .media(media_path)
        .detail(format!(
            "video={video} · track={} · out={}",
            track.display(),
            dst.display()
        ))
        .send();
    let bitrate = crate::pipeline::DubOptions::default().audio_bitrate;
    if let Err(e) = media::mux_video_audio(
        &video,
        &track.to_string_lossy(),
        &tmp,
        bitrate,
        // The track is a full-length timeline, so trimming it to the video is
        // what a correct output needs — the same `-shortest` the CLI uses.
        true,
    ) {
        let _ = std::fs::remove_file(&tmp);
        logs::error("mux", "muxing failed")
            .media(media_path)
            .detail(&e)
            .send();
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, &dst) {
        let _ = std::fs::remove_file(&tmp);
        logs::error("mux", "cannot move the muxed file into place")
            .media(media_path)
            .detail(&e)
            .send();
        return Err(format!("cannot move the muxed file into place: {e}"));
    }
    println!("sentence-video: muxed video → {}", dst.display());
    logs::info("mux", "muxed video written")
        .media(media_path)
        .detail(dst.display().to_string())
        .send();
    Ok(dst)
}

/// Mux a freshly dubbed session so the player can stream one file (video +
/// replacement voice) instead of keeping a separate voice track in sync.
///
/// Best-effort and blocking (the caller runs on a background thread): a failure
/// is recorded in `mux_jobs` and the player simply falls back to its
/// per-sentence clips, so this never breaks a working session.
fn auto_mux(
    data_dir: &Path,
    media_path: &str,
    track: &Path,
    mux_jobs: &Mutex<HashMap<String, String>>,
) {
    if muxed_file_path(data_dir, media_path).is_file() {
        return;
    }
    // Claim the media path so two callers (a finished dub and a session being
    // opened) cannot mux it at once.
    {
        let mut jobs = mux_jobs.lock().unwrap();
        if jobs.get(media_path).map_or(false, |s| s == "running") {
            return;
        }
        jobs.insert(media_path.to_string(), "running".to_string());
    }
    // A source the browser cannot decode must be muxed from its H.264 copy —
    // muxing the original would just produce another unplayable file. That copy
    // is re-encoded at session build time and dubbing takes far longer, so it is
    // normally already there; wait briefly if it is not.
    if let Some(copy) = playable_copy_path(data_dir, media_path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !copy.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !copy.is_file() {
            mux_jobs.lock().unwrap().insert(
                media_path.to_string(),
                "failed: the browser-playable copy is not ready".to_string(),
            );
            logs::warn(
                "mux",
                "auto-mux gave up: the browser-playable copy is not ready",
            )
            .media(media_path)
            .send();
            return;
        }
    }
    match mux_video_into(data_dir, media_path, track) {
        Ok(_) => {
            mux_jobs.lock().unwrap().remove(media_path);
        }
        Err(e) => {
            eprintln!("sentence-video: auto-mux of '{media_path}' failed: {e}");
            mux_jobs
                .lock()
                .unwrap()
                .insert(media_path.to_string(), format!("failed: {e}"));
        }
    }
}

/// `path|size|mtime` — a media file's cache identity. Editing or replacing the
/// file at the same path yields a different key, so nothing computed from the
/// old bytes is reused.
fn media_cache_key(media_path: &str) -> Option<String> {
    let meta = std::fs::metadata(media_path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{media_path}|{}|{mtime}", meta.len()))
}

/// Identifies what the raw transcript depends on: the whisper model, the
/// language, and the aligner weights.
///
/// The `align` flag is deliberately excluded — the *words* do not depend on it,
/// and `POST /api/session` may override it per request, so including it would
/// throw away a perfectly good transcript. A change here clears the work
/// directory via [`checkpoint::RecipeDir`].
fn asr_recipe() -> String {
    let cfg = crate::asr::config();
    let model = cfg
        .model
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "embedded".to_string());
    let language = cfg.language.clone().unwrap_or_else(|| "en".to_string());
    format!("asr|{model}|{language}|{}", align_weights_recipe())
}

/// The aligner weights' part of [`asr_recipe`].
#[cfg(feature = "asr-align")]
fn align_weights_recipe() -> String {
    crate::align::default_weights_path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "none".to_string())
}

/// Without the aligner compiled in there is nothing to invalidate on.
#[cfg(not(feature = "asr-align"))]
fn align_weights_recipe() -> String {
    "none".to_string()
}

fn gen_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}",
        (nanos.rotate_left(32) as u64 ^ ((c as u64) << 40)) & 0xFFFFFFFFFFFFFFu64
    )
}

/// Paths that must NOT be logged per request. Video streaming issues a stream of
/// `Range` requests (one per seek/buffer) and the Logs page polls `/api/logs`
/// itself, so recording either would flood the database and — for `/api/logs` —
/// feed the page its own traffic. Static assets are omitted for the same reason.
fn should_log_request(path: &str) -> bool {
    const SKIP: &[&str] = &["/", "/index.html", "/app.js", "/styles.css", "/health"];
    if SKIP.contains(&path) {
        return false;
    }
    if path.starts_with("/api/logs") {
        return false;
    }
    // Media streams: the video, its audio, the dubbed voice, the muxed file and
    // per-sentence clips are all high-frequency Range reads.
    const STREAM_SUFFIXES: &[&str] = &["/media", "/dubbed-audio", "/muxed", "/audio"];
    if STREAM_SUFFIXES.iter().any(|s| path.ends_with(s)) {
        return false;
    }
    true
}

/// Severity for a completed request: server errors are `error`, client errors
/// are `warn`, everything else is `info`.
fn level_for_status(status: u16) -> logs::Level {
    if status >= 500 {
        logs::Level::Error
    } else if status >= 400 {
        logs::Level::Warn
    } else {
        logs::Level::Info
    }
}

impl Handler for AppState {
    /// The single chokepoint every HTTP request passes through: time the
    /// dispatch, then record it (except on the high-frequency paths listed in
    /// [`should_log_request`]) so the log database carries a request timeline.
    fn handle(&self, req: &Request) -> Response {
        let started = std::time::Instant::now();
        let res = self.dispatch(req);
        if should_log_request(&req.path) {
            let mut entry = logs::log(
                level_for_status(res.status),
                "http",
                format!("{} {} → {}", req.method, req.path, res.status),
            )
            .duration_ms(started.elapsed().as_millis() as i64);
            // The JSON error body is the most useful part of a failed request.
            if res.status >= 400 {
                if let Ok(body) = std::str::from_utf8(&res.body) {
                    if !body.is_empty() {
                        entry = entry.detail(body.to_string());
                    }
                }
            }
            entry.send();
        }
        res
    }
}

impl AppState {
    /// Match a request to its handler. Separated from [`Handler::handle`] so the
    /// logging wrapper can time and record every route from one place.
    fn dispatch(&self, req: &Request) -> Response {
        let path = req.path.as_str();
        match (req.method.as_str(), path) {
            ("GET", "/") | ("GET", "/index.html") => {
                bytes_ok(INDEX_HTML.as_bytes(), "text/html; charset=utf-8")
            }
            ("GET", "/app.js") => {
                bytes_ok(APP_JS.as_bytes(), "application/javascript; charset=utf-8")
            }
            ("GET", "/styles.css") => bytes_ok(STYLES_CSS.as_bytes(), "text/css; charset=utf-8"),
            ("GET", "/health") => Response::text(200, "application/json", r#"{"status":"ok"}"#),
            ("GET", "/api/sessions") => self.list_sessions(),

            // The log database: read, aggregate, or clear it.
            ("GET", "/api/logs") => self.list_logs(req),
            ("GET", "/api/logs/stats") => self.log_stats(),
            ("DELETE", "/api/logs") => self.clear_logs(),

            // Media being processed right now. A build has no session id until
            // it ends, so it cannot be reported by `/api/sessions`.
            ("GET", "/api/builds") => self.list_builds(),

            // Stop a build in flight. Keyed by media path because that is what a
            // build is identified by: it has no session id until it ends.
            ("POST", "/api/builds/cancel") => self.cancel_build(req),

            // Wipe every stored session (cues + progress cascade).
            ("DELETE", "/api/sessions") => self.delete_all_sessions(),

            ("POST", "/api/session") => self.create_session(req),

            // Opens the OS file dialog on the machine running this server and
            // returns the chosen absolute path (the browser cannot provide it).
            ("POST", "/api/pick-file") => self.pick_file(req),

            (m, p)
                if (m == "GET" || m == "POST" || m == "DELETE")
                    && p.starts_with("/api/session/") =>
            {
                self.route_session(p, req)
            }

            // SPA fallback: any other GET that is neither an API call nor a
            // static asset is a client-side route (e.g. `/player`, `/subtitle`).
            // Serve the embedded `index.html` so the frontend router handles it
            // instead of the user getting a 404 on refresh/deep-link.
            (m, p) if m == "GET" && !p.starts_with("/api/") && !has_extension(p) => {
                bytes_ok(INDEX_HTML.as_bytes(), "text/html; charset=utf-8")
            }

            _ => Response::not_found(),
        }
    }
}

impl AppState {
    fn create_session(&self, req: &Request) -> Response {
        let body = match serde_json::from_slice::<serde_json::Value>(&req.body) {
            Ok(b) => b,
            Err(_) => return Response::error(400, "invalid JSON body"),
        };
        let media_path = match body.get("media_path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => media::normalize_path(p),
            _ => return Response::error(400, "media_path is required"),
        };
        let subtitle_path = body
            .get("subtitle_path")
            .and_then(|v| v.as_str())
            .map(|p| media::normalize_path(p));
        // `?force=1` rebuilds even when a cached session exists.
        let force = matches!(
            req.query.get("force").map(String::as_str),
            Some("1") | Some("true")
        );
        // Optional per-request opt-in to wav2vec2 timing refinement; defaults to
        // the process-wide setting (`--align` / `SENTENCE_VIDEO_ALIGN=1`).
        let align = body
            .get("align")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| crate::asr::config().align);
        // Optional per-request dubbing: generate a replacement voice track for
        // this session, which playback uses instead of the original audio.
        let dub = body.get("dub").and_then(|v| v.as_bool()).unwrap_or(false);
        let dub_engine = body
            .get("dub_engine")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        match self.build_session(&media_path, subtitle_path, force, align) {
            Ok(session) => {
                // Dubbing runs in the background so the caller — the Manage page
                // — can poll its progress. A missing engine is still reported on
                // this request (422); everything else is asynchronous.
                let dubbing = if dub {
                    match self.start_dub(&session, dub_engine.as_deref(), false) {
                        Ok(state) => state,
                        Err((status, msg)) => return Response::error(status, &msg),
                    }
                } else {
                    self.dub_status(&session)
                };
                // A source the browser cannot decode (HEVC) is re-encoded in
                // the background, so the player can show it once ready.
                self.ensure_playable_async(&media_path);
                let body = serde_json::json!({
                    "session_id": session.id,
                    "media_duration": session.media_duration,
                    "cue_count": session.cues.len(),
                    "dubbed": self.is_dubbed(&session.id),
                    "dubbing": dubbing,
                    "video": self.video_status(&media_path),
                });
                Response::text(200, "application/json; charset=utf-8", body.to_string())
            }
            Err((status, msg)) => Response::error(status, &msg),
        }
    }

    /// `POST /api/pick-file?kind=media|subtitle`
    ///
    /// Opens the native OS file dialog on the server machine and returns the
    /// chosen absolute path as `{"path": "..."}` (or `{"path": null}` when the
    /// user cancels). Guarded against cross-origin callers so a random website
    /// cannot drive the local dialog.
    fn pick_file(&self, req: &Request) -> Response {
        let origin = req.headers.get("origin").map(|s| s.as_str());
        if !local_origin_ok(origin) {
            return Response::error(
                403,
                "the file picker is only available from the same machine (loopback)",
            );
        }
        let kind = req.query.get("kind").map(|s| s.as_str()).unwrap_or("media");
        match crate::picker::pick_file(kind) {
            Ok(Some(path)) => Response::text(
                200,
                "application/json; charset=utf-8",
                serde_json::json!({ "path": path }).to_string(),
            ),
            Ok(None) => Response::text(200, "application/json; charset=utf-8", r#"{"path":null}"#),
            Err(e) => Response::error(500, &e),
        }
    }

    /// Build and register a session from a media path and optional subtitle.
    /// Returns the session id. Errors carry an HTTP status + message.
    ///
    /// The result is persisted to SQLite and keyed by the media path plus the
    /// media/subtitle modification times, so re-opening the same source later
    /// returns the stored sentence list instead of re-parsing (`force` skips
    /// the cache and rebuilds).
    pub fn build_session(
        &self,
        media_path: &str,
        subtitle_path: Option<String>,
        force: bool,
        align: bool,
    ) -> Result<Session, (u16, String)> {
        let media_path = media::normalize_path(media_path);
        if !Path::new(&media_path).is_file() {
            logs::error("build", "media file not found")
                .media(&media_path)
                .send();
            return Err((404, format!("media file not found: {media_path}")));
        }
        logs::info("build", "building session")
            .media(&media_path)
            .detail(format!(
                "force={force} · align={align} · subtitle={}",
                subtitle_path.as_deref().unwrap_or("auto")
            ))
            .send();
        let media_duration = media::probe_duration(&media_path).map_err(|e| (500, e))?;

        // Cue source, in priority order:
        //   1. an explicitly supplied subtitle,
        //   2. `<stem>_full_sentence.srt` next to the media (reused as-is),
        //   3. a legacy adjacent `<stem>.srt`/`.vtt`,
        //   4. otherwise transcribe — and save the result as (2) for next time.
        let located: Option<String> = match subtitle_path {
            Some(p) if Path::new(&p).is_file() => Some(media::normalize_path(&p)),
            Some(p) => return Err((404, format!("subtitle file not found: {p}"))),
            None => transcribe::locate_subtitle(&media_path, None),
        };

        let media_mtime = file_mtime(&media_path);
        let key = SourceKey {
            media_path: media_path.clone(),
            media_mtime,
            subtitle_path: located.clone(),
            subtitle_mtime: located.as_deref().and_then(file_mtime),
        };

        // Cache hit: the same media + subtitle (unchanged) was processed before.
        // Checked before transcribing, so a repeat selection is instant.
        if !force {
            if let Some(existing) = self.db.find_by_source(&key) {
                logs::debug("build", "reusing the cached session")
                    .media(&media_path)
                    .session(&existing.id)
                    .detail(format!("{} cue(s)", existing.cues.len()))
                    .send();
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(existing.id.clone(), existing.clone());
                return Ok(existing);
            }
        }

        let (cues, source_format, used_subtitle) = match &located {
            Some(path) => (
                transcribe::cues_from_subtitle(path, &media_path).map_err(|e| (422, e))?,
                subtitle::detect_format(path).to_string(),
                Some(path.clone()),
            ),
            None => {
                // Transcription and alignment checkpoints. Opening the recipe
                // directory clears the previous one when the model, language, or
                // aligner weights changed, so a stale window is never reused.
                let work = match self.work_dir(&media_path) {
                    Some(dir) => Some(
                        checkpoint::RecipeDir::open(dir, &asr_recipe()).map_err(|e| (500, e))?,
                    ),
                    None => None,
                };
                let (cues, saved) = {
                    // Only this branch is slow enough to be worth reporting:
                    // parsing a subtitle is instant, transcribing and aligning a
                    // video is minutes. The build is registered around it and
                    // removed on both the success and the error path.
                    let sink = self.begin_build(&media_path);
                    let transcribed = transcribe::transcribe_and_save(
                        &media_path,
                        align,
                        work.as_ref().map(|w| w.dir()),
                        Some(&sink),
                    );
                    self.end_build(&media_path);
                    match transcribed {
                        Ok(done) => done,
                        // Cancelling is not a failure: the request that started
                        // the build reports it as a clean "stopped", and the
                        // checkpoints stay on disk so a later attempt resumes.
                        Err(e) if e == transcribe::CANCELLED => {
                            logs::warn("build", "build cancelled")
                                .media(&media_path)
                                .send();
                            return Err((409, e));
                        }
                        Err(e) => {
                            logs::error("build", "transcription failed")
                                .media(&media_path)
                                .detail(&e)
                                .send();
                            return Err((422, format!("transcription failed: {e}")));
                        }
                    }
                };
                if let Some(path) = &saved {
                    println!("sentence-video: saved transcript → {path}");
                }
                (cues, "asr".to_string(), saved)
            }
        };

        // Re-key with the subtitle that was actually used, so a transcription
        // that just created `<stem>_full_sentence.srt` is cached under that file.
        let key = SourceKey {
            media_path: media_path.clone(),
            media_mtime,
            subtitle_path: used_subtitle.clone(),
            subtitle_mtime: used_subtitle.as_deref().and_then(file_mtime),
        };

        let id = gen_id();
        let session = Session {
            id: id.clone(),
            media_path,
            media_duration,
            source_format,
            cues,
        };
        self.db
            .insert_session(&session, &key)
            .map_err(|e| (500, format!("cannot persist session: {e}")))?;
        logs::info("build", "session ready")
            .session(&id)
            .media(&session.media_path)
            .detail(format!(
                "{} cue(s) · source={} · {:.1}s",
                session.cues.len(),
                session.source_format,
                session.media_duration
            ))
            .send();
        self.sessions.lock().unwrap().insert(id, session.clone());
        Ok(session)
    }

    /// Fetch a session from the hot cache, falling back to SQLite.
    fn load_session(&self, id: &str) -> Option<Session> {
        if let Some(s) = self.sessions.lock().unwrap().get(id) {
            return Some(s.clone());
        }
        let session = self.db.get_session(id)?;
        self.sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), session.clone());
        Some(session)
    }

    /// `GET /api/sessions` — every stored session, each enriched with the two
    /// things that are *derived* rather than stored: whether a voice track
    /// exists, and how far dubbing got. [`Store`] deliberately stays free of job
    /// state, which is process-local and dies with the server.
    fn list_sessions(&self) -> Response {
        let summaries: Vec<serde_json::Value> = self
            .db
            .list_sessions()
            .into_iter()
            .map(|summary| {
                let mut value =
                    serde_json::to_value(&summary).unwrap_or_else(|_| serde_json::json!({}));
                let dubbed = self.is_dubbed(&summary.session_id);
                // `dubbing.done` counts the sentence checkpoints, which needs the
                // cues, so the session is loaded rather than guessed at.
                let dubbing = match self.load_session(&summary.session_id) {
                    Some(session) => self.dub_status(&session),
                    None => serde_json::json!({
                        "state": if dubbed { "done" } else { "idle" },
                        "stage": "synth",
                        "done": 0,
                        "total": summary.cue_count,
                        "failed": [],
                        "error": serde_json::Value::Null,
                    }),
                };
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("dubbed".to_string(), serde_json::Value::Bool(dubbed));
                    obj.insert("dubbing".to_string(), dubbing);
                    obj.insert("muxed".to_string(), self.muxed_state(&summary.media_path));
                    obj.insert(
                        "platform_subtitle".to_string(),
                        self.platform_subtitle_state(&summary.session_id, &summary.media_path),
                    );
                }
                value
            })
            .collect();
        match serde_json::to_string(&summaries) {
            Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
            Err(e) => Response::error(500, &e.to_string()),
        }
    }

    /// `GET /api/logs` — one page of the log database, newest first.
    ///
    /// Query params: `level`, `category`, `session`, `media`, `q`, `since_ms`,
    /// `before_id`, `limit`. The response bundles `records`, their `total`, and
    /// `stats` so the Logs page fills its table and header from one round trip.
    fn list_logs(&self, req: &Request) -> Response {
        let p = &req.query;
        let query = logs::Query {
            level: p.get("level").and_then(|l| logs::Level::from_str(l)),
            category: p.get("category").filter(|c| !c.is_empty()).cloned(),
            session_id: p.get("session").filter(|s| !s.is_empty()).cloned(),
            media: p.get("media").filter(|m| !m.is_empty()).cloned(),
            q: p.get("q").filter(|s| !s.is_empty()).cloned(),
            since_ms: p.get("since_ms").and_then(|v| v.parse().ok()),
            before_id: p.get("before_id").and_then(|v| v.parse().ok()),
            limit: p.get("limit").and_then(|v| v.parse().ok()).unwrap_or(500),
        };
        let records = match logs::query(&query) {
            Ok(r) => r,
            Err(e) => return Response::error(500, &e),
        };
        let stats = logs::stats().unwrap_or_else(|_| serde_json::json!({}));
        let body = serde_json::json!({
            "records": records,
            "total": records.len(),
            "stats": stats,
        });
        match serde_json::to_string(&body) {
            Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
            Err(e) => Response::error(500, &e.to_string()),
        }
    }

    /// `GET /api/logs/stats` — counts the Logs page header shows.
    fn log_stats(&self) -> Response {
        match logs::stats().and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())) {
            Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
            Err(e) => Response::error(500, &e),
        }
    }

    /// `DELETE /api/logs` — wipe the log database; reports how many rows went.
    fn clear_logs(&self) -> Response {
        match logs::clear() {
            Ok(n) => Response::text(
                200,
                "application/json; charset=utf-8",
                format!("{{\"cleared\": {n}}}"),
            ),
            Err(e) => Response::error(500, &e),
        }
    }

    /// `GET /api/builds` — the media being processed right now, longest first.
    ///
    /// Exists because a build is synchronous: the session row is written only
    /// when transcription *and* alignment are done, and for a subtitle-free video
    /// that is the entire cost of the pipeline. Without this the Manage page had
    /// nothing to show for a video mid-build, which is indistinguishable from a
    /// video that was never submitted.
    fn list_builds(&self) -> Response {
        let mut builds: Vec<serde_json::Value> = self
            .build_jobs
            .lock()
            .map(|map| map.values().map(BuildProgress::to_json).collect())
            .unwrap_or_default();
        // The one running longest is the one the user is worried about.
        builds.sort_by(|a, b| b["elapsed_secs"].as_u64().cmp(&a["elapsed_secs"].as_u64()));
        match serde_json::to_string(&builds) {
            Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
            Err(e) => Response::error(500, &e.to_string()),
        }
    }

    /// `POST /api/builds/cancel` — ask the build of one media file to stop.
    ///
    /// Asynchronous like the build itself: this only raises the flag. The build
    /// notices at its next stage boundary, unwinds, and answers the request that
    /// started it with 409 "cancelled by the user" — until then it keeps
    /// appearing in [`Self::list_builds`], because it is still running (a window
    /// of alignment cannot be cut in half).
    ///
    /// A 404 means there is nothing to cancel: either it finished between the
    /// poll that showed it and this click, or the path is wrong.
    fn cancel_build(&self, req: &Request) -> Response {
        let body = match serde_json::from_slice::<serde_json::Value>(&req.body) {
            Ok(b) => b,
            Err(_) => return Response::error(400, "invalid JSON body"),
        };
        let media_path = match body.get("media_path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => media::normalize_path(p),
            _ => return Response::error(400, "media_path is required"),
        };
        let cancelled = match self.build_jobs.lock() {
            Ok(map) => match map.get(&media_path) {
                Some(build) => {
                    build
                        .cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    true
                }
                None => false,
            },
            Err(_) => false,
        };
        match cancelled {
            true => {
                println!("sentence-video: cancelling the build of '{media_path}'");
                logs::warn("build", "cancellation requested")
                    .media(&media_path)
                    .send();
                Response::text(
                    200,
                    "application/json; charset=utf-8",
                    serde_json::json!({ "cancelled": true }).to_string(),
                )
            }
            false => Response::error(404, "no build in flight for that media file"),
        }
    }

    /// `DELETE /api/session/{id}` — remove a stored session (its cues and
    /// progress rows cascade with it) and the files only it owns.
    fn delete_session(&self, session: &Session) -> Response {
        match self.db.delete_session(&session.id) {
            Ok(true) => {
                self.sessions.lock().unwrap().remove(&session.id);
                self.remove_session_artifacts(session);
                Response::text(200, "application/json; charset=utf-8", r#"{"ok":true}"#)
            }
            Ok(false) => Response::error(404, "unknown session id"),
            Err(e) => Response::error(500, &e),
        }
    }

    /// Drop what this session produced: its voice track, its sentence
    /// checkpoints, its job state — and the transcription/alignment
    /// checkpoints *and* the muxed video **only once no other session uses the
    /// same media file**, since those two are keyed by the media and not by the
    /// session.
    ///
    /// The exported platform subtitle is left alone: it sits in the user's own
    /// folder under the video's own name, like the transcript written there, so
    /// it is not ours to delete.
    fn remove_session_artifacts(&self, session: &Session) {
        let _ = std::fs::remove_file(self.dubbed_track_path(&session.id));
        let _ = std::fs::remove_dir_all(self.tts_dir(&session.id));
        self.dub_jobs.lock().unwrap().remove(&session.id);
        self.platform_jobs.lock().unwrap().remove(&session.id);
        let shared = self
            .db
            .list_sessions()
            .iter()
            .any(|s| s.media_path == session.media_path);
        if !shared {
            let _ = std::fs::remove_file(self.muxed_path(&session.media_path));
            if let Some(dir) = self.work_dir(&session.media_path) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// `DELETE /api/sessions` — remove every stored session.
    fn delete_all_sessions(&self) -> Response {
        match self.db.delete_all_sessions() {
            Ok(n) => {
                self.sessions.lock().unwrap().clear();
                self.dub_jobs.lock().unwrap().clear();
                self.platform_jobs.lock().unwrap().clear();
                // Nothing references these trees any more, so the whole lot goes
                // — including checkpoints whose run never finished.
                let _ = std::fs::remove_dir_all(self.data_dir.join("dubbed"));
                let _ = std::fs::remove_dir_all(self.data_dir.join("muxed"));
                let _ = std::fs::remove_dir_all(self.data_dir.join("work"));
                Response::text(
                    200,
                    "application/json; charset=utf-8",
                    format!(r#"{{"deleted":{n}}}"#),
                )
            }
            Err(e) => Response::error(500, &e),
        }
    }

    /// `POST /api/session/{id}/dub` → body `{engine?, retry_failed?}`
    /// `GET  /api/session/{id}/dub` → the current state of the job.
    ///
    /// The POST is idempotent while a run is in flight and restartable after a
    /// run failed, which is what makes "resume where it failed" work: the
    /// sentences already checkpointed are not sent to the engine again.
    fn route_dub(&self, session: &Session, req: &Request) -> Response {
        let result = match req.method.as_str() {
            "GET" => Ok(self.dub_status(session)),
            "POST" => {
                let body: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
                let engine = body.get("engine").and_then(|v| v.as_str());
                let retry_failed = body
                    .get("retry_failed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.start_dub(session, engine, retry_failed)
            }
            _ => return Response::not_found(),
        };
        match result {
            Ok(value) => bytes_ok(
                value.to_string().as_bytes(),
                "application/json; charset=utf-8",
            ),
            Err((status, msg)) => Response::error(status, &msg),
        }
    }

    /// `POST /api/session/{id}/retranscode` — discard the cached H.264 copy and
    /// re-encode it.
    ///
    /// [`Self::ensure_playable_async`] only ever runs when no copy exists, so
    /// without this a copy produced by a bad transcode would be permanent.
    fn route_retranscode(&self, session: &Session, req: &Request) -> Response {
        if req.method != "POST" {
            return Response::not_found();
        }
        if let Some(path) = self.playable_path(&session.media_path) {
            let _ = std::fs::remove_file(&path);
        }
        // Clear the failure marker too, or the job would refuse to restart.
        self.playable_jobs
            .lock()
            .unwrap()
            .remove(&session.media_path);
        self.ensure_playable_async(&session.media_path);
        bytes_ok(
            self.video_status(&session.media_path)
                .to_string()
                .as_bytes(),
            "application/json; charset=utf-8",
        )
    }

    /// `POST /api/session/{id}/mux` — write this session's video out with the
    /// replacement voice track as its audio, into `data_dir/muxed/`.
    ///
    /// Synchronous on purpose: the video stream is copied and only the audio is
    /// encoded, so this is seconds where dubbing is minutes — the caller can
    /// wait for the file instead of polling a job. The output is written to a
    /// sibling temp file and renamed, so an interrupted or failed mux is never
    /// mistaken for a finished one — and because the final name is derived from
    /// the media alone, a re-mux replaces the previous file rather than adding a
    /// second one next to it.
    fn route_mux(&self, session: &Session, req: &Request) -> Response {
        if req.method != "POST" {
            return Response::not_found();
        }
        let track = self.dubbed_track_path(&session.id);
        if !track.is_file() {
            return Response::error(409, "this session has no voice track to mux — dub it first");
        }
        match mux_video_into(&self.data_dir, &session.media_path, &track) {
            Ok(_) => bytes_ok(
                self.muxed_state(&session.media_path).to_string().as_bytes(),
                "application/json; charset=utf-8",
            ),
            Err(e) => Response::error(500, &e),
        }
    }

    /// `POST /api/session/{id}/platform-subtitle` with an empty body starts (or
    /// redoes) the export; `GET` reports the state to poll instead.
    fn route_platform_subtitle(&self, session: &Session, req: &Request) -> Response {
        let value = match req.method.as_str() {
            "GET" => self.platform_subtitle_state(&session.id, &session.media_path),
            "POST" => self.start_platform_subtitle(session),
            _ => return Response::not_found(),
        };
        bytes_ok(
            value.to_string().as_bytes(),
            "application/json; charset=utf-8",
        )
    }

    /// `POST /api/session/{id}/open-muxed-dir`
    ///
    /// Opens the folder holding this session's muxed video in the OS file
    /// manager on the server machine. Guarded like the file picker: it hands
    /// control to a desktop program, so a random website must not reach it.
    fn open_muxed_dir(&self, session: &Session, req: &Request) -> Response {
        let file = self.muxed_path(&session.media_path);
        self.reveal_dir(req, &file, "this session has no muxed video")
    }

    /// `POST /api/session/{id}/open-subtitle-dir`
    ///
    /// Opens the folder holding the exported platform subtitle — the source
    /// video's own folder — for the same reason and under the same guard.
    fn open_subtitle_dir(&self, session: &Session, req: &Request) -> Response {
        let path = crate::platform::platform_subtitle_path(&session.media_path);
        self.reveal_dir(
            req,
            Path::new(&path),
            "this session has no platform subtitle",
        )
    }

    /// Reveal `file`'s folder in the server machine's file manager.
    fn reveal_dir(&self, req: &Request, file: &Path, missing: &str) -> Response {
        if req.method != "POST" {
            return Response::not_found();
        }
        let origin = req.headers.get("origin").map(|s| s.as_str());
        if !local_origin_ok(origin) {
            return Response::error(
                403,
                "opening a folder is only available from the same machine (loopback)",
            );
        }
        if !file.is_file() {
            return Response::error(404, missing);
        }
        let dir = match file.parent() {
            Some(d) => d.to_string_lossy().into_owned(),
            None => return Response::error(500, "the file has no parent folder"),
        };
        match crate::picker::open_dir(&dir) {
            Ok(()) => Response::text(
                200,
                "application/json; charset=utf-8",
                serde_json::json!({ "opened": dir }).to_string(),
            ),
            Err(e) => Response::error(500, &e),
        }
    }

    fn route_session(&self, path: &str, req: &Request) -> Response {
        let rest = &path["/api/session/".len()..];
        let mut segs = rest.split('/');
        let id = segs.next().unwrap_or_default().to_string();
        let sub = segs.next();

        let session = match self.load_session(&id) {
            Some(s) => s,
            None => return Response::error(404, "unknown session id"),
        };

        match sub {
            None | Some("") => {
                if req.method == "DELETE" {
                    return self.delete_session(&session);
                }
                // `dubbed` is derived from whether a voice track exists, so it
                // needs no column in the database. `video` reports the source
                // codec, and opening a session is what starts the H.264
                // re-encode of a source the browser cannot play.
                self.ensure_playable_async(&session.media_path);
                // A dubbed session whose voice track predates auto-mux still has
                // no single file to play; opening it is the trigger to make one.
                self.ensure_muxed_async(&session);
                let mut value = match serde_json::to_value(&session) {
                    Ok(v) => v,
                    Err(e) => return Response::error(500, &e.to_string()),
                };
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "dubbed".to_string(),
                        serde_json::Value::Bool(self.is_dubbed(&session.id)),
                    );
                    obj.insert("dubbing".to_string(), self.dub_status(&session));
                    obj.insert("video".to_string(), self.video_status(&session.media_path));
                    obj.insert("muxed".to_string(), self.muxed_state(&session.media_path));
                    obj.insert(
                        "platform_subtitle".to_string(),
                        self.platform_subtitle_state(&session.id, &session.media_path),
                    );
                }
                bytes_ok(
                    value.to_string().as_bytes(),
                    "application/json; charset=utf-8",
                )
            }
            Some("cues") => {
                let body = serde_json::to_vec(&session.cues).unwrap();
                bytes_ok(&body, "application/json; charset=utf-8")
            }
            Some("dub") => self.route_dub(&session, req),
            Some("retranscode") => self.route_retranscode(&session, req),
            Some("mux") => self.route_mux(&session, req),
            // The muxed video (video + replacement voice track), range-streamed
            // so it can be played or saved from the browser.
            Some("muxed") => {
                let path = self.muxed_path(&session.media_path);
                if !path.is_file() {
                    return Response::error(404, "this session has no muxed video");
                }
                stream_file_range(&path.to_string_lossy(), "video/mp4", req)
            }
            // Reveal the muxed video's folder in the server's file manager,
            // rather than handing the browser a file it can only download.
            Some("open-muxed-dir") => self.open_muxed_dir(&session, req),
            // The short cues a video platform needs, written as `<stem>.srt`
            // beside the source video rather than into the server's own folders.
            Some("platform-subtitle") => self.route_platform_subtitle(&session, req),
            Some("open-subtitle-dir") => self.open_subtitle_dir(&session, req),
            Some("media") => self.stream_media(&session, req),
            // The generated replacement voice track (a WAV), range-streamed so
            // the player can seek it like any other media.
            Some("dubbed-audio") => {
                let path = self.dubbed_track_path(&session.id);
                if !path.is_file() {
                    return Response::error(404, "this session has no dubbed audio");
                }
                stream_file_range(&path.to_string_lossy(), "audio/wav", req)
            }
            // Exact audio slice of an arbitrary time range, decoded server-side.
            Some("audio") => self.route_audio(&session, req),
            Some("progress") => self.route_progress(&session, req),
            Some(_) => Response::not_found(),
        }
    }

    /// `GET /api/session/{id}/progress` → `{ last_index, position }`
    /// `POST /api/session/{id}/progress` with the same body → persists it.
    fn route_progress(&self, session: &Session, req: &Request) -> Response {
        match req.method.as_str() {
            "GET" => {
                let progress = self.db.get_progress(&session.id).unwrap_or(Progress {
                    last_index: -1,
                    position: 0.0,
                });
                match serde_json::to_string(&progress) {
                    Ok(body) => Response::text(200, "application/json; charset=utf-8", body),
                    Err(e) => Response::error(500, &e.to_string()),
                }
            }
            "POST" => {
                let progress: Progress = match serde_json::from_slice(&req.body) {
                    Ok(p) => p,
                    Err(_) => return Response::error(400, "invalid JSON body"),
                };
                match self.db.set_progress(&session.id, progress) {
                    Ok(()) => {
                        Response::text(200, "application/json; charset=utf-8", r#"{"ok":true}"#)
                    }
                    Err(e) => Response::error(500, &e),
                }
            }
            _ => Response::not_found(),
        }
    }

    fn stream_media(&self, session: &Session, req: &Request) -> Response {
        // Prefer the H.264 copy when the source codec is one browsers cannot
        // decode; the player polls until that copy exists, so it is only ever
        // requested once it is ready.
        let path = self
            .playable_path(&session.media_path)
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| session.media_path.clone());
        let media_type = mime_for(&path);
        stream_file_range(&path, media_type, req)
    }

    /// `GET /api/session/{id}/audio?start=<sec>&end=<sec>`
    ///
    /// Decodes exactly that slice of the source into a 24 kHz mono WAV clip.
    /// This is the server-side counterpart to `/media`: `/media` lets the
    /// browser play and seek the original file itself, whereas this guarantees
    /// sample-exact boundaries and returns audio only. `start`/`end` are
    /// clamped to the media duration, and the temp WAV is deleted right after
    /// it is read into the response.
    ///
    /// When the session has a replacement voice track, the clip comes from
    /// that instead, so "play one sentence" uses the dubbed voice too.
    fn route_audio(&self, session: &Session, req: &Request) -> Response {
        let start = req
            .query
            .get("start")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            .max(0.0);
        let end = req
            .query
            .get("end")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(session.media_duration)
            .min(session.media_duration);
        if end <= start {
            return Response::error(400, "end must be greater than start");
        }
        let track = self.dubbed_track_path(&session.id);
        let source = if track.is_file() {
            track.to_string_lossy().into_owned()
        } else {
            session.media_path.clone()
        };
        let sample_rate = 24000u32;
        let out_path = self
            .cache_dir
            .join(format!("clip_{}_{}.wav", session.id, gen_id()));
        match media::decode_segment_wav(
            &source,
            start,
            end,
            sample_rate,
            &out_path.to_string_lossy(),
        ) {
            Ok(()) => {
                let data = match std::fs::read(&out_path) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = std::fs::remove_file(&out_path);
                        return Response::error(500, &e.to_string());
                    }
                };
                let _ = std::fs::remove_file(&out_path);
                Response::bytes(200, "OK", "audio/wav", data)
            }
            Err(e) => Response::error(500, &e),
        }
    }
}

/// Write the video-platform subtitle for a session and return its path.
///
/// With a dubbed track, the sentence timings have to be re-measured first: the
/// replacement voice was synthesised into the original cue slots, so its words
/// do not fall where the original voice put them. Transcribing the dubbed audio
/// costs as much as building the session did, which is why the caller runs this
/// on a worker thread. Without one, the cues already carry the right timings
/// and nothing is listened to again.
fn write_platform_subtitle(
    cues: &[Cue],
    dubbed: Option<&std::path::Path>,
    media_path: &str,
) -> Result<String, String> {
    let cues = match dubbed {
        Some(track) => {
            let transcript =
                crate::pipeline::transcribe::transcribe_words(&track.to_string_lossy(), false)?;
            crate::platform::retime_to_dubbed(cues, &transcript.words)
        }
        None => cues.to_vec(),
    };
    crate::platform::export(&cues, media_path, crate::platform::DEFAULT_MAX_CHARS)
}

fn file_mtime(path: &str) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn mime_for(path: &str) -> &'static str {
    let l = path.to_ascii_lowercase();
    if l.ends_with(".mp4") {
        "video/mp4"
    } else if l.ends_with(".webm") {
        "video/webm"
    } else if l.ends_with(".mkv") {
        "video/x-matroska"
    } else if l.ends_with(".mov") {
        "video/quicktime"
    } else if l.ends_with(".ogg") {
        "audio/ogg"
    } else if l.ends_with(".wav") {
        "audio/wav"
    } else if l.ends_with(".mp3") {
        "audio/mpeg"
    } else if l.ends_with(".m4a") {
        "audio/mp4"
    } else if l.ends_with(".aac") {
        "audio/aac"
    } else {
        "application/octet-stream"
    }
}

/// Serve a file with HTTP `Range` support.
fn stream_file_range(path: &str, content_type: &str, req: &Request) -> Response {
    let len = match std::fs::metadata(path).map(|m| m.len()) {
        Ok(l) => l,
        Err(_) => return Response::error(404, "media file unavailable"),
    };
    let accept = req.headers.get("range").cloned().unwrap_or_default();

    if accept.starts_with("bytes=") && !accept.trim().eq_ignore_ascii_case("bytes=0-") {
        let spec = accept.trim_start_matches("bytes=").trim();
        if let Some((s, e)) = parse_range(spec, len) {
            let mut file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(_) => return Response::error(500, "cannot open media file"),
            };
            if s >= len {
                return Response {
                    status: 416,
                    reason: "Range Not Satisfiable",
                    headers: vec![("Content-Range".into(), format!("bytes */{len}"))],
                    body: Vec::new(),
                };
            }
            let eff_end = e.min(len.saturating_sub(1));
            let count = eff_end - s + 1;
            let mut buf = vec![0u8; count as usize];
            file.seek(SeekFrom::Start(s)).ok();
            let _ = file.read_exact(&mut buf);
            return Response {
                status: 206,
                reason: "Partial Content",
                headers: vec![
                    ("Content-Type".into(), content_type.to_string()),
                    ("Accept-Ranges".into(), "bytes".into()),
                    (
                        "Content-Range".into(),
                        format!("bytes {}-{}/{}", s, eff_end, len),
                    ),
                ],
                body: buf,
            };
        }
    }

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Response::error(500, "cannot open media file"),
    };
    let mut buf = Vec::with_capacity(len as usize);
    let _ = file.read_to_end(&mut buf);
    Response {
        status: 200,
        reason: "OK",
        headers: vec![
            ("Content-Type".into(), content_type.to_string()),
            ("Accept-Ranges".into(), "bytes".into()),
        ],
        body: buf,
    }
}

/// Parse `start-end`, `start-`, or `-suffix` from a byte-range spec.
fn parse_range(spec: &str, len: u64) -> Option<(u64, u64)> {
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        // suffix range: last N bytes
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = len.saturating_sub(n);
        return Some((start, len.saturating_sub(1)));
    }
    let start: u64 = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() {
        len.saturating_sub(1)
    } else {
        b.trim().parse().ok()?
    };
    Some((start, end))
}

fn bytes_ok(data: &[u8], content_type: &str) -> Response {
    Response::bytes(200, "OK", content_type, data.to_vec())
}

/// True when the last path segment looks like a static asset (contains a `.`),
/// e.g. `/assets/index-abc.js`. Those must 404 rather than fall back to the
/// SPA `index.html`; extension-less paths are client-side routes.
fn has_extension(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .map(|seg| seg.contains('.'))
        .unwrap_or(false)
}

/// True when the request may drive the native file dialog: no `Origin` at all
/// (curl, same-origin navigation) or a loopback origin. A public website cannot
/// read the response without CORS headers, but this also stops it from popping
/// the dialog in the first place.
fn local_origin_ok(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let origin = origin.trim().to_ascii_lowercase();
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(end) = authority.strip_prefix('[') {
        end.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spa_fallback_distinguishes_routes_from_assets() {
        assert!(has_extension("/assets/index-abc.js"));
        assert!(has_extension("/favicon.svg"));
        assert!(!has_extension("/player"));
        assert!(!has_extension("/subtitle-video"));
        assert!(!has_extension("/"));
    }

    #[test]
    fn file_picker_allows_only_loopback_origins() {
        // Non-browser clients and same-origin navigations send no Origin.
        assert!(local_origin_ok(None));
        // Dev server (vite) and the binary itself.
        assert!(local_origin_ok(Some("http://127.0.0.1:5173")));
        assert!(local_origin_ok(Some("http://localhost:8018")));
        assert!(local_origin_ok(Some("http://[::1]:8018")));
        // A public site must not be able to open the dialog.
        assert!(!local_origin_ok(Some("https://evil.example")));
        assert!(!local_origin_ok(Some("http://127.0.0.1.evil.example")));
        assert!(!local_origin_ok(Some("http://192.168.1.5:5173")));
    }

    #[test]
    fn a_running_dub_blocks_a_second_run() {
        let running = DubJob::Running {
            stage: "synth",
            done: 3,
            total: 23,
        };
        // Even with `retry_failed`, a run already in flight is only reported.
        assert!(running.blocks_restart(false, false));
        assert!(running.blocks_restart(true, false));
        assert!(running.blocks_restart(false, true));
    }

    #[test]
    fn a_partial_or_failed_dub_is_always_restartable() {
        // The track exists — the run still wrote one — but the failures are
        // recorded so pressing "resume" must actually resume, without the caller
        // having to know a flag is needed.
        let partial = DubJob::Finished {
            synthesized: 21,
            total: 23,
            failed: vec![DubFailure {
                index: 5,
                text: "five.".to_string(),
                error: "boom".to_string(),
            }],
        };
        assert!(!partial.blocks_restart(false, true));

        let broken = DubJob::Failed {
            error: "no engine".to_string(),
        };
        assert!(!broken.blocks_restart(false, true));
    }

    #[test]
    fn a_complete_dub_is_a_no_op_unless_a_retry_is_asked_for() {
        let clean = DubJob::Finished {
            synthesized: 23,
            total: 23,
            failed: Vec::new(),
        };
        assert!(clean.blocks_restart(false, true));
        assert!(!clean.blocks_restart(true, true));
        // A finished run whose track has gone (deleted by hand) is redone.
        assert!(!clean.blocks_restart(false, false));
    }

    #[test]
    fn dub_json_reports_progress_and_failures() {
        let running = DubJob::Running {
            stage: "synth",
            done: 3,
            total: 23,
        }
        .to_json(23);
        assert_eq!(running["state"], "running");
        assert_eq!(running["stage"], "synth");
        assert_eq!(running["done"], 3);
        assert_eq!(running["total"], 23);
        assert!(running["failed"].as_array().unwrap().is_empty());
        assert!(running["error"].is_null());

        let partial = DubJob::Finished {
            synthesized: 21,
            total: 23,
            failed: vec![DubFailure {
                index: 5,
                text: "five.".to_string(),
                error: "boom".to_string(),
            }],
        }
        .to_json(23);
        assert_eq!(partial["state"], "failed");
        assert_eq!(partial["stage"], "stitch");
        assert_eq!(partial["done"], 21);
        assert_eq!(partial["failed"][0]["index"], 5);
        assert_eq!(partial["failed"][0]["text"], "five.");
        assert_eq!(partial["failed"][0]["error"], "boom");

        let clean = DubJob::Finished {
            synthesized: 23,
            total: 23,
            failed: Vec::new(),
        }
        .to_json(23);
        assert_eq!(clean["state"], "done");
        assert!(clean["failed"].as_array().unwrap().is_empty());

        let broken = DubJob::Failed {
            error: "no engine".to_string(),
        }
        .to_json(23);
        assert_eq!(broken["state"], "failed");
        assert_eq!(broken["total"], 23);
        assert_eq!(broken["error"], "no engine");
    }

    #[test]
    fn recorded_failures_outlive_the_job_map() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-app-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::Db::open(&dir.join("test.db")).expect("open db");
        let app = AppState::new(dir.join("cache"), dir.join("data"), Arc::new(db));

        let session = Session {
            id: "s1".to_string(),
            media_path: "/media/a.mp4".to_string(),
            media_duration: 12.5,
            source_format: "srt".to_string(),
            cues: vec![
                crate::models::Cue {
                    index: 0,
                    start: 0.0,
                    end: 1.0,
                    text: "one.".to_string(),
                    words: Vec::new(),
                    source_path: None,
                },
                crate::models::Cue {
                    index: 1,
                    start: 1.0,
                    end: 2.0,
                    text: "two.".to_string(),
                    words: Vec::new(),
                    source_path: None,
                },
            ],
        };

        // No job object (as after a restart) and a track that was written with a
        // silent slot: the recorded failure must win over "the track exists".
        let tts = app.tts_dir(&session.id);
        std::fs::create_dir_all(&tts).unwrap();
        std::fs::write(app.dubbed_track_path(&session.id), b"RIFF").unwrap();
        checkpoint::write_json(
            &tts,
            FAILURES_FILE,
            &vec![DubFailure {
                index: 1,
                text: "two.".to_string(),
                error: "boom".to_string(),
            }],
        )
        .unwrap();

        let status = app.dub_status(&session);
        assert_eq!(status["state"], "failed");
        assert_eq!(status["done"], 1);
        assert_eq!(status["total"], 2);
        assert_eq!(status["failed"][0]["index"], 1);
        assert_eq!(status["failed"][0]["text"], "two.");

        // Once the failure is gone, the same track reports as complete.
        std::fs::remove_file(tts.join(FAILURES_FILE)).unwrap();
        assert_eq!(app.dub_status(&session)["state"], "done");
    }

    /// A build is synchronous, so the only way to see it is the registry this
    /// endpoint reads: press "Add video" and the media must be visible — with its
    /// stage — while the POST is still blocked in transcription.
    #[test]
    fn an_in_flight_build_is_reported_until_it_ends() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-build-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::Db::open(&dir.join("test.db")).expect("open db");
        let app = AppState::new(dir.join("cache"), dir.join("data"), Arc::new(db));

        let builds = |app: &AppState| -> serde_json::Value {
            serde_json::from_slice(&app.list_builds().body).unwrap()
        };

        // Nothing submitted yet: the Manage page has nothing to report.
        assert_eq!(builds(&app).as_array().unwrap().len(), 0);

        let sink = app.begin_build("/media/a.mp4");
        // `true` from the sink is "keep going" — the pipeline unwinds on `false`.
        assert!(sink(transcribe::BuildStage::Transcribe));
        let list = builds(&app);
        assert_eq!(list[0]["media_path"], "/media/a.mp4");
        assert_eq!(list[0]["stage"], "transcribe");
        // Whisper is one pass over the file, so it has no countable unit.
        assert_eq!(list[0]["total"], 0);

        // Alignment is the stage a progress bar is worth drawing.
        sink(transcribe::BuildStage::Align { done: 1, total: 4 });
        let list = builds(&app);
        assert_eq!(list[0]["stage"], "align");
        assert_eq!(list[0]["done"], 1);
        assert_eq!(list[0]["total"], 4);

        // Cancelling goes through the same sink the pipeline reports to, so the
        // flag must reach whoever is running the build.
        let cancel = |media: &str| Request {
            method: "POST".to_string(),
            path: "/api/builds/cancel".to_string(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: serde_json::json!({ "media_path": media })
                .to_string()
                .into_bytes(),
        };
        // A path nobody is building is a 404, and must not cancel the live build.
        assert_eq!(app.cancel_build(&cancel("/media/other.mp4")).status, 404);
        assert!(sink(transcribe::BuildStage::Align { done: 2, total: 4 }));
        assert_eq!(app.cancel_build(&cancel("/media/a.mp4")).status, 200);
        // Raised flag: the next stage boundary tells the build to stop.
        assert!(!sink(transcribe::BuildStage::Align { done: 3, total: 4 }));

        // A finished build must vanish, or the page shows a phantom row forever.
        app.end_build("/media/a.mp4");
        assert_eq!(builds(&app).as_array().unwrap().len(), 0);

        // A window finishing just after the build ended is ignored, not an error —
        // and there is nothing left to cancel.
        sink(transcribe::BuildStage::Align { done: 4, total: 4 });
        assert_eq!(builds(&app).as_array().unwrap().len(), 0);
        assert_eq!(app.cancel_build(&cancel("/media/a.mp4")).status, 404);
    }

    /// Muxing is the only step that produces a file the user takes away, so the
    /// two things that matter are where it lands and that "dub first" is a real
    /// refusal rather than a half-written mp4.
    #[test]
    fn muxing_needs_a_voice_track_and_writes_into_its_own_folder() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-mux-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::Db::open(&dir.join("test.db")).expect("open db");
        let data = dir.join("data");
        let app = AppState::new(dir.join("cache"), data.clone(), Arc::new(db));

        let session = Session {
            id: "s1".to_string(),
            media_path: "/media/lecture.mp4".to_string(),
            media_duration: 12.5,
            source_format: "srt".to_string(),
            cues: Vec::new(),
        };

        // The file *is* the state: nothing written yet reads as `idle`.
        assert_eq!(app.muxed_state(&session.media_path)["state"], "idle");

        // The path is inside `muxed/` and is named after the source, so the
        // result is recognisable next to the video it came from.
        let dst = app.muxed_path(&session.media_path);
        assert_eq!(dst.parent(), Some(data.join("muxed").as_path()));
        assert_eq!(dst.file_name().unwrap(), "lecture.mp4");

        let post = |method: &str| Request {
            method: method.to_string(),
            path: "/api/session/s1/mux".to_string(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Vec::new(),
        };

        // Only a POST may start one.
        assert_eq!(app.route_mux(&session, &post("GET")).status, 404);

        // No voice track → refuse, and leave no file behind for the page to
        // report as a finished mux.
        let refused = app.route_mux(&session, &post("POST"));
        assert_eq!(refused.status, 409);
        assert!(!dst.exists());
        assert_eq!(app.muxed_state(&session.media_path)["state"], "idle");

        // With a track present the mux runs; this media has no video stream, so
        // the ffmpeg step fails and the `.part` file must not survive it.
        std::fs::create_dir_all(app.dubbed_track_path("s1").parent().unwrap()).unwrap();
        std::fs::write(app.dubbed_track_path("s1"), b"RIFF").unwrap();
        let failed = app.route_mux(&session, &post("POST"));
        assert_eq!(failed.status, 500);
        assert!(!dst.exists());
        assert!(!data.join("muxed").join("lecture.s1.part.mp4").exists());

        // A second session over the same media reuses the same name rather than
        // piling up a copy per session — so a re-mux overwrites the first file.
        let other = Session {
            id: "s2".to_string(),
            ..session
        };
        assert_eq!(app.muxed_path(&other.media_path), dst);
    }

    /// The platform subtitle is the one export that lands in the *user's* folder
    /// instead of the server's, so what matters is that it is named after the
    /// video and that its cues are short enough for a video platform.
    #[test]
    fn a_platform_subtitle_lands_beside_the_video_named_after_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-platform-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("test.db")).expect("open db");
        let app = AppState::new(dir.join("cache"), dir.join("data"), Arc::new(db));

        // A real file: the export writes next to it and does not invent a folder.
        let media = dir.join("lecture.mp4");
        std::fs::write(&media, b"not really a video").unwrap();
        let media_path = media.to_string_lossy().into_owned();
        let text = "The report was late because the train broke down near the station";
        let session = Session {
            id: "s1".to_string(),
            media_path: media_path.clone(),
            media_duration: 20.0,
            source_format: "asr".to_string(),
            cues: vec![Cue {
                index: 0,
                start: 0.0,
                end: 13.0,
                text: text.to_string(),
                words: text
                    .split_whitespace()
                    .enumerate()
                    .map(|(i, w)| crate::models::Word {
                        text: w.to_string(),
                        start: i as f64,
                        end: i as f64 + 1.0,
                        score: 1.0,
                    })
                    .collect(),
                source_path: Some(media_path.clone()),
            }],
        };

        // The file *is* the state: nothing written yet reads as `idle`.
        assert_eq!(
            app.platform_subtitle_state(&session.id, &session.media_path)["state"],
            "idle"
        );

        // An undubbed session needs no audio — its cues already carry the right
        // timings — so the export is a pure text-and-timing job.
        let out = write_platform_subtitle(&session.cues, None, &session.media_path).unwrap();
        assert_eq!(out, dir.join("lecture.srt").to_string_lossy().into_owned());
        // The segmented subtitle is a different file for a different audience.
        assert!(!dir.join("lecture_full_sentence.srt").exists());

        let srt = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = srt
            .lines()
            .filter(|l| {
                !l.trim().is_empty()
                    && !l.contains("-->")
                    && !l.trim().chars().all(|c| c.is_ascii_digit())
            })
            .collect();
        assert!(lines.len() > 1, "expected the sentence to be split: {srt}");
        for line in &lines {
            assert!(
                line.chars().count() <= crate::platform::DEFAULT_MAX_CHARS,
                "too long for one line: {line:?}"
            );
        }
        // Split, not rewritten: every word is still there, once.
        assert_eq!(lines.join(" "), text);

        let state = app.platform_subtitle_state(&session.id, &session.media_path);
        assert_eq!(state["state"], "done");
        assert_eq!(state["path"], out);

        // A run in flight or one that failed is reported from memory, since a
        // finished export is the file and needs no entry at all.
        app.platform_jobs
            .lock()
            .unwrap()
            .insert("s2".to_string(), "running".to_string());
        assert_eq!(
            app.platform_subtitle_state("s2", "/media/other.mp4")["state"],
            "running"
        );
        app.platform_jobs
            .lock()
            .unwrap()
            .insert("s3".to_string(), "failed: no engine".to_string());
        let failed = app.platform_subtitle_state("s3", "/media/other.mp4");
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"], "no engine");
    }

    /// The muxed folder is opened on the *server* machine, so the endpoint is
    /// gated like the file picker. The success branch is deliberately not
    /// exercised — it would launch a real file manager wherever the suite runs.
    #[test]
    fn open_muxed_dir_guards_itself() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-open-dir-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::Db::open(&dir.join("test.db")).expect("open db");
        let app = AppState::new(dir.join("cache"), dir.join("data"), Arc::new(db));

        let session = Session {
            id: "s1".to_string(),
            media_path: "/media/lecture.mp4".to_string(),
            media_duration: 1.0,
            source_format: "srt".to_string(),
            cues: Vec::new(),
        };
        let req = |method: &str, origin: Option<&str>| {
            let mut headers = HashMap::new();
            if let Some(o) = origin {
                headers.insert("origin".to_string(), o.to_string());
            }
            Request {
                method: method.to_string(),
                path: "/api/session/s1/open-muxed-dir".to_string(),
                query: HashMap::new(),
                headers,
                body: Vec::new(),
            }
        };

        // Only a POST may open one.
        assert_eq!(app.open_muxed_dir(&session, &req("GET", None)).status, 404);

        // A page from another origin must not be able to pop a window open.
        assert_eq!(
            app.open_muxed_dir(&session, &req("POST", Some("https://evil.example")))
                .status,
            403
        );

        // Nothing muxed yet → nothing to reveal.
        assert_eq!(app.open_muxed_dir(&session, &req("POST", None)).status, 404);
    }
}
