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

use crate::media;
use crate::models::{Cue, Word};
use crate::segmenter;
use crate::subtitle;

pub use crate::segmenter::is_sentence_end;

/// Grouping limits mirrored from Python's `cue_builder` so sentences never
/// grow unbounded in duration or word count.
const MAX_CUE_SECS: f64 = 12.0;
const MAX_CUE_WORDS: usize = 40;

/// Build a sentence-sized `Cue` from a run of words.
fn make_cue(index: usize, words: &[Word]) -> Cue {
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let start = words
        .first()
        .map(|w| w.start)
        .unwrap_or(0.0);
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
/// `cue_builder.build_aligned_cues`. Words accumulate until a sentence-final
/// punctuation mark, too many words, or too long a span — whichever comes
/// first. Output is sample-clamped via [`segmenter::finalize`].
pub fn build_cues(words: &[Word], media_duration: f64) -> Vec<Cue> {
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

        let start = current
            .first()
            .map(|w| w.start)
            .unwrap_or(0.0);
        let end = word.end;
        let too_long = (end - start) > MAX_CUE_SECS;
        let too_many = current.len() >= MAX_CUE_WORDS;
        let finally_sentence = open_parens <= 0 && is_sentence_end(&word.text);

        if too_long || too_many || finally_sentence {
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

    segmenter::finalize(cues, media_duration, "")
}

/// A speech-recognition transcript: word-level timings plus the audio
/// duration they were aligned against.
pub struct Transcript {
    pub words: Vec<Word>,
    pub duration_secs: f64,
}

/// From-audio entry point: transcribe the media's audio into word timings.
///
/// This is the natural home for a whisper.cpp binding. Until a binding is
/// compiled in, it returns a descriptive error so the pipeline can fall back
/// to an explicit/adjacent subtitle file.
pub fn transcribe_words(media_path: &str) -> Result<Transcript, String> {
    // TODO: bind whisper.cpp here; probe the duration with
    // `crate::media::probe_duration(media_path)` to align word timings.
    let _ = media_path;
    Err("speech-to-text backend is not bound yet. Bind a whisper.cpp backend in \
         `pipeline::transcribe::transcribe_words`, or supply a subtitle file via \
         cues_from_media(media, Some(subtitle))."
        .to_string())
}

/// Look for an adjacent `<media-basename>.(srt|vtt)` subtitle file.
pub fn auto_subtitle(media_path: &str) -> Option<String> {
    let p = Path::new(media_path);
    let stem = p.with_extension("srt");
    if stem.exists() {
        return Some(stem.to_string_lossy().into_owned());
    }
    let stem = p.with_extension("vtt");
    if stem.exists() {
        return Some(stem.to_string_lossy().into_owned());
    }
    None
}

/// Best-effort cue source for a media file:
///
/// 1. an explicitly supplied subtitle path (parsed + finalized), else
/// 2. an adjacent `<media>.srt`/`.vtt` auto-detected next to the media.
///
/// If neither exists, a clear error referencing the [`transcribe_words`] hook
/// is returned.
pub fn cues_from_media(
    media_path: &str,
    explicit_subtitle: Option<&str>,
) -> Result<Vec<Cue>, String> {
    let subtitle_path = explicit_subtitle
        .map(str::to_string)
        .or_else(|| auto_subtitle(media_path));

    match subtitle_path {
        Some(path) => {
            let mut cues = subtitle::parse_subtitle(&path)?;
            // Build full word lists for cues parsed from file (parser leaves
            // them time-less), then clamp timings to the media length.
            let media_path = String::from(media_path);
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
            let duration = media::probe_duration(&media_path).unwrap_or(0.0);
            Ok(segmenter::finalize(cues, duration, &media_path))
        }
        None => Err(format!(
            "no subtitle found for '{media_path}' (looked for <basename>.srt/.vtt). \
             Pass one with `--subtitle`, or bind a whisper.cpp backend in \
             `pipeline::transcribe::transcribe_words` to derive cues from the audio."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_words_into_sentences_at_punctuation() {
        let words = [
            Word { text: "Hello".into(), start: 0.0, end: 0.5, score: 1.0 },
            Word { text: "world.".into(), start: 0.5, end: 1.0, score: 1.0 },
            Word { text: "Next".into(), start: 2.0, end: 2.5, score: 1.0 },
            Word { text: "sentence!".into(), start: 2.5, end: 3.0, score: 1.0 },
        ];
        let cues = build_cues(&words, 10.0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "Hello world.");
        assert_eq!(cues[1].text, "Next sentence!");
    }

    #[test]
    fn keeps_parens_containing_period_inside_one_cue() {
        let words = [
            Word { text: "See".into(), start: 0.0, end: 0.3, score: 1.0 },
            Word { text: "(e.g.".into(), start: 0.3, end: 0.6, score: 1.0 },
            Word { text: "one).".into(), start: 0.6, end: 1.0, score: 1.0 },
            Word { text: "Next.".into(), start: 1.5, end: 2.0, score: 1.0 },
        ];
        let cues = build_cues(&words, 10.0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "See (e.g. one).");
    }
}