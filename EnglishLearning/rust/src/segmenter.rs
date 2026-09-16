//! Segmentation: turns parsed subtitle cues / word timings into a final,
//! sample-clamped sentence list.
//!
//! For the subtitle-supplied path (`.srt`/`.vtt`) each subtitle block is
//! already a sentence: we normalise indices, clamp the last cue's `end` to the
//! media duration, fill `words`, and attach the source path. The word-level
//! grouper lives here too so a future whisper.cpp path can reuse the same
//! `Cue` shape.

use crate::models::{Cue, Word};

/// Sentence-final punctuation.
pub fn is_sentence_end(word: &str) -> bool {
    word.trim_end().ends_with('.') || word.trim_end().ends_with('!') || word.trim_end().ends_with('?')
}

/// Finalize a raw cue list (e.g. straight from the subtitle parser) into a
/// ready-to-serve sentence list.
pub fn finalize(mut cues: Vec<Cue>, media_duration: f64, media_path: &str) -> Vec<Cue> {
    for (i, cue) in cues.iter_mut().enumerate() {
        cue.index = i;
        cue.source_path = Some(media_path.to_string());
        if cue.end > media_duration && media_duration > 0.0 {
            cue.end = media_duration;
        }
        if cue.start < 0.0 {
            cue.start = 0.0;
        }
        if cue.words.is_empty() {
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
    cues
}