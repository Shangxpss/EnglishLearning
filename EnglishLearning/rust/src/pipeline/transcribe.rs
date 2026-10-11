//! **Step 1 of the replace-audio pipeline — obtain sentence cues from the
//! source media.**
//!
//! Every function here is self-contained and reusable:
//!
//! * [`parse_subtitle`] — parse an existing `.srt`/`.vtt` into cues.
//! * [`build_cues`] — group word-level timings (`Vec<Word>`) into sentence
//!   cues. This is the pure-Rust equivalent of Python's `cue_builder.py` and
//!   is the grouper used by any future speech-recognition backend.
//! * [`transcribe_words`] — the from-audio entry point: audio → words. This
//!   is where a whisper.cpp backend plugs in; until it is bound it returns a
//!   descriptive error so callers can fall back to a subtitle file.
//! * [`cues_from_media`] — convenience orchestrator that picks the best cue
//!   source for a media file (explicit subtitle → adjacent subtitle).

use std::path::Path;

use crate::logs;
use crate::media;
use crate::models::{Cue, Word};
use crate::pipeline::checkpoint;
use crate::segmenter;
use crate::subtitle;

pub use crate::segmenter::is_sentence_end;

/// Build a sentence-sized `Cue` from a run of words.
fn make_cue(index: usize, words: &[Word]) -> Cue {
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let start = words.first().map(|w| w.start).unwrap_or(0.0);
    let end = words.iter().map(|w| w.end).fold(start, f64::max);
    Cue {
        index,
        start,
        end,
        text,
        words: words.to_vec(),
        source_path: None,
    }
}

/// Group word-level timings into sentence cues, ported from Python's
/// `cue_builder.build_aligned_cues`.
///
/// Words accumulate until a **sentence-final punctuation mark**, so a cue is
/// normally a complete sentence. Whisper punctuates a long monologue with commas
/// alone, though, so a "sentence" can run for minutes; [`segmenter::finalize`]
/// then cuts anything past [`segmenter::MAX_CUE_SECS`] at the nearest natural
/// boundary, because such a cue can neither be synthesised as one clip nor read
/// at a glance. There is deliberately no word-count cap and no aggressive
/// duration cap: a normal sentence is never split, and the older 12 s / 40-word
/// cap that sliced sentences mid-thought (ending a cue on "...powering today's"
/// instead of at the next full stop) stays gone.
/// Any trailing words at the end of the audio form a final cue.
///
/// Output is sample-clamped via [`segmenter::finalize`].
pub fn build_cues(words: &[Word], media_duration: f64, media_path: &str) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut open_parens = 0i32;

    for word in words {
        // Track bracket depth so periods inside parentheses don't split.
        for ch in word.text.chars() {
            match ch {
                '(' | '[' | '{' => open_parens += 1,
                ')' | ']' | '}' if open_parens > 0 => open_parens -= 1,
                _ => {}
            }
        }
        current.push(word.clone());

        if open_parens <= 0 && is_sentence_end(&word.text) {
            cues.push(make_cue(cues.len(), &current));
            current.clear();
            open_parens = 0;
        }
    }
    if !current.is_empty() {
        cues.push(make_cue(cues.len(), &current));
    }

    // Merge an overly-short trailing cue into the previous one, mirroring the
    // Python post-processing that avoids one-word fragments.
    if cues.len() >= 2 {
        let last = cues.last().unwrap();
        let short_tail = (last.end - last.start) < 0.5 && last.words.len() <= 2;
        if short_tail {
            let tail = cues.pop().unwrap();
            if let Some(prev) = cues.last_mut() {
                prev.end = tail.end;
                prev.text = format!("{} {}", prev.text, tail.text);
                prev.words.extend(tail.words);
            } else {
                cues.push(tail);
            }
        }
    }

    segmenter::finalize(cues, media_duration, media_path)
}

/// A speech-recognition transcript: word-level timings plus the audio
/// duration they were aligned against.
pub struct Transcript {
    pub words: Vec<Word>,
    pub duration_secs: f64,
}

/// Which stage of a session build is running right now.
///
/// Building a session is **synchronous** — the session row only reaches the
/// database when it ends — so without this a caller cannot tell the slow part
/// (whisper, then wav2vec2 alignment, which is minutes per video) from a hang:
/// the video is not in the session list yet and no endpoint knows it is being
/// worked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStage {
    /// Decoding the audio to 16 kHz mono for the model.
    Decode,
    /// Whisper transcribes the whole buffer in one call, so there is no
    /// sub-progress to report — only that this is the current stage.
    Transcribe,
    /// Wav2vec2 forced alignment, window by window. Alignment dominates the
    /// cost of a build, so this is the one stage where a bar is worth drawing.
    Align { done: usize, total: usize },
}

/// The error a build unwinds with when the user cancelled it. Matched by the
/// HTTP layer, which reports it as "cancelled" instead of as a failed
/// transcription — the user asked for it, so it is not a failure.
pub const CANCELLED: &str = "cancelled by the user";

/// Receives [`BuildStage`] updates during a build and answers "keep going?".
///
/// The return value is how cancellation reaches the pipeline. A build is minutes
/// of CPU inside one request, so there is no other moment to ask: whoever owns
/// the sink also owns the button, and every stage boundary is already a place
/// where the build reports — folding the check into the same call means no
/// second parameter has to be threaded down to the aligner.
///
/// `Sync` because alignment spreads its windows over several threads and every
/// window reports its own completion.
pub type BuildSink<'a> = Option<&'a (dyn Fn(BuildStage) -> bool + Sync)>;

/// Report a stage when a sink is attached, failing the build if it says stop.
///
/// Checked at stage boundaries only, which is what can be checked: whisper
/// transcribes the whole buffer in one call and cannot be interrupted, so a
/// cancel during transcription takes effect when it returns.
fn report(sink: BuildSink<'_>, stage: BuildStage) -> Result<(), String> {
    match sink {
        Some(sink) if !sink(stage) => Err(CANCELLED.to_string()),
        _ => Ok(()),
    }
}

/// From-audio entry point: transcribe the media's audio into word timings.
///
/// Decodes the audio to 16 kHz mono with the in-process FFmpeg layer, then runs
/// whisper.cpp (see [`crate::asr`]). The model is embedded in the executable, so
/// no external files are needed; `--asr-model <path>` overrides it.
pub fn transcribe_words(media_path: &str, align: bool) -> Result<Transcript, String> {
    let cfg = crate::asr::config();
    transcribe_words_with(
        media_path,
        cfg.model.as_deref(),
        cfg.language.as_deref(),
        align,
        None,
        None,
    )
}

/// [`transcribe_words`] with an explicit model, language, alignment choice, and
/// an optional checkpoint directory.
///
/// With `checkpoint` set, `words.json` inside it holds the raw whisper word
/// timings and `align/w{first}-{last}.json` holds each refined alignment window
/// (see [`crate::align::refine_words`]). A re-run therefore skips whisper when
/// the raw words are already there, and skips every alignment window already
/// computed — which is the expensive part. **Whisper itself is not resumable**:
/// it transcribes the whole buffer in one call, so the raw-words file buys the
/// single boundary "ASR finished, alignment/dubbing interrupted".
pub fn transcribe_words_with(
    media_path: &str,
    model: Option<&Path>,
    language: Option<&str>,
    align: bool,
    checkpoint: Option<&Path>,
    progress: BuildSink<'_>,
) -> Result<Transcript, String> {
    let duration_secs = media::probe_duration(media_path).unwrap_or(0.0);

    let cached: Option<Vec<Word>> =
        checkpoint.and_then(|dir| checkpoint::read_json::<Vec<Word>>(dir, "words.json"));

    let (samples, words) = match cached.filter(|w| !w.is_empty()) {
        Some(words) => {
            println!(
                "sentence-video: reusing {} transcribed word(s) from the checkpoint",
                words.len()
            );
            logs::debug("asr", "reusing transcribed words from the checkpoint")
                .media(media_path)
                .detail(format!("{} word(s)", words.len()))
                .send();
            // Only the aligner needs the audio, and decoding it is not free.
            let samples = if align {
                report(progress, BuildStage::Decode)?;
                logs::debug("asr", "decoding audio for alignment")
                    .media(media_path)
                    .send();
                media::decode_to_f32(media_path, crate::asr::SAMPLE_RATE, true, None)?.0
            } else {
                Vec::new()
            };
            (samples, words)
        }
        None => {
            if !crate::asr::model_available(model) {
                logs::error("asr", "speech-to-text model unavailable")
                    .media(media_path)
                    .send();
                return Err(crate::asr::NO_MODEL_HELP.to_string());
            }
            report(progress, BuildStage::Decode)?;
            logs::debug("asr", "decoding audio")
                .media(media_path)
                .send();
            let (samples, _channels) =
                media::decode_to_f32(media_path, crate::asr::SAMPLE_RATE, true, None)?;
            if samples.is_empty() {
                logs::error("asr", "no audio track could be decoded")
                    .media(media_path)
                    .send();
                return Err(format!(
                    "no audio track could be decoded from '{media_path}'"
                ));
            }
            report(progress, BuildStage::Transcribe)?;
            logs::info("asr", "transcribing audio")
                .media(media_path)
                .detail(format!(
                    "model={} · language={} · {:.1}s of audio",
                    model
                        .map(|m| m.display().to_string())
                        .unwrap_or_else(|| "<embedded>".into()),
                    language.unwrap_or("en"),
                    duration_secs
                ))
                .send();
            let started = std::time::Instant::now();
            let words = crate::asr::transcribe(&samples, model, language)?;
            logs::info("asr", "transcription finished")
                .media(media_path)
                .detail(format!("{} word(s)", words.len()))
                .duration_ms(started.elapsed().as_millis() as i64)
                .send();
            if let Some(dir) = checkpoint {
                // Written straight after ASR so an interruption during
                // alignment does not cost the transcription again.
                if let Err(e) = checkpoint::write_json(dir, "words.json", &words) {
                    eprintln!("sentence-video: cannot checkpoint the transcript: {e}");
                }
            }
            (samples, words)
        }
    };

    let align_dir = checkpoint.map(|dir| dir.join("align"));
    let words = refine_timings(&samples, words, align, align_dir.as_deref(), progress)?;
    Ok(Transcript {
        words,
        duration_secs,
    })
}

/// Replace whisper's DTW word timings with wav2vec2 forced alignment.
///
/// Every failure here is soft: the alignment is an optional refinement of timings
/// whisper already produced, so a missing feature, missing weights, or a window
/// that will not align all fall back to whisper's timings rather than failing the
/// session. The samples were already decoded for whisper, so this adds no I/O.
#[cfg(feature = "asr-align")]
fn refine_timings(
    samples: &[f32],
    words: Vec<Word>,
    align: bool,
    checkpoint: Option<&Path>,
    progress: BuildSink<'_>,
) -> Result<Vec<Word>, String> {
    use crate::align;
    if !align {
        return Ok(words);
    }
    let Some(path) = align::default_weights_path() else {
        eprintln!("sentence-video: {}", align::NO_WEIGHTS_HELP);
        logs::warn(
            "align",
            "alignment weights missing; keeping whisper timings",
        )
        .send();
        return Ok(words);
    };
    let windows = align::plan_windows(&words).len();
    println!(
        "sentence-video: refining word timings with {} ({} words, {} windows)",
        path.display(),
        words.len(),
        windows
    );
    logs::info("align", "refining word timings with wav2vec2")
        .detail(format!(
            "weights={} · {} word(s) · {} window(s)",
            path.display(),
            words.len(),
            windows
        ))
        .send();
    let started = std::time::Instant::now();
    let aligner = match align::Aligner::load(&path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sentence-video: cannot load alignment weights: {e}");
            logs::error(
                "align",
                "cannot load alignment weights; keeping whisper timings",
            )
            .detail(&e)
            .send();
            return Ok(words);
        }
    };
    // The only stage that can be interrupted: it runs window by window, and every
    // finished window is its own checkpoint — so cancelling here keeps all the
    // work done so far for the next attempt.
    let (refined, stats) = align::refine_words(&aligner, samples, words, checkpoint, progress)?;
    println!(
        "sentence-video: alignment finished: {} of {} window(s) aligned, {} kept whisper timings",
        stats.aligned, stats.windows, stats.failed
    );
    logs::info("align", "alignment finished")
        .detail(format!(
            "{} of {} window(s) aligned · {} kept whisper timings",
            stats.aligned, stats.windows, stats.failed
        ))
        .duration_ms(started.elapsed().as_millis() as i64)
        .send();
    Ok(refined)
}

/// Without the feature compiled in, `align` can only be reported, not honoured.
#[cfg(not(feature = "asr-align"))]
fn refine_timings(
    _samples: &[f32],
    words: Vec<Word>,
    align: bool,
    _checkpoint: Option<&Path>,
    _progress: BuildSink<'_>,
) -> Result<Vec<Word>, String> {
    if align {
        eprintln!(
            "sentence-video: this build has no wav2vec2 aligner; rebuild with \
             `cargo build --features asr-align` (keeping whisper timings)"
        );
    }
    Ok(words)
}

/// Path of the canonical sentence-level subtitle for a media file:
/// `<dir>/<stem>_full_sentence.srt` (e.g. `lecture.mp4` →
/// `lecture_full_sentence.srt`). This is where a transcription is saved so the
/// next run — and the user — can reuse or edit it.
pub fn sentence_subtitle_path(media_path: &str) -> String {
    let p = Path::new(media_path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = p.parent().unwrap_or_else(|| Path::new(""));
    dir.join(format!("{stem}_full_sentence.srt"))
        .to_string_lossy()
        .into_owned()
}

/// Locate an existing subtitle **without transcribing anything**:
///
/// 1. the explicitly supplied path, else
/// 2. `<stem>_full_sentence.srt` next to the media.
///
/// A legacy adjacent `<stem>.srt` / `<stem>.vtt` is deliberately **not** used:
/// the canonical file is always the source of truth, so a missing one triggers
/// transcription rather than silently adopting an unrelated subtitle.
///
/// `None` means "nothing on disk — transcription is required".
pub fn locate_subtitle(media_path: &str, explicit: Option<&str>) -> Option<String> {
    if let Some(path) = explicit {
        return Some(path.to_string());
    }
    let canonical = sentence_subtitle_path(media_path);
    if Path::new(&canonical).is_file() {
        return Some(canonical);
    }
    None
}

/// Parse a subtitle file into finalized sentence cues.
pub fn cues_from_subtitle(path: &str, media_path: &str) -> Result<Vec<Cue>, String> {
    let mut cues = subtitle::parse_subtitle(path)?;
    // The parser leaves word timings empty; give each cue a word list so the
    // shape matches transcription-derived cues.
    for cue in cues.iter_mut() {
        if cue.words.is_empty() || cue.words[0].end == 0.0 {
            cue.words = cue
                .text
                .split_whitespace()
                .map(|w| Word {
                    text: w.to_string(),
                    start: 0.0,
                    end: 0.0,
                    score: 1.0,
                })
                .collect();
        }
    }
    let duration = media::probe_duration(media_path).unwrap_or(0.0);
    Ok(segmenter::finalize(cues, duration, media_path))
}

/// Transcribe the media's audio into sentence cues and **save them** as
/// [`sentence_subtitle_path`], so later runs reuse the file instead of
/// re-running the model.
///
/// `checkpoint` is passed through to [`transcribe_words_with`] so the raw words
/// and alignment windows survive an interruption.
///
/// Returns the cues and the path written (`None` if saving failed — the
/// transcription itself still succeeds).
pub fn transcribe_and_save(
    media_path: &str,
    align: bool,
    checkpoint: Option<&Path>,
    progress: BuildSink<'_>,
) -> Result<(Vec<Cue>, Option<String>), String> {
    let cfg = crate::asr::config();
    let transcript = transcribe_words_with(
        media_path,
        cfg.model.as_deref(),
        cfg.language.as_deref(),
        align,
        checkpoint,
        progress,
    )?;
    // With alignment off, transcription is the last thing that reports, and the
    // session is written as soon as this returns — so without a check here a
    // cancel that arrived while whisper ran would still produce a session. (With
    // alignment on, the aligner's own checks cover the same window.)
    report(progress, BuildStage::Transcribe)?;
    if transcript.words.is_empty() {
        return Err(format!("speech-to-text found no words in '{media_path}'"));
    }
    let cues = build_cues(&transcript.words, transcript.duration_secs, media_path);
    let out = sentence_subtitle_path(media_path);
    match subtitle::write_srt(&cues, &out) {
        Ok(()) => {
            logs::info("asr", "transcript saved")
                .media(media_path)
                .detail(format!("{} cue(s) → {out}", cues.len()))
                .send();
            Ok((cues, Some(out)))
        }
        Err(e) => {
            // A read-only folder must not fail the session.
            eprintln!("sentence-video: could not save the transcript: {e}");
            logs::warn("asr", "could not save the transcript")
                .media(media_path)
                .detail(&e)
                .send();
            Ok((cues, None))
        }
    }
}

/// Best-effort cue source for a media file (used by the `dub` pipeline):
/// an existing subtitle via [`locate_subtitle`], else transcription saved to
/// `<stem>_full_sentence.srt`.
pub fn cues_from_media(
    media_path: &str,
    explicit_subtitle: Option<&str>,
    align: bool,
) -> Result<Vec<Cue>, String> {
    match locate_subtitle(media_path, explicit_subtitle) {
        Some(path) => cues_from_subtitle(&path, media_path),
        None => transcribe_and_save(media_path, align, None, None).map(|(cues, _)| cues),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_words_into_sentences_at_punctuation() {
        let words = [
            Word {
                text: "Hello".into(),
                start: 0.0,
                end: 0.5,
                score: 1.0,
            },
            Word {
                text: "world.".into(),
                start: 0.5,
                end: 1.0,
                score: 1.0,
            },
            Word {
                text: "Next".into(),
                start: 2.0,
                end: 2.5,
                score: 1.0,
            },
            Word {
                text: "sentence!".into(),
                start: 2.5,
                end: 3.0,
                score: 1.0,
            },
        ];
        let cues = build_cues(&words, 10.0, "/media/sample.mp4");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hello world.");
        assert_eq!(cues[1].text, "Next sentence!");
    }

    #[test]
    fn keeps_parens_containing_period_inside_one_cue() {
        let words = [
            Word {
                text: "See".into(),
                start: 0.0,
                end: 0.3,
                score: 1.0,
            },
            Word {
                text: "(e.g.".into(),
                start: 0.3,
                end: 0.6,
                score: 1.0,
            },
            Word {
                text: "one).".into(),
                start: 0.6,
                end: 1.0,
                score: 1.0,
            },
            Word {
                text: "Next.".into(),
                start: 1.5,
                end: 2.0,
                score: 1.0,
            },
        ];
        let cues = build_cues(&words, 10.0, "/media/sample.mp4");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "See (e.g. one).");
    }

    /// Regression: a normal sentence must stay **one** cue. This 12 s / 40-word
    /// run is well under [`segmenter::MAX_CUE_SECS`], whereas the former 12 s /
    /// 40-word cap sliced exactly this kind of sentence mid-thought.
    #[test]
    fn a_normal_long_sentence_is_not_split() {
        let mut words: Vec<Word> = (0..40)
            .map(|i| Word {
                text: format!("word{i}"),
                start: i as f64 * 0.3,
                end: i as f64 * 0.3 + 0.25,
                score: 1.0,
            })
            .collect();
        words.last_mut().unwrap().text = "end.".to_string();

        let span = words.last().unwrap().end - words[0].start;
        assert!(
            (11.0..segmenter::MAX_CUE_SECS).contains(&span),
            "test needs a ~12s run, got {span}s"
        );
        assert!(words.len() >= 40, "test needs a 40-word run");

        let cues = build_cues(&words, 120.0, "/media/sample.mp4");
        assert_eq!(cues.len(), 1, "a sentence under the cap must not be split");
        assert!(cues[0].text.ends_with("end."));
    }

    /// A runaway "sentence" — whisper punctuated a monologue with a single comma
    /// — is cut at natural boundaries so every cue is short enough to synthesise
    /// and read. No word is dropped and the pieces join seamlessly.
    #[test]
    fn a_runaway_sentence_is_split_at_a_natural_boundary() {
        // 100 words at 0.5 s apart = ~50 s, punctuated only by one comma.
        let mut words: Vec<Word> = (0..100)
            .map(|i| Word {
                text: format!("word{i}"),
                start: i as f64 * 0.5,
                end: i as f64 * 0.5 + 0.4,
                score: 1.0,
            })
            .collect();
        words[50].text = "however,".to_string();
        words.last_mut().unwrap().text = "end.".to_string();

        let span = words.last().unwrap().end - words[0].start;
        assert!(
            span > segmenter::MAX_CUE_SECS,
            "test needs a >20s run, got {span}s"
        );

        let cues = build_cues(&words, 120.0, "/media/sample.mp4");
        assert!(cues.len() > 1, "a runaway sentence must be split");

        let original = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let joined = cues
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, original, "no word may be lost");

        for c in &cues {
            assert!(c.end - c.start <= segmenter::MAX_CUE_SECS + 1e-9, "{c:?}");
        }
        for pair in cues.windows(2) {
            assert!((pair[1].start - pair[0].end).abs() < 1e-9, "{pair:?}");
        }
    }
}
