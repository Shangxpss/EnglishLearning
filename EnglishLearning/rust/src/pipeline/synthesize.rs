//! **Step 2 of the replace-audio pipeline — generate a new audio track from
//! the sentence cues produced by [`crate::pipeline::transcribe`].**
//!
//! Composable, reusable pieces:
//!
//! * [`TtsBackend`] — the pluggable speech-synthesis contract. Swap in any
//!   engine (`espeak-ng` command, `edge-tts`, an in-process model) without
//!   touching the rest of the pipeline.
//! * [`synthesize_cues`] — two-pass, concurrent synthesis: one call per
//!   *sentence* (never per word — see below), then a second pass that
//!   re-synthesises only the sentences that overflow their room, at a faster
//!   rate.
//! * [`synthesize_cues_checkpointed`] — the same rule with the two properties a
//!   long network-bound job needs: every sentence is **written to disk as it
//!   succeeds**, so an interrupted run resumes instead of re-calling the engine
//!   for work already done; and a failing sentence is **retried and then
//!   reported, not fatal**, so one bad sentence only leaves its own slot silent.
//!   This mirrors the Python reference's `ParallelSynthesizer`
//!   (`parallel_synth.py`), for which *"the filesystem IS the checkpoint"*.
//! * [`stitch_timeline`] — place those sentence waveforms onto the media
//!   timeline at each cue's start time, filling gaps with silence.
//! * [`synthesize_track`] — orchestration: synth → stitch → write the WAV.
//!
//! # Timing model (ported from Python's `TTS_SYNC_MECHANISM.md`)
//!
//! Only two of the three timing quantities are controllable, and both are
//! *sentence* level: **when** (the sentence starts at its first aligned word)
//! and **how long** (one speaking rate per sentence). Per-word timing is
//! deliberately not attempted — the reference's word-granular v2 stitcher
//! produced flat prosody and 46 % vocoder clicks, and is documented as a
//! failed design. `wav2vec2` alignment feeds this step by making the
//! sentence's *start* word-accurate.
//!
//! Fitting never uses DSP: an overflowing sentence is **re-synthesised** at a
//! faster rate, not time-stretched, and a sentence shorter than its room is
//! never expanded. See [`synthesize_cues`] for the exact rule.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::logs;
use crate::media;
use crate::models::Cue;
use crate::pipeline::checkpoint;

/// Sentence-level speaking-rate cap: a sentence may be re-synthesised at up to
/// 2× (+100 %), beyond which speech stops being intelligible.
const MAX_SPEEDUP: f64 = 2.0;

/// Overflow tolerance in seconds — TTS duration within this margin of the room
/// is treated as fitting (audio players pad sub-100 ms anyway).
const OVERFLOW_TOLERANCE_SECS: f64 = 0.1;

/// Headroom over [`MIN_CHARS_PER_SEC`] when retrying a sentence the engine read
/// too slowly, so the retry clears the floor instead of re-testing it.
const RATE_FIX_HEADROOM: f64 = 1.05;

/// Bounded synthesis concurrency. The work is network-bound for `edge-tts` and
/// the reference measured 45 s → 8 s for 23 sentences at this width.
const MAX_SYNTH_THREADS: usize = 8;

/// Fade applied at a sentence's start (and, conditionally, its end) to avoid a
/// click at the splice. 5 ms — long enough to remove the click, short enough
/// not to audibly attenuate speech.
const FADE_SECS: f64 = 0.005;

/// Target RMS for per-sentence loudness matching: −20 dBFS ≈ −20 LUFS for
/// speech (the broadcast standard). Engines vary ±6 dB between sentences;
/// normalising makes the dub sound like one consistent speaker.
const TARGET_RMS: f32 = 0.1; // 10^(-20/20)

/// Cap on the loudness gain (10× / +20 dB) so near-silent output is not
/// amplified into noise.
const MAX_LOUDNESS_GAIN: f32 = 10.0;

/// How many extra attempts a failed sentence gets before the run gives up on it
/// and moves on. Ported from the Python reference's `max_retries = 2` — the
/// retries are what make a flaky network call survivable, and giving up is what
/// keeps one bad sentence from aborting a 4-minute lecture.
pub const MAX_SYNTH_RETRIES: usize = 2;

/// Integrity bounds for one synthesized sentence, ported from the Python
/// reference's `validate_audio` (`parallel_synth.py`). A sentence shorter than
/// [`MIN_SENTENCE_SECS`] or longer than [`MAX_SENTENCE_SECS`] is almost
/// certainly truncated or corrupt.
const MIN_SENTENCE_SECS: f64 = 0.3;
const MAX_SENTENCE_SECS: f64 = 60.0;

/// Peak below this is treated as near-silent (−60 dBFS): an engine that returns
/// an empty or dead clip has failed, and its slot should be retried.
const MIN_PEAK: f32 = 0.001;

/// Spoken English is roughly 12–16 chars/sec. A deliberately wide band (the
/// reference uses the same 5–30) flags obviously broken audio without
/// false-positiving on unusually fast or slow sentences.
const MIN_CHARS_PER_SEC: f64 = 5.0;
const MAX_CHARS_PER_SEC: f64 = 30.0;

/// Is `samples` a plausible rendering of `text`? `Err` carries the reason, so a
/// skipped sentence reports *why* rather than just disappearing.
///
/// This is the Rust port of the reference's `validate_audio`. It is applied to
/// freshly synthesized audio as well as to cached clips — the engine can return
/// a truncated or silent file without reporting an error, and a cache written
/// from such a clip must not be trusted either.
pub fn validate_sentence(samples: &[f32], sample_rate: u32, text: &str) -> Result<(), String> {
    let dur = if sample_rate > 0 {
        samples.len() as f64 / sample_rate as f64
    } else {
        0.0
    };
    if dur < MIN_SENTENCE_SECS {
        return Err(format!("too short ({dur:.2}s < {MIN_SENTENCE_SECS}s)"));
    }
    if dur > MAX_SENTENCE_SECS {
        return Err(format!("too long ({dur:.2}s > {MAX_SENTENCE_SECS}s)"));
    }
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak < MIN_PEAK {
        return Err(format!("near-silent (peak={peak:.4})"));
    }

    // Char-rate sanity. The ceiling is widened by [`MAX_SPEEDUP`] because a
    // cached clip may be a pass-2 re-synthesis: at 2× the same sentence takes
    // half the time, so its chars/sec legitimately doubles. The floor needs no
    // such treatment — speeding a sentence up only ever raises its rate.
    let n_chars = text.trim().chars().count();
    if n_chars > 10 {
        let chars_per_sec = n_chars as f64 / dur;
        if chars_per_sec > MAX_CHARS_PER_SEC * MAX_SPEEDUP {
            return Err(format!("too fast ({chars_per_sec:.1} chars/s)"));
        }
        if chars_per_sec < MIN_CHARS_PER_SEC {
            return Err(format!("too slow ({chars_per_sec:.1} chars/s)"));
        }
    }
    Ok(())
}

/// How far a checked-and-retried synthesis run got, for a progress endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SynthProgress {
    /// Sentences that have usable audio — reused from the checkpoint, or
    /// synthesized successfully in this run.
    pub done: usize,
    /// Sentences in the session.
    pub total: usize,
}

/// The sentences that never produced usable audio, and why.
#[derive(Debug, Clone, Default)]
pub struct SynthFailures {
    /// `(cue index, reason)`, in cue order.
    pub failed: Vec<(usize, String)>,
    /// Sentences produced (or reused) successfully in this run.
    pub synthesized: usize,
}

/// The speech-synthesis contract. A backend renders `text` into interleaved
/// f32 mono samples at [`Self::sample_rate`].
///
/// `speedup` is a speaking-rate multiplier (`1.0` = the engine's natural
/// rate). Backends translate it into their own units (`espeak-ng` words per
/// minute, `edge-tts` `--rate=+N%`).
pub trait TtsBackend: Send + Sync {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn synthesize(&self, text: &str, speedup: f64) -> Result<Vec<f32>, String>;
}

/// Room a sentence may occupy: its own span plus the pause that follows it.
///
/// The pause is genuine silence between this sentence's end and the next
/// sentence's start, so overflowing into it does not collide with anything.
/// The **last** cue has no following sentence, so its pause would run past the
/// end of the media and is not counted.
fn available_room(cues: &[Cue], i: usize) -> f64 {
    let cue = &cues[i];
    let slot = (cue.end - cue.start).max(0.0);
    match cues.get(i + 1) {
        Some(next) => slot + cue.pause_after(next.start),
        None => slot,
    }
}

/// The speedup that fits `tts_secs` into `room`, capped at [`MAX_SPEEDUP`].
/// Always `>= 1.0` — audio that already fits is never sped up.
fn speedup_for(tts_secs: f64, room: f64) -> f64 {
    if room <= 0.0 || tts_secs <= room {
        return 1.0;
    }
    (tts_secs / room).clamp(1.0, MAX_SPEEDUP)
}

/// The rate that lifts a clip over [`MIN_CHARS_PER_SEC`] when the engine read its
/// text too slowly.
///
/// Deliberately not [`speedup_for`]: that one fits a clip into its cue's room,
/// and a too-slow clip usually *does* fit its room — the rate floor is about the
/// text-to-audio ratio, not about the timeline. A little headroom keeps the
/// retry from landing exactly on the floor and being rejected again.
fn rate_fix_speedup(dur: f64, text: &str) -> f64 {
    let n_chars = text.trim().chars().count();
    if dur <= 0.0 || n_chars == 0 {
        return 1.0;
    }
    let chars_per_sec = n_chars as f64 / dur;
    if chars_per_sec >= MIN_CHARS_PER_SEC {
        return 1.0;
    }
    (MIN_CHARS_PER_SEC * RATE_FIX_HEADROOM / chars_per_sec).clamp(1.0, MAX_SPEEDUP)
}

/// Two-pass synthesis of every cue, one TTS call per **sentence**.
///
/// * **Pass 1** synthesises each cue at its natural rate, concurrently.
/// * **Pass 2** re-synthesises only the cues that overflow
///   [`available_room`] at the rate that fits. Note that the **last** cue has
///   no following sentence, so only its own span counts as room — it is not
///   exempt from re-synthesis. If a re-synthesis fails, the pass-1 audio is
///   kept.
///
/// Cues that fail to synthesise at all are absent from the result (their slot
/// stays silent), so one bad sentence never aborts the track.
///
/// This is the no-frills form — no checkpoint, one attempt, no validation — and
/// is what the unit tests drive. The server uses
/// [`synthesize_cues_checkpointed`].
#[allow(dead_code)] // only the tests call the no-checkpoint form
pub fn synthesize_cues(backend: &dyn TtsBackend, cues: &[Cue]) -> Vec<(usize, Vec<f32>)> {
    synthesize_cues_inner(backend, cues, SynthOptions::default(), &mut |_| {}).0
}

/// [`synthesize_cues`] plus the two things a long, network-bound job needs:
/// **checkpointing** and **retries**.
///
/// * Every sentence that comes out valid is written to `checkpoint_dir` as
///   `sent_NNNN_<texthash>.wav` *before* the next attempt, so an interrupted run
///   resumes: sentences already on disk are not re-sent to the engine. The text
///   hash in the name means an edited sentence is simply not found, i.e. the
///   checkpoint invalidates itself.
/// * A sentence the engine fails (or that fails validation) is retried up to
///   `max_retries` extra times, then reported in [`SynthFailures`] rather than
///   aborting the run — exactly the reference's `max_retries = 2` behaviour.
///
/// `on_progress` is called as each sentence becomes usable, from whichever
/// worker produced it.
pub fn synthesize_cues_checkpointed(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    checkpoint_dir: Option<&Path>,
    max_retries: usize,
    on_progress: impl FnMut(SynthProgress) + Send,
) -> (Vec<(usize, Vec<f32>)>, SynthFailures) {
    let mut on_progress = on_progress;
    synthesize_cues_inner(
        backend,
        cues,
        SynthOptions {
            checkpoint_dir,
            max_retries,
            validate: true,
        },
        &mut on_progress,
    )
}

/// What [`synthesize_cues_inner`] is allowed to do. [`SynthOptions::default`]
/// reproduces the historical behaviour exactly (no on-disk state, one attempt,
/// no validation) so `synthesize_cues` stays a faithful thin wrapper.
#[derive(Default)]
struct SynthOptions<'a> {
    checkpoint_dir: Option<&'a Path>,
    max_retries: usize,
    validate: bool,
}

fn synthesize_cues_inner(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    opts: SynthOptions<'_>,
    on_progress: &mut (dyn FnMut(SynthProgress) + Send),
) -> (Vec<(usize, Vec<f32>)>, SynthFailures) {
    let sample_rate = backend.sample_rate().max(1);
    let sr = sample_rate as f64;

    // ---- Reuse the checkpoint, and list the sentences still to do ----
    let mut audio: Vec<(usize, Vec<f32>)> = Vec::new();
    let mut work: Vec<(usize, &str, f64)> = Vec::new();
    for (i, cue) in cues.iter().enumerate() {
        if let Some(dir) = opts.checkpoint_dir {
            let path = checkpoint_path(dir, i, &cue.text);
            if path.is_file() {
                match load_checkpoint(&path, sample_rate) {
                    Ok(samples) if !opts.validate => {
                        audio.push((i, samples));
                        continue;
                    }
                    Ok(samples) => match validate_sentence(&samples, sample_rate, &cue.text) {
                        Ok(()) => {
                            audio.push((i, samples));
                            continue;
                        }
                        Err(reason) => {
                            // A cached clip that no longer passes is worse than
                            // no cache at all: it would be stitched as-is.
                            println!(
                                "sentence-video: cached sentence #{} failed validation ({reason}) — re-synthesizing",
                                i + 1
                            );
                            let _ = std::fs::remove_file(&path);
                        }
                    },
                    Err(_) => {
                        // Unreadable/truncated cache — same treatment.
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
        work.push((i, cue.text.as_str(), 1.0));
    }

    let total = cues.len();
    if !audio.is_empty() {
        logs::debug(
            "dub",
            "reused synthesized sentence clips from the checkpoint",
        )
        .detail(format!("{} of {} sentence(s)", audio.len(), total))
        .send();
    }
    let mut failures = SynthFailures::default();
    // Report where we already are before the first worker starts.
    on_progress(SynthProgress {
        done: audio.len(),
        total,
    });
    let on_one_more = progress_ticker(audio.len(), total, on_progress);

    // ---- Pass 1: synthesize the rest, retrying only what still fails ----
    let mut reasons: Vec<(usize, String)> = Vec::new();
    for attempt in 0..=opts.max_retries {
        if work.is_empty() {
            break;
        }
        if attempt > 0 {
            println!(
                "sentence-video: retrying {} failed sentence(s) (attempt {}/{})",
                work.len(),
                attempt + 1,
                opts.max_retries + 1
            );
        }
        let results = run_concurrent(backend, &work, &on_one_more);
        let mut still_bad: Vec<(usize, &str, f64)> = Vec::new();
        reasons.clear();
        for (i, res) in results {
            let text = cues[i].text.as_str();
            // `Err` carries the failing clip's duration when there was one to
            // measure, so a clip the engine simply read too slowly can be retried
            // at a faster rate instead of failing the same way every run.
            let outcome: Result<Vec<f32>, (String, Option<f64>)> = match res {
                Ok(samples) if opts.validate => {
                    match validate_sentence(&samples, sample_rate, text) {
                        Ok(()) => Ok(samples),
                        Err(e) => {
                            let dur = samples.len() as f64 / sr;
                            Err((format!("validation failed: {e}"), Some(dur)))
                        }
                    }
                }
                Ok(samples) => Ok(samples),
                Err(e) => Err((e, None)),
            };
            match outcome {
                Ok(samples) => {
                    if let Some(dir) = opts.checkpoint_dir {
                        if let Err(e) =
                            save_checkpoint(dir, i, text, &samples, sample_rate, backend.channels())
                        {
                            // Losing a checkpoint costs work next time, not
                            // correctness — never fail the sentence for it.
                            eprintln!("sentence-video: cannot checkpoint sentence #{}: {e}", i + 1);
                        }
                    }
                    logs::debug("dub", format!("synthesized sentence #{}", i + 1))
                        .detail(format!(
                            "{:.2}s · {} char(s)",
                            samples.len() as f64 / sr,
                            text.chars().count()
                        ))
                        .send();
                    audio.push((i, samples));
                }
                Err((e, dur)) => {
                    if opts.validate {
                        println!(
                            "sentence-video: sentence #{} failed on attempt {}: {e}",
                            i + 1,
                            attempt + 1
                        );
                    }
                    logs::debug("dub", format!("sentence #{} failed to synthesize", i + 1))
                        .detail(format!("attempt {} · {e}", attempt + 1))
                        .send();
                    // Pass 2 never sees a rejected sentence — it only speeds up
                    // clips that *passed* — so a sentence the engine read below
                    // the rate floor must be retried faster here, or it fails
                    // identically on every resume. A clip that is broken for any
                    // other reason (silent, truncated, absurd) keeps its natural
                    // rate: `rate_fix_speedup` only lifts a clip whose only
                    // defect is that it is too slow for its text.
                    let speedup = dur.map_or(1.0, |d| rate_fix_speedup(d, text));
                    if speedup > 1.0 {
                        println!(
                            "sentence-video: sentence #{} too slow for its text — retrying at {:.2}x",
                            i + 1,
                            speedup
                        );
                    }
                    reasons.push((i, e));
                    still_bad.push((i, text, speedup));
                }
            }
        }
        work = still_bad;
    }
    failures.synthesized = audio.len();
    let mut reasons = reasons;
    reasons.sort_by_key(|(i, _)| *i);
    failures.failed = reasons;

    // ---- Pass 2: re-synthesise the sentences that overflow their room ----
    let mut respeed: Vec<(usize, &str, f64)> = Vec::new();
    for (i, samples) in &audio {
        let tts_secs = samples.len() as f64 / sr;
        let room = available_room(cues, *i);
        if tts_secs <= room + OVERFLOW_TOLERANCE_SECS {
            continue;
        }
        let speedup = speedup_for(tts_secs, room);
        if speedup > 1.0 {
            println!(
                "sentence-video: cue #{} tts={:.2}s room={:.2}s → resynthesising at {:.2}x",
                i + 1,
                tts_secs,
                room,
                speedup
            );
            respeed.push((*i, cues[*i].text.as_str(), speedup));
        }
    }

    if !respeed.is_empty() {
        for (i, res) in run_concurrent(backend, &respeed, &|| {}) {
            if let Ok(samples) = res {
                if let Some(entry) = audio.iter_mut().find(|(idx, _)| *idx == i) {
                    entry.1 = samples;
                    // Overwrite the checkpoint in place. One file per cue is
                    // idempotent: a resumed run measures the clip and re-derives
                    // the rate with `speedup_for`, and a clip that already fits
                    // is never sped up a second time.
                    if let Some(dir) = opts.checkpoint_dir {
                        if let Err(e) = save_checkpoint(
                            dir,
                            i,
                            cues[i].text.as_str(),
                            &entry.1,
                            sample_rate,
                            backend.channels(),
                        ) {
                            eprintln!("sentence-video: cannot checkpoint sentence #{}: {e}", i + 1);
                        }
                    }
                }
            }
        }
    }

    audio.sort_by_key(|(i, _)| *i);
    (audio, failures)
}

/// A `Fn()` that reports `done = start + 1, 2, …` up to `total` on every call,
/// safe to share between the synthesis workers.
fn progress_ticker<'a>(
    start: usize,
    total: usize,
    on_progress: &'a mut (dyn FnMut(SynthProgress) + Send),
) -> impl Fn() + Sync + 'a {
    let counter = std::sync::atomic::AtomicUsize::new(start);
    let sink = std::sync::Mutex::new(on_progress);
    move || {
        // Clamped: a sentence that fails one attempt and is retried is counted
        // again, so without this the progress would read "24 of 23" in the UI.
        let done = (counter.fetch_add(1, Ordering::Relaxed) + 1).min(total);
        if let Ok(mut report) = sink.lock() {
            report(SynthProgress { done, total });
        }
    }
}

/// Where one sentence's audio is checkpointed: `sent_0007_1f3a9c02.wav`.
///
/// The text hash is part of the name, so an edited sentence resolves to a
/// **different** file and the stale clip is simply not found — no separate
/// validity check is needed. There is deliberately no rate suffix: see the
/// pass-2 comment in [`synthesize_cues_inner`].
fn checkpoint_path(dir: &Path, index: usize, text: &str) -> PathBuf {
    dir.join(format!(
        "sent_{:04}_{}.wav",
        index,
        checkpoint::short_hash(text)
    ))
}

/// [`checkpoint_path`], exposed so the server can report how many sentences are
/// already checkpointed — the Manage page's "resume from here" progress — without
/// duplicating the naming scheme.
pub fn sentence_checkpoint_path(dir: &Path, index: usize, text: &str) -> PathBuf {
    checkpoint_path(dir, index, text)
}

fn load_checkpoint(path: &Path, sample_rate: u32) -> Result<Vec<f32>, String> {
    media::decode_to_f32(&path.to_string_lossy(), sample_rate, true, None).map(|(s, _)| s)
}

/// Write one sentence's audio into the checkpoint, atomically.
///
/// The WAV is written beside the target and renamed into place, so a crash
/// mid-write can never leave a truncated file that reads as a finished
/// sentence. Same directory, so the rename cannot cross a filesystem boundary.
fn save_checkpoint(
    dir: &Path,
    index: usize,
    text: &str,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = checkpoint_path(dir, index, text);
    let tmp = path.with_extension("wav.part");
    media::write_wav(&tmp.to_string_lossy(), samples, sample_rate, channels)?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("cannot move {} into place: {e}", tmp.display()))
}

/// Synthesize `jobs` concurrently (bounded by [`MAX_SYNTH_THREADS`]) and return
/// the results in the same order, paired with their cue index.
fn run_concurrent(
    backend: &dyn TtsBackend,
    jobs: &[(usize, &str, f64)],
    on_one_done: &(dyn Fn() + Sync),
) -> Vec<(usize, Result<Vec<f32>, String>)> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let slots: Vec<OnceLock<Result<Vec<f32>, String>>> =
        (0..jobs.len()).map(|_| OnceLock::new()).collect();
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_SYNTH_THREADS)
        .min(jobs.len());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= jobs.len() {
                    break;
                }
                let (_, text, speedup) = jobs[i];
                let _ = slots[i].set(backend.synthesize(text, speedup));
                on_one_done();
            });
        }
    });

    jobs.iter()
        .enumerate()
        .map(|(i, &(idx, _, _))| {
            let res = slots[i]
                .get()
                .cloned()
                .unwrap_or_else(|| Err("synthesis slot was never filled".to_string()));
            (idx, res)
        })
        .collect()
}

/// Place each synthesized segment at its cue's `start` time on the media
/// timeline and return the full mono track as `f32` samples at `sample_rate`.
///
/// This is the pure-Rust equivalent of Python's `PlaceStitcher`; since the
/// audio has already been rate-fitted by [`synthesize_cues`] there is no
/// stretching or boundary truncation here:
///
/// * per-sentence loudness normalisation (RMS → −20 dBFS, gain-capped, with a
///   `tanh` soft-clip if that would clip);
/// * a 5 ms fade-in always, but a fade-out **only** when the audio reaches its
///   slot boundary — an unconditional fade-out attenuates the final consonant;
/// * gaps between cues stay silent.
pub fn stitch_timeline(
    cues: &[Cue],
    segments: &[(usize, Vec<f32>)],
    sample_rate: u32,
    total_duration_secs: f64,
) -> Vec<f32> {
    let sr = sample_rate.max(1) as f64;
    let fade_n = ((FADE_SECS * sr) as usize).max(1);
    // +1 s tail so the last sentence's audio is never clipped by the buffer.
    let total_samples = (total_duration_secs.max(0.0) * sr) as usize + sample_rate.max(1) as usize;
    let mut track = vec![0.0f32; total_samples];

    for (idx, seg) in segments {
        let Some(cue) = cues.iter().find(|c| c.index == *idx) else {
            continue;
        };
        let mut audio = loudness_normalize(seg);

        if audio.len() > 2 * fade_n {
            for i in 0..fade_n {
                audio[i] *= i as f32 / fade_n as f32;
            }
            let start_sample = (cue.start * sr) as usize;
            let slot_end = (cue.end * sr) as usize;
            if start_sample + audio.len() >= slot_end {
                for i in 0..fade_n {
                    let k = audio.len() - fade_n + i;
                    audio[k] *= 1.0 - i as f32 / fade_n as f32;
                }
            }
        }

        let start = (cue.start * sr) as usize;
        let end = (start + audio.len()).min(track.len());
        if start >= end {
            continue;
        }
        track[start..end].copy_from_slice(&audio[..end - start]);
    }

    // Final safety net: a peak-normalise only if the mix clips.
    let peak = track.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.95 {
        let scale = 0.95 / peak;
        for s in track.iter_mut() {
            *s *= scale;
        }
    }
    track
}

/// Per-sentence loudness normalisation: scale to [`TARGET_RMS`], cap the gain,
/// and soft-clip if the result would exceed full scale.
fn loudness_normalize(audio: &[f32]) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    let sum_sq: f64 = audio.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / audio.len() as f64).sqrt() as f32;
    if rms < 1e-5 {
        return audio.to_vec(); // near-silent — leave it alone
    }
    let gain = (TARGET_RMS / rms).min(MAX_LOUDNESS_GAIN);
    let mut out: Vec<f32> = audio.iter().map(|&s| s * gain).collect();
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.99 {
        let k = 0.9f32.tanh();
        for s in out.iter_mut() {
            *s = (*s * 0.9).tanh() / k;
        }
    }
    out
}

/// Full Step-2 orchestration: synthesize every cue, stitch onto the media
/// timeline, and write the result as a WAV track ready for the mux (CLI) or
/// for serving as the replacement audio (server).
///
/// Sentences that could not be synthesized (after [`MAX_SYNTH_RETRIES`] retries)
/// leave their slot silent; the track is still written and the failures are
/// reported on stderr. Only a run that produced *nothing* is an error.
pub fn synthesize_track(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    total_duration_secs: f64,
    out_wav: &str,
) -> Result<(), String> {
    let failures =
        synthesize_track_checkpointed(backend, cues, total_duration_secs, out_wav, None, |_| {})?;
    if !failures.failed.is_empty() {
        eprintln!(
            "sentence-video: {} sentence(s) could not be synthesized (first: #{}, {}); their slots stay silent",
            failures.failed.len(),
            failures.failed[0].0 + 1,
            failures.failed[0].1
        );
    }
    Ok(())
}

/// [`synthesize_track`] with a sentence checkpoint, retries, and progress
/// reporting — what the server's dubbing job uses so an interrupted run can be
/// resumed and polled.
///
/// Returns the sentences that never produced usable audio. The track is written
/// regardless, so one bad sentence costs only its own slot.
pub fn synthesize_track_checkpointed(
    backend: &dyn TtsBackend,
    cues: &[Cue],
    total_duration_secs: f64,
    out_wav: &str,
    checkpoint_dir: Option<&Path>,
    on_progress: impl FnMut(SynthProgress) + Send,
) -> Result<SynthFailures, String> {
    let (segments, failures) = synthesize_cues_checkpointed(
        backend,
        cues,
        checkpoint_dir,
        MAX_SYNTH_RETRIES,
        on_progress,
    );
    if segments.is_empty() {
        return Err("no cues could be synthesized — check the TTS backend".to_string());
    }
    let track = stitch_timeline(cues, &segments, backend.sample_rate(), total_duration_secs);
    media::write_wav(out_wav, &track, backend.sample_rate(), backend.channels())?;
    Ok(failures)
}

/// A [`TtsBackend`] that shells out to an installed TTS command
/// (default `espeak-ng`). It writes each sentence to a temp WAV, decodes it
/// back to f32 mono via the shared media helper, and returns the samples.
/// Fails with a clear message if the command is not installed.
pub struct CommandTts {
    command: String,
    voice: Option<String>,
    rate_words_per_min: i32,
    sample_rate: u32,
}

impl CommandTts {
    pub fn new(command: &str, voice: Option<&str>, rate_words_per_min: i32) -> Self {
        CommandTts {
            command: command.to_string(),
            voice: voice.map(str::to_string),
            rate_words_per_min: rate_words_per_min,
            sample_rate: 44100,
        }
    }

    /// The `espeak-ng` executable name: a bare `espeak` becomes `espeak-ng`.
    fn exe(&self) -> String {
        if self.command.contains('/') {
            self.command.clone()
        } else {
            format!("{}-ng", self.command)
        }
    }

    fn run_to_wav(&self, text: &str, speedup: f64) -> Result<String, String> {
        let exe = self.exe();
        // espeak's rate is words per minute; scale it by the requested speedup
        // and keep it inside the range the engine renders intelligibly.
        let wpm = (self.rate_words_per_min as f64 * speedup).round() as i32;
        let mut cmd = Command::new(&exe);
        cmd.arg("--stdout");
        cmd.arg("-s").arg(wpm.clamp(80, 450).to_string());
        if let Some(v) = &self.voice {
            cmd.arg("-v").arg(v);
        }
        cmd.arg("--").arg(text);

        let out = cmd
            .output()
            .map_err(|e| format!("TTS command '{}' failed: {e} (is it installed?)", exe))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("TTS command '{}' failed: {}", exe, stderr.trim()));
        }
        if out.stdout.is_empty() {
            return Err("TTS command produced no output".to_string());
        }

        // Persist the bytes as WAV and decode with the shared media helper so
        // every backend reuses one decoding path.
        let tmp = temp_path("wav");
        std::fs::write(&tmp, &out.stdout).map_err(|e| format!("cannot write temp wav: {e}"))?;
        Ok(tmp)
    }
}

impl TtsBackend for CommandTts {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        1
    }
    fn synthesize(&self, text: &str, speedup: f64) -> Result<Vec<f32>, String> {
        let wav = self.run_to_wav(text, speedup)?;
        let res = media::decode_to_f32(&wav, self.sample_rate, true, None).map(|(s, _)| s);
        let _ = std::fs::remove_file(&wav);
        res
    }
}

/// A [`TtsBackend`] that drives the externally installed `edge-tts` CLI
/// (Microsoft neural voices, 24 kHz mono). This is the default engine — the
/// other backends need system packages, this one needs only `edge-tts` on
/// `PATH` and a network connection.
///
/// `edge-tts` does **not** honour `https_proxy` for its synthesis WebSocket —
/// the proxy must be passed explicitly (TTS_SYNC_MECHANISM §8, Lesson 5).
pub struct EdgeTts {
    exe: String,
    voice: String,
    proxy: Option<String>,
    sample_rate: u32,
}

impl EdgeTts {
    pub fn new(voice: &str, proxy: Option<&str>) -> Self {
        EdgeTts {
            exe: "edge-tts".to_string(),
            voice: voice.to_string(),
            proxy: proxy.map(str::to_string),
            // Microsoft neural voices are 24 kHz mono.
            sample_rate: 24_000,
        }
    }

    fn run_to_media(&self, text: &str, speedup: f64) -> Result<String, String> {
        // edge-tts expresses speed as a percentage adjustment, e.g. "+50%".
        let percent = ((speedup - 1.0) * 100.0).round().max(0.0) as i32;
        let out_path = temp_path("mp3");
        let mut cmd = Command::new(&self.exe);
        cmd.arg("--voice").arg(&self.voice);
        cmd.arg(format!("--rate=+{percent}%"));
        cmd.arg("--text").arg(text);
        cmd.arg("--write-media").arg(&out_path);
        if let Some(p) = &self.proxy {
            cmd.arg("--proxy").arg(p);
        }

        let out = cmd
            .output()
            .map_err(|e| format!("edge-tts failed to start: {e} (is it installed and on PATH?)"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let _ = std::fs::remove_file(&out_path);
            return Err(format!("edge-tts failed: {}", stderr.trim()));
        }
        Ok(out_path)
    }
}

impl TtsBackend for EdgeTts {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        1
    }
    fn synthesize(&self, text: &str, speedup: f64) -> Result<Vec<f32>, String> {
        let media_path = self.run_to_media(text, speedup)?;
        let res = media::decode_to_f32(&media_path, self.sample_rate, true, None).map(|(s, _)| s);
        let _ = std::fs::remove_file(&media_path);
        res
    }
}

/// A deterministic [`TtsBackend`] used for tests / pipeline wiring checks —
/// it produces a short beep per cue so the stitcher and track serving can be
/// validated end-to-end without a real voice.
pub struct SilenceTts {
    sample_rate: u32,
    beep_hz: f32,
    /// Beep length; scaled down by `speedup` like a real engine, so the
    /// re-synthesis pass is observable without any engine installed.
    beep_secs: f64,
}

impl SilenceTts {
    pub fn new(sample_rate: u32) -> Self {
        SilenceTts {
            sample_rate,
            beep_hz: 440.0,
            beep_secs: 1.0,
        }
    }
}

impl TtsBackend for SilenceTts {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn channels(&self) -> u16 {
        1
    }
    fn synthesize(&self, _text: &str, speedup: f64) -> Result<Vec<f32>, String> {
        let sr = self.sample_rate as f32;
        let n = (self.sample_rate as f64 * self.beep_secs / speedup.max(0.01)) as usize;
        Ok((0..n)
            .map(|i| (2.0 * std::f32::consts::PI * self.beep_hz * i as f32 / sr).sin())
            .collect())
    }
}

/// The default `edge-tts` voice: **English (United Kingdom) — Maisie**, the
/// British female neural voice. Chosen as the project default so learners hear
/// a non-rhotic British accent rather than the US voice `edge-tts` would
/// otherwise fall back to. Override with `--dub-voice` /
/// `SENTENCE_VIDEO_DUB_VOICE`.
pub const DEFAULT_EDGE_VOICE: &str = "en-GB-MaisieNeural";

/// The voice `build_backend` will actually use for `engine` when handed
/// `voice`. Kept in one place so the log line and the UI can report the real
/// default without duplicating it.
pub fn effective_voice<'a>(engine: &str, voice: Option<&'a str>) -> Option<&'a str> {
    match engine {
        "edge-tts" | "edge" => Some(voice.unwrap_or(DEFAULT_EDGE_VOICE)),
        _ => voice,
    }
}

/// Build the backend named by `engine`, checking up front that it is actually
/// runnable so a missing engine is a clear error rather than a silently
/// un-dubbed track.
///
/// Accepted: `espeak` / `espeak-ng`, `edge-tts`, and `silence` (the beep test
/// backend — no engine needed).
pub fn build_backend(
    engine: &str,
    voice: Option<&str>,
    rate_words_per_min: i32,
    proxy: Option<&str>,
) -> Result<Box<dyn TtsBackend>, String> {
    match engine {
        "espeak" | "espeak-ng" => {
            let cmd = CommandTts::new("espeak", voice, rate_words_per_min);
            probe(&cmd.exe(), "install it with `apt install espeak-ng`")?;
            Ok(Box::new(cmd))
        }
        "edge-tts" | "edge" => {
            probe("edge-tts", "install it with `uv tool install edge-tts`")?;
            Ok(Box::new(EdgeTts::new(
                effective_voice(engine, voice).unwrap_or(DEFAULT_EDGE_VOICE),
                proxy,
            )))
        }
        "silence" => Ok(Box::new(SilenceTts::new(44_100))),
        other => Err(format!(
            "unknown TTS engine '{other}' — expected 'espeak-ng', 'edge-tts' or 'silence'"
        )),
    }
}

/// Fail early with an actionable message when `exe` is not on `PATH`.
fn probe(exe: &str, hint: &str) -> Result<(), String> {
    match Command::new(exe).arg("--version").output() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("{exe} not found — {hint}"))
        }
        Err(e) => Err(format!("{exe} could not be run: {e}")),
    }
}

/// Per-call temp-file counter. Synthesis runs on several threads at once, so a
/// name derived from the process id alone would let two workers write, decode
/// and delete the same file — one sentence would then be stitched from another
/// sentence's (possibly half-written) audio.
static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A temp path unique to this process **and** this call.
fn temp_path(ext: &str) -> String {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "sentence-video-tts-{}-{n}.{ext}",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the speedup of every call so the two-pass rule can be asserted
    /// without an engine. Each call returns a tone of a fixed length, scaled by
    /// the requested speedup (like a real engine).
    struct RecordingTts {
        calls: std::sync::Mutex<Vec<(String, f64)>>,
        secs: f64,
        sample_rate: u32,
    }

    impl RecordingTts {
        fn new(secs: f64, sample_rate: u32) -> Self {
            RecordingTts {
                calls: std::sync::Mutex::new(Vec::new()),
                secs,
                sample_rate,
            }
        }
        fn recorded(&self) -> Vec<(String, f64)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl TtsBackend for RecordingTts {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn channels(&self) -> u16 {
            1
        }
        fn synthesize(&self, text: &str, speedup: f64) -> Result<Vec<f32>, String> {
            self.calls.lock().unwrap().push((text.to_string(), speedup));
            let n = (self.sample_rate as f64 * self.secs / speedup) as usize;
            Ok(vec![0.5f32; n])
        }
    }

    fn cue(i: usize, start: f64, end: f64, text: &str) -> Cue {
        Cue {
            index: i,
            start,
            end,
            text: text.to_string(),
            words: vec![],
            source_path: None,
        }
    }

    /// Like [`RecordingTts`] but it can fail: a text is refused until it has
    /// been tried `fail_first` times, and the texts in `always_fail` never
    /// succeed. Stands in for a flaky or dead engine.
    struct FlakyTts {
        calls: std::sync::Mutex<Vec<(String, f64)>>,
        attempts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
        fail_first: usize,
        always_fail: Vec<String>,
        secs: f64,
        sample_rate: u32,
    }

    impl FlakyTts {
        fn new(fail_first: usize, secs: f64, sample_rate: u32) -> Self {
            FlakyTts {
                calls: std::sync::Mutex::new(Vec::new()),
                attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
                fail_first,
                always_fail: Vec::new(),
                secs,
                sample_rate,
            }
        }
        /// Succeeds for everything except the listed sentences.
        fn always_failing(texts: &[&str], secs: f64, sample_rate: u32) -> Self {
            let mut t = FlakyTts::new(0, secs, sample_rate);
            t.always_fail = texts.iter().map(|s| s.to_string()).collect();
            t
        }
        fn recorded(&self) -> Vec<(String, f64)> {
            self.calls.lock().unwrap().clone()
        }
        fn calls_for(&self, text: &str) -> usize {
            self.recorded().iter().filter(|(t, _)| t == text).count()
        }
    }

    impl TtsBackend for FlakyTts {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
        fn channels(&self) -> u16 {
            1
        }
        fn synthesize(&self, text: &str, speedup: f64) -> Result<Vec<f32>, String> {
            self.calls.lock().unwrap().push((text.to_string(), speedup));
            if !self.always_fail.iter().any(|t| t == text) {
                let mut attempts = self.attempts.lock().unwrap();
                let seen = attempts.entry(text.to_string()).or_insert(0);
                if *seen >= self.fail_first {
                    let n = (self.sample_rate as f64 * self.secs / speedup) as usize;
                    return Ok(vec![0.5f32; n]);
                }
                *seen += 1;
            }
            Err("engine hiccup".to_string())
        }
    }

    /// A private directory per test so parallel tests cannot collide.
    fn tmp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sv-synth-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn synthetic_beeps_land_at_cue_times() {
        let cues = vec![cue(0, 1.0, 2.0, "one."), cue(1, 3.0, 4.0, "two.")];
        let backend = SilenceTts::new(1000);
        let track = stitch_timeline(&cues, &synthesize_cues(&backend, &cues), 1000, 5.0);
        // The gap region before 1.0 s must be silent.
        assert!(track[500].abs() < 1e-6);
        // Cue 0 starts exactly at 1.0 s (after the 5 ms fade-in's first sample).
        assert!(track[1000].abs() < 1e-6);
        assert!(track[1010].abs() > 1e-3);
    }

    #[test]
    fn sentence_inside_its_room_is_never_speed_up() {
        // 1 s of audio in a 2 s slot: fits, so exactly one call at rate 1.0.
        let cues = vec![cue(0, 0.0, 2.0, "fits.")];
        let backend = RecordingTts::new(1.0, 1000);
        let segments = synthesize_cues(&backend, &cues);
        assert_eq!(segments.len(), 1);
        assert_eq!(backend.recorded(), vec![("fits.".to_string(), 1.0)]);
    }

    #[test]
    fn sentence_colliding_with_the_next_cue_is_resynthesised() {
        // 4 s of audio, 2 s slot, no pause before cue 2 → needs 2×.
        let cues = vec![cue(0, 0.0, 2.0, "long."), cue(1, 2.0, 4.0, "next.")];
        let backend = RecordingTts::new(4.0, 1000);
        let segments = synthesize_cues(&backend, &cues);
        assert_eq!(segments.len(), 2);
        let calls = backend.recorded();
        assert!(calls.contains(&("long.".to_string(), 2.0)), "got {calls:?}");
        // "next." fits its own 2 s slot, so it is never re-synthesised.
        assert!(calls.contains(&("next.".to_string(), 1.0)));
        assert_eq!(calls.iter().filter(|(t, _)| t == "long.").count(), 2);
    }

    #[test]
    fn the_pause_after_a_cue_is_usable_room() {
        // 3 s of audio, 2 s slot + 1.5 s pause → fits, no re-synthesis.
        let cues = vec![cue(0, 0.0, 2.0, "pause."), cue(1, 3.5, 5.0, "next.")];
        let backend = RecordingTts::new(3.0, 1000);
        synthesize_cues(&backend, &cues);
        let calls = backend.recorded();
        assert_eq!(calls.iter().filter(|(t, _)| t == "pause.").count(), 1);
    }

    #[test]
    fn the_last_cue_gets_no_pause_room() {
        // Cue 0 has a 2 s pause after it, so 2.5 s of audio fits in its 3 s of
        // room and is left alone. The final cue has nothing after it, so only
        // its own 1 s slot counts — the same audio must be sped up to fit.
        let cues = vec![cue(0, 0.0, 1.0, "first."), cue(1, 3.0, 4.0, "last.")];
        let backend = RecordingTts::new(2.5, 1000);
        synthesize_cues(&backend, &cues);
        let calls = backend.recorded();
        assert_eq!(
            calls.iter().filter(|(t, _)| t == "first.").count(),
            1,
            "got {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|(t, _)| t == "last.").count(),
            2,
            "got {calls:?}"
        );
    }

    #[test]
    fn a_short_sentence_is_zero_padded_and_not_stretched() {
        // 0.1 s of audio in a 1 s slot: silence fills the rest.
        let cues = vec![cue(0, 0.0, 1.0, "short.")];
        let backend = RecordingTts::new(0.1, 1000);
        let segments = synthesize_cues(&backend, &cues);
        let track = stitch_timeline(&cues, &segments, 1000, 1.0);
        assert!(track[500].abs() < 1e-6, "tail must stay silent");
    }

    #[test]
    fn loudness_is_matched_across_sentences() {
        // A quiet sentence is lifted towards the −20 dBFS target.
        let quiet = vec![0.01f32; 1000];
        let out = loudness_normalize(&quiet);
        let rms = (out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
        assert!((rms - TARGET_RMS as f64).abs() < 0.01, "rms = {rms}");
    }

    #[test]
    fn fade_out_is_skipped_when_the_audio_ends_inside_its_slot() {
        // 0.5 s of audio in a 2 s slot: it ends well before the boundary, so
        // the final sample must NOT be faded to silence.
        let cues = vec![cue(0, 0.0, 2.0, "fits.")];
        let backend = RecordingTts::new(0.5, 1000);
        let segments = synthesize_cues(&backend, &cues);
        let track = stitch_timeline(&cues, &segments, 1000, 2.0);
        assert!(track[499].abs() > 1e-3, "final sample was faded out");
    }

    #[test]
    fn unknown_engine_is_a_clear_error() {
        let err = match build_backend("festival", None, 170, None) {
            Ok(_) => panic!("an unknown engine must not build a backend"),
            Err(e) => e,
        };
        assert!(err.contains("unknown TTS engine"), "got {err}");
    }

    #[test]
    fn concurrent_calls_never_share_a_temp_file() {
        // Synthesis runs on several threads, each shelling out to the engine and
        // handing the output over through a temp file. A per-process name would
        // make two workers clobber each other.
        let names: Vec<String> = (0..16).map(|_| temp_path("mp3")).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "temp paths collide: {names:?}");
    }

    #[test]
    fn a_valid_checkpoint_is_reused_without_asking_the_engine_again() {
        let dir = tmp_dir("resume");
        let cues = vec![cue(0, 0.0, 2.0, "fits.")];

        let first = RecordingTts::new(1.0, 1000);
        let (segments, failures) =
            synthesize_cues_checkpointed(&first, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert_eq!(failures.failed, vec![]);
        assert_eq!(segments.len(), 1);
        assert_eq!(first.recorded().len(), 1);

        // Resuming must not re-send a sentence the checkpoint already holds.
        let resumed = RecordingTts::new(1.0, 1000);
        let (segments, failures) =
            synthesize_cues_checkpointed(&resumed, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert!(
            resumed.recorded().is_empty(),
            "a cache hit must cost zero engine calls, got {:?}",
            resumed.recorded()
        );
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].1.len(), 1000, "the cached clip is used as-is");
        assert_eq!(failures.synthesized, 1);
    }

    #[test]
    fn an_edited_sentence_does_not_reuse_the_stale_clip() {
        let dir = tmp_dir("edited");
        let cues = vec![cue(0, 0.0, 2.0, "first version.")];
        let first = RecordingTts::new(1.0, 1000);
        synthesize_cues_checkpointed(&first, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});

        // Same cue position, different text: the checkpoint must not match.
        let edited = vec![cue(0, 0.0, 2.0, "second version.")];
        let resumed = RecordingTts::new(1.0, 1000);
        synthesize_cues_checkpointed(&resumed, &edited, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert_eq!(
            resumed.recorded(),
            vec![("second version.".to_string(), 1.0)],
            "an edited sentence must be re-synthesized"
        );
    }

    #[test]
    fn a_flaky_sentence_is_retried_until_it_succeeds() {
        let dir = tmp_dir("flaky");
        let cues = vec![cue(0, 0.0, 2.0, "fits.")];
        // Fails twice, so it succeeds on the third and final attempt.
        let backend = FlakyTts::new(MAX_SYNTH_RETRIES, 1.0, 1000);
        let (segments, failures) =
            synthesize_cues_checkpointed(&backend, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert_eq!(failures.failed, vec![], "the retry must have rescued it");
        assert_eq!(segments.len(), 1);
        assert_eq!(backend.calls_for("fits."), MAX_SYNTH_RETRIES + 1);
    }

    #[test]
    fn a_permanently_failing_sentence_is_reported_but_the_track_is_still_written() {
        let dir = tmp_dir("always-fails");
        let cues = vec![cue(0, 0.0, 1.0, "good."), cue(1, 2.0, 3.0, "bad.")];
        let backend = FlakyTts::always_failing(&["bad."], 1.0, 1000);
        let out = dir.join("track.wav");

        let failures = synthesize_track_checkpointed(
            &backend,
            &cues,
            4.0,
            &out.to_string_lossy(),
            Some(&dir),
            |_| {},
        )
        .expect("one bad sentence must not abort the track");

        assert_eq!(failures.failed.len(), 1);
        assert_eq!(failures.failed[0].0, 1);
        assert!(failures.failed[0].1.contains("engine hiccup"));
        assert!(out.is_file(), "the track must still be written");
        // Tried once plus one retry per MAX_SYNTH_RETRIES.
        assert_eq!(backend.calls_for("bad."), MAX_SYNTH_RETRIES + 1);
        assert_eq!(backend.calls_for("good."), 1);
    }

    /// The case that used to fail on every resume: a short sentence the engine
    /// reads just under the rate floor, inside a cue room it comfortably fits.
    /// Pass 2 only speeds up clips that passed, so the repair has to happen in
    /// pass 1's retry — otherwise the identical failure repeats forever.
    #[test]
    fn a_sentence_the_engine_reads_too_slowly_is_retried_faster() {
        let dir = tmp_dir("too-slow");
        // "On one CPU." is 11 characters: its 2.3 s reading is 4.8 chars/s, just
        // under `MIN_CHARS_PER_SEC`, while the cue's own 3.46 s slot is room
        // enough — so nothing but the rate floor can reject it.
        let text = "On one CPU.";
        let cues = vec![cue(0, 0.0, 3.46, text)];
        let backend = RecordingTts::new(2.3, 1000);

        let (segments, failures) =
            synthesize_cues_checkpointed(&backend, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});

        assert!(
            failures.failed.is_empty(),
            "the retry must rescue it, got {:?}",
            failures.failed
        );
        assert_eq!(segments.len(), 1);

        let calls = backend.recorded();
        assert_eq!(
            calls.len(),
            2,
            "one natural attempt, one faster, got {calls:?}"
        );
        assert_eq!(calls[0].1, 1.0);
        assert!(calls[1].1 > 1.0, "the retry must be faster, got {calls:?}");

        // The clip that is kept must clear the floor, not just the one that was
        // thrown away.
        let kept_secs = segments[0].1.len() as f64 / 1000.0;
        let rate = text.chars().count() as f64 / kept_secs;
        assert!(
            rate >= MIN_CHARS_PER_SEC,
            "kept clip is still too slow ({rate:.2} chars/s)"
        );
    }

    #[test]
    fn a_near_silent_checkpoint_is_rejected_and_re_synthesized() {
        let dir = tmp_dir("silent-cache");
        let cues = vec![cue(0, 0.0, 2.0, "fits.")];
        // A plausible-looking cache entry: real duration, but no sound in it.
        save_checkpoint(&dir, 0, &cues[0].text, &vec![0.0f32; 1000], 1000, 1).unwrap();

        let backend = RecordingTts::new(1.0, 1000);
        let (segments, _) =
            synthesize_cues_checkpointed(&backend, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});

        assert_eq!(
            backend.recorded().len(),
            1,
            "the dead cache must be re-made"
        );
        let peak = segments[0].1.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > MIN_PEAK, "the replacement must actually have audio");
    }

    #[test]
    fn a_resynthesised_sentence_overwrites_its_checkpoint() {
        let dir = tmp_dir("respeed");
        // 4 s of audio in a 2 s slot: it must be re-synthesised at 2×.
        let cues = vec![cue(0, 0.0, 2.0, "long.")];
        let first = RecordingTts::new(4.0, 1000);
        let (segments, _) =
            synthesize_cues_checkpointed(&first, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert_eq!(segments[0].1.len(), 2000, "pass 2 must replace pass 1");

        // One file per cue, no leftover temp — the fitted clip is written under
        // the same name as the pass-1 clip it replaced.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![format!("sent_0000_{}.wav", checkpoint::short_hash("long."))]
        );

        // The stored clip already fits, so a resumed run re-speeds nothing.
        let resumed = RecordingTts::new(4.0, 1000);
        let (segments, _) =
            synthesize_cues_checkpointed(&resumed, &cues, Some(&dir), MAX_SYNTH_RETRIES, |_| {});
        assert!(
            resumed.recorded().is_empty(),
            "a saved pass-2 clip must not be sped up twice, got {:?}",
            resumed.recorded()
        );
        assert_eq!(segments[0].1.len(), 2000);
    }

    #[test]
    fn progress_is_reported_for_every_sentence() {
        let cues = vec![cue(0, 0.0, 1.0, "one."), cue(1, 2.0, 3.0, "two.")];
        let backend = RecordingTts::new(1.0, 1000);
        let mut seen = Vec::new();
        synthesize_cues_checkpointed(&backend, &cues, None, MAX_SYNTH_RETRIES, |p| seen.push(p));
        seen.sort_by_key(|p| p.done);
        // The initial "here is where we start" report, then one per sentence.
        assert_eq!(
            seen,
            vec![
                SynthProgress { done: 0, total: 2 },
                SynthProgress { done: 1, total: 2 },
                SynthProgress { done: 2, total: 2 },
            ]
        );
    }
}
