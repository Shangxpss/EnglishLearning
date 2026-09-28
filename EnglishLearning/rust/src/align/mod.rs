//! wav2vec2-base-960h forced alignment — an experimental, dependency-free
//! alternative to whisper's DTW token timestamps (feature `asr-align`).
//!
//! Why it exists: whisper's DTW timestamps are still ~0.2 s late at phrase
//! boundaries (see `crate::asr`), which the audio-onset snapping only partly
//! fixes. A CTC acoustic model aligned with a Viterbi pass over a *known*
//! transcript is the standard fix (it is what WhisperX does): the transcript
//! comes from whisper, the frame-level evidence comes from wav2vec2.
//!
//! The whole model is hand-written here — see [`wav2vec2`] for why the ONNX
//! runtimes were ruled out — so this feature adds **no dependencies**. The
//! weights are 378 MB, far too large for `include_bytes!`, so unlike the
//! whisper model they are loaded from disk.
//!
//! # Cost and window size
//!
//! Self-attention is quadratic in frames, and this implementation is
//! single-threaded and deliberately naive. Align **segment-sized** audio
//! (whisper's own tens-of-seconds segments), not whole files: a 30 s window is
//! ~1500 frames, while a 10-minute file would be 30 000 frames and would need
//! ~3.6 GB for one layer's score matrix.
//!
//! Timings returned by [`Aligner::align`] are relative to the start of the
//! samples handed in; the caller adds the window's offset.

pub mod ctc;
pub mod safetensors;
pub mod tensor;
pub mod wav2vec2;

use crate::models::Word;
use crate::pipeline::checkpoint;
use crate::pipeline::transcribe::{BuildSink, BuildStage, CANCELLED};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use wav2vec2::{Wav2Vec2, SAMPLES_PER_FRAME};

/// Seconds per output frame (320 samples at 16 kHz, i.e. 20 ms).
///
/// The conv stack's receptive-field centre sits ~12 ms into the first frame,
/// but that is below the 20 ms frame granularity this alignment resolves, so it
/// is not corrected for.
const FRAME_SECS: f64 = SAMPLES_PER_FRAME as f64 / wav2vec2::SAMPLE_RATE as f64;

/// Weights file name looked for next to the executable or in `assets/models/`.
pub const WEIGHTS_FILE: &str = "wav2vec2-base-960h.safetensors";

/// Message shown when alignment is requested without the weights.
pub const NO_WEIGHTS_HELP: &str = "no wav2vec2 alignment weights: pass --align-model <path>, set \
     SENTENCE_VIDEO_ALIGN_MODEL, or place wav2vec2-base-960h.safetensors in assets/models/";

/// Where the alignment weights are, or `None` if nowhere obvious.
///
/// Checked in order: `$SENTENCE_VIDEO_ALIGN_MODEL`, next to the executable, then
/// `assets/models/` relative to the working directory (where the fetch script
/// puts it, so `cargo run` works from `rust/`).
pub fn default_weights_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SENTENCE_VIDEO_ALIGN_MODEL") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.with_file_name(WEIGHTS_FILE);
        if p.is_file() {
            return Some(p);
        }
    }
    let p = PathBuf::from("assets/models").join(WEIGHTS_FILE);
    p.is_file().then_some(p)
}

/// A loaded wav2vec2 acoustic model, ready to align against.
pub struct Aligner {
    model: Wav2Vec2,
}

impl Aligner {
    /// Load the weights (378 MB; this reads and holds them all in memory).
    pub fn load(path: &Path) -> Result<Self, String> {
        Ok(Self {
            model: Wav2Vec2::load(path)?,
        })
    }

    /// Greedy CTC transcript of 16 kHz mono audio.
    ///
    /// This is the sanity check that matters: if this does not read back
    /// roughly what was said, the encoder is wrong and any timing it produces
    /// is meaningless.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        let (logits, frames) = self.model.emissions(samples)?;
        Ok(ctc::greedy_decode(&logits, frames))
    }

    /// Word timings for a known transcript, in seconds from the start of
    /// `samples`.
    ///
    /// Words containing no vocabulary characters (digits, dashes) are omitted
    /// rather than given a fabricated time, so the result may be shorter than
    /// `words`. Scores are the mean per-frame CTC confidence over each word.
    pub fn align(&self, samples: &[f32], words: &[&str]) -> Result<Vec<Word>, String> {
        let encoded = ctc::encode_words(words)?;
        let (logits, frames) = self.model.emissions(samples)?;
        let aligned = ctc::forced_align(&logits, frames, &encoded.tokens)?;
        let confidence = ctc::frame_confidence(&logits, frames);

        let mut out = Vec::with_capacity(encoded.word_ranges.len());
        for range in &encoded.word_ranges {
            // A word spans its first symbol's first frame to its last symbol's
            // last frame.
            let first = aligned[range.start].start;
            let last = aligned[range.end - 1].end;
            let span = &confidence[first..=last];
            out.push(Word {
                text: words[range.word].to_string(),
                start: first as f64 * FRAME_SECS,
                // `last` is inclusive, so the word runs to the end of that frame.
                end: (last + 1) as f64 * FRAME_SECS,
                score: (span.iter().sum::<f32>() / span.len() as f32) as f64,
            });
        }
        Ok(out)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Windowed refinement of whisper's word timings
// ─────────────────────────────────────────────────────────────────────────

/// Longest audio window handed to the encoder, in seconds.
///
/// Self-attention is quadratic in frames, so the cost per second of audio climbs
/// with window length (measured ~5.7 s of CPU per second of audio at 15–20 s,
/// ~12 % more at 30 s). 30 s keeps that overhead small while leaving few windows
/// per minute of video.
pub const WINDOW_SECS: f64 = 30.0;

/// Audio kept on either side of a window so its first and last word keep their
/// onset and release. Whisper's timings run ~0.2 s late, hence more than a frame.
const WINDOW_PAD_SECS: f64 = 0.4;

/// A slice of audio together with the words spoken inside it.
pub struct Window {
    /// Padded window bounds, in media seconds.
    pub start: f64,
    pub end: f64,
    /// Inclusive range of word indices this window covers.
    pub first: usize,
    pub last: usize,
}

/// Group word timings into windows of at most [`WINDOW_SECS`].
///
/// The transcript handed to the encoder must cover exactly the audio it is given
/// (that is what `--start` exposed), so each window carries the words whose
/// whisper timings fall inside it.
pub fn plan_windows(words: &[Word]) -> Vec<Window> {
    let mut windows = Vec::new();
    let mut first = 0;
    while first < words.len() {
        let start = words[first].start;
        let mut last = first;
        while last + 1 < words.len() && words[last + 1].end - start <= WINDOW_SECS {
            last += 1;
        }
        windows.push(Window {
            start: (start - WINDOW_PAD_SECS).max(0.0),
            end: words[last].end + WINDOW_PAD_SECS,
            first,
            last,
        });
        first = last + 1;
    }
    windows
}

/// How the refinement went: windows aligned, and windows left with whisper's
/// timings (a failure there is never fatal).
#[derive(Debug, Default, Clone, Copy)]
pub struct RefineStats {
    pub windows: usize,
    pub aligned: usize,
    pub failed: usize,
}

/// Replace whisper's word timings with wav2vec2 alignment, window by window.
///
/// Windows are independent, so they are spread over the available cores with
/// `std::thread::scope` (no thread-pool dependency). A window that fails — a
/// degenerate transcript, or no audio — is reported and the words in it keep
/// whisper's timings, so one bad window cannot lose the whole file.
///
/// With `checkpoint` set, each window's refined words are written to
/// `checkpoint/w{first}-{last}.json` as soon as it succeeds, and a window whose
/// file is already there is not aligned again. Alignment is the CPU bottleneck
/// here (≈5.7 s of CPU per second of audio), so on a long file this is what
/// makes an interrupted run resumable rather than a restart. The window name is
/// derived from the raw whisper word timings, and the raw words themselves are
/// checkpointed alongside, so a window can only be reused for the exact audio
/// range it was computed from.
pub fn refine_words(
    aligner: &Aligner,
    samples: &[f32],
    words: Vec<Word>,
    checkpoint: Option<&Path>,
    progress: BuildSink<'_>,
) -> Result<(Vec<Word>, RefineStats), String> {
    let windows = plan_windows(&words);
    let mut stats = RefineStats {
        windows: windows.len(),
        ..Default::default()
    };
    if windows.is_empty() {
        return Ok((words, stats));
    }

    // Announced before the first window starts, not after it finishes: a window
    // takes minutes and the stages before this one report no sub-progress, so a
    // sink that only heard about completed windows would keep claiming the file
    // was still being transcribed for the whole of the first one. It is also the
    // first chance to cancel — before any window has burned a core.
    if let Some(sink) = progress {
        let keep_going = sink(BuildStage::Align {
            done: 0,
            total: windows.len(),
        });
        if !keep_going {
            return Err(CANCELLED.to_string());
        }
    }

    let rate = wav2vec2::SAMPLE_RATE as f64;
    let max_span = WINDOW_SECS + 2.0 * WINDOW_PAD_SECS;
    let slots: Vec<OnceLock<Vec<Word>>> = windows.iter().map(|_| OnceLock::new()).collect();
    let next = AtomicUsize::new(0);
    // Windows finish out of order (they are pulled from a shared counter), so the
    // only honest progress is "how many of them are done" — reported by each
    // thread the moment it finishes one, including one served from a checkpoint.
    let finished = AtomicUsize::new(0);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let threads = cores.min(windows.len());
    // Each window thread may use an equal share of the cores *inside* a window.
    // Without this a clip whose window count is below the core count leaves most
    // of the machine idle — a 60 s clip has two windows, so two cores worked
    // while four sat still. Dividing keeps `threads × budget <= cores`.
    let budget = (cores / threads).max(1);
    // Set by whichever thread hears "keep going?" answered with no. The other
    // threads stop pulling windows; the ones already running finish, because a
    // window is minutes of uninterruptible matmul and its result is still worth
    // checkpointing.
    let cancelled = std::sync::atomic::AtomicBool::new(false);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            // Pull the next window rather than slicing the work up front: window
            // costs vary with how much speech they contain.
            scope.spawn(|| loop {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= windows.len() {
                    break;
                }
                // Thread-local and never unset: this thread exists only to run
                // windows, so the budget applies to everything it does.
                tensor::set_thread_budget(budget);
                let w = &windows[i];
                let span = format!("{:.1}-{:.1}s", w.start, w.end);
                let progress_msg = format!("window {}/{} [{span}]", i + 1, windows.len());

                // `break 'window` replaces `continue`: whatever the outcome, the
                // window is over and the UI must hear about it.
                'window: {
                    if let Some(dir) = checkpoint {
                        let name = window_checkpoint(w);
                        if let Some(cached) = checkpoint::read_json::<Vec<Word>>(dir, &name) {
                            println!(
                                "sentence-video: {progress_msg}: reusing {} aligned word(s) from the checkpoint",
                                cached.len()
                            );
                            let _ = slots[i].set(cached);
                            break 'window;
                        }
                    }

                    // Degenerate timings (every word at 0.0, say) would otherwise
                    // make one window out of the whole file and blow up the
                    // quadratic attention buffer.
                    if w.end - w.start > max_span {
                        eprintln!(
                            "sentence-video: {progress_msg}: skipped, word timings span {:.1}s",
                            w.end - w.start
                        );
                        break 'window;
                    }
                    let a = ((w.start * rate) as usize).min(samples.len());
                    let b = ((w.end * rate) as usize).min(samples.len());
                    if b <= a {
                        eprintln!("sentence-video: {progress_msg}: skipped, no audio");
                        break 'window;
                    }
                    let texts: Vec<&str> = words[w.first..=w.last]
                        .iter()
                        .map(|x| x.text.as_str())
                        .collect();
                    match aligner.align(&samples[a..b], &texts) {
                        Ok(mut refined) => {
                            // Times come back relative to the window.
                            for r in &mut refined {
                                r.start += w.start;
                                r.end += w.start;
                            }
                            if let Some(dir) = checkpoint {
                                if let Err(e) =
                                    checkpoint::write_json(dir, &window_checkpoint(w), &refined)
                                {
                                    // Losing the checkpoint costs CPU next run,
                                    // not correctness — never fail a window for it.
                                    eprintln!(
                                        "sentence-video: {progress_msg}: cannot checkpoint: {e}"
                                    );
                                }
                            }
                            println!(
                                "sentence-video: {progress_msg}: aligned {} word(s)",
                                refined.len()
                            );
                            let _ = slots[i].set(refined);
                        }
                        Err(e) => eprintln!(
                            "sentence-video: {progress_msg}: alignment failed: {e} (keeping whisper timings)"
                        ),
                    }
                }

                if let Some(sink) = progress {
                    let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
                    if !sink(BuildStage::Align {
                        done,
                        total: windows.len(),
                    }) {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    // A cancelled build is not a result: without the refined words the session
    // would be written with whisper's timings and look like it succeeded. The
    // windows already checkpointed stay on disk, so resuming is cheap.
    if cancelled.load(Ordering::Relaxed) {
        return Err(CANCELLED.to_string());
    }

    let mut out = words;
    for (i, slot) in slots.iter().enumerate() {
        match slot.get() {
            Some(refined) => {
                merge_window(&mut out, &windows[i], refined);
                stats.aligned += 1;
            }
            None => stats.failed += 1,
        }
    }
    Ok((out, stats))
}

/// The checkpoint file name for one window: `w{first}-{last}.json`, naming the
/// inclusive range of raw whisper words it covers.
fn window_checkpoint(window: &Window) -> String {
    format!("w{}-{}.json", window.first, window.last)
}

/// Copy `refined` back over the words of one window.
///
/// [`Aligner::align`] drops words with no vocabulary characters (digits, dashes),
/// so the two lists are walked together by text instead of matched by position;
/// a word it dropped keeps whisper's timing.
fn merge_window(out: &mut [Word], window: &Window, refined: &[Word]) {
    let mut cursor = window.first;
    for r in refined {
        while cursor <= window.last && out[cursor].text != r.text {
            cursor += 1;
        }
        if cursor > window.last {
            break;
        }
        out[cursor] = r.clone();
        cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.to_string(),
            start,
            end,
            score: 1.0,
        }
    }

    #[test]
    fn frame_duration_is_twenty_milliseconds() {
        assert!((FRAME_SECS - 0.02).abs() < 1e-12);
    }

    #[test]
    fn windows_cover_every_word_once_and_respect_the_cap() {
        // Ten words five seconds apart, i.e. 50 s of speech.
        let words: Vec<Word> = (0..10)
            .map(|i| word(&format!("w{i}"), i as f64 * 5.0, i as f64 * 5.0 + 2.0))
            .collect();

        let windows = plan_windows(&words);

        let mut next_first = 0;
        for w in &windows {
            assert_eq!(w.first, next_first, "windows must be contiguous");
            assert!(w.last >= w.first);
            assert!(w.start >= 0.0);
            assert!(
                words[w.last].end - words[w.first].start <= WINDOW_SECS,
                "window {} spans more than the cap",
                w.first
            );
            next_first = w.last + 1;
        }
        assert_eq!(next_first, words.len(), "every word must be covered once");
    }

    #[test]
    fn a_word_longer_than_the_cap_gets_its_own_window() {
        let words = vec![word("a", 0.0, 100.0), word("b", 101.0, 102.0)];

        let windows = plan_windows(&words);

        assert_eq!(windows.len(), 2);
        assert_eq!((windows[0].first, windows[0].last), (0, 0));
        assert_eq!((windows[1].first, windows[1].last), (1, 1));
    }

    #[test]
    fn merge_keeps_words_the_aligner_dropped() {
        // "4.0" encodes to no vocabulary characters, so `align` omits it; the
        // words either side are matched by text and take the aligned times.
        let mut words = vec![
            word("Kafka", 0.0, 0.4),
            word("4.0", 0.4, 0.9),
            word("today", 0.9, 1.2),
        ];
        let window = Window {
            start: 0.0,
            end: 1.6,
            first: 0,
            last: 2,
        };
        let refined = vec![word("Kafka", 0.05, 0.45), word("today", 1.0, 1.15)];

        merge_window(&mut words, &window, &refined);

        assert_eq!((words[0].start, words[0].end), (0.05, 0.45));
        assert_eq!(
            (words[1].start, words[1].end),
            (0.4, 0.9),
            "a dropped word must keep whisper's timing"
        );
        assert_eq!((words[2].start, words[2].end), (1.0, 1.15));
    }
}
