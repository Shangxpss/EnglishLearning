//! Segmentation: turns parsed subtitle cues / word timings into a final,
//! sample-clamped sentence list.
//!
//! For the subtitle-supplied path (`.srt`/`.vtt`) each subtitle block is
//! already a sentence: we normalise indices, clamp the last cue's `end` to the
//! media duration, fill `words`, and attach the source path. A cue that outran
//! [`MAX_CUE_SECS`] is then cut at the nearest natural boundary, because whisper
//! punctuates a long monologue with commas alone and those minutes-long
//! "sentences" can neither be synthesised in one clip nor read at a glance.
//!
//! The word-level grouper lives here too so a future whisper.cpp path can reuse
//! the same `Cue` shape, and this module owns the boundary machinery shared with
//! the platform exporter ([`crate::platform`]).

use crate::models::{Cue, Word};

/// Sentence-final punctuation.
pub fn is_sentence_end(word: &str) -> bool {
    word.trim_end().ends_with('.') || word.trim_end().ends_with('!') || word.trim_end().ends_with('?')
}

/// A cue longer than this is cut at a natural boundary.
///
/// Speech synthesis renders one cue as one clip and the dubber rejects a clip
/// past 60 s, so a cue of several minutes never dubs; the same cue is also
/// unreadable. A real sentence is well under this, so ordinary cues pass through
/// untouched.
pub const MAX_CUE_SECS: f64 = 20.0;

/// A split never makes a chunk shorter than this while a longer one is
/// available, so a sentence does not break into a two-word orphan.
const MIN_CHUNK_CHARS: usize = 14;

/// Words that may not be left at the end of a line: on its own "the" or "will"
/// reads as a mistake, so the break moves earlier instead.
pub(crate) const DANGLING: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "my", "your", "his", "her", "its", "our",
    "their", "some", "any", "no", "every", "each", "both", "all", "such", "am", "is", "are", "was",
    "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will", "would",
    "shall", "should", "can", "could", "may", "might", "must",
];

/// Coordinating conjunctions: a break lands *before* one, so the new cue opens
/// with it and both halves read as clauses.
const COORDINATORS: &[&str] = &["and", "but", "or", "nor", "so", "yet", "for"];

/// Subordinating conjunctions and relative pronouns — the "pronoun" half of
/// "split at a conjunction or a pronoun".
const SUBORDINATORS: &[&str] = &[
    "because", "although", "though", "while", "when", "whenever", "whereas", "since", "if",
    "unless", "until", "till", "after", "before", "as", "which", "who", "whom", "whose", "where",
    "why", "how", "than", "whether", "once", "that",
];

pub(crate) const PREPOSITIONS: &[&str] = &[
    "to",
    "of",
    "in",
    "on",
    "at",
    "with",
    "by",
    "from",
    "about",
    "into",
    "over",
    "under",
    "between",
    "during",
    "without",
    "within",
    "through",
    "against",
    "among",
    "across",
    "upon",
    "toward",
    "towards",
    "behind",
    "beyond",
    "beneath",
    "beside",
    "despite",
    "except",
    "inside",
    "outside",
    "onto",
    "throughout",
    "underneath",
    "unlike",
    "via",
];

/// One word of a cue with the timing a break can be placed against.
pub(crate) struct Tok {
    text: String,
    start: f64,
    end: f64,
}

/// Finalize a raw cue list (e.g. straight from the subtitle parser) into a
/// ready-to-serve sentence list.
///
/// Normalisation happens first so the split below sees real timings; a cue
/// already within [`MAX_CUE_SECS`] is then passed through **byte-identical**,
/// and only a runaway one is cut at a natural boundary. Indices are rewritten
/// over the final list, since a cut adds cues.
pub fn finalize(mut cues: Vec<Cue>, media_duration: f64, media_path: &str) -> Vec<Cue> {
    for cue in cues.iter_mut() {
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

    let mut out = Vec::with_capacity(cues.len());
    for cue in cues {
        if cue.end - cue.start > MAX_CUE_SECS {
            out.extend(split_by(&[cue], |toks, start, end| {
                toks[end - 1].end - toks[start].start <= MAX_CUE_SECS
            }));
        } else {
            out.push(cue);
        }
    }
    for (i, cue) in out.iter_mut().enumerate() {
        cue.index = i;
    }
    out
}

/// Cut a sentence cue into cues that satisfy `fits` — the nearest natural
/// boundary inside the longest run that still fits (see [`best_gap`]); a cue
/// that already fits is passed through untouched.
///
/// `fits(toks, start, end)` answers "does `toks[start..end]` stay within the
/// budget?", which is what lets one splitter serve both the character budget of
/// the platform exporter and the time budget of [`finalize`].
pub(crate) fn split_by(cues: &[Cue], fits: impl Fn(&[Tok], usize, usize) -> bool) -> Vec<Cue> {
    let mut out = Vec::new();
    for cue in cues {
        let toks = tokens(cue);
        if toks.is_empty() {
            continue;
        }
        let mut start = 0usize;
        loop {
            let end = furthest_fit(&toks, start, |end| fits(&toks, start, end));
            if end >= toks.len() {
                out.push(chunk(cue, &toks, start, toks.len(), out.len()));
                break;
            }
            let split = best_gap(&toks, start, end);
            out.push(chunk(cue, &toks, start, split, out.len()));
            start = split;
        }
    }
    out
}

/// The words of `cue`, timed. Word timings are used when they are real;
/// otherwise (a cue parsed from a hand-written subtitle carries zeros) the
/// words are spread across the cue in proportion to their length.
pub(crate) fn tokens(cue: &Cue) -> Vec<Tok> {
    let texts: Vec<String> = if cue.words.is_empty() {
        cue.text.split_whitespace().map(|w| w.to_string()).collect()
    } else {
        cue.words.iter().map(|w| w.text.clone()).collect()
    };
    if texts.is_empty() {
        return Vec::new();
    }
    let timed = cue.words.len() == texts.len() && cue.words.iter().any(|w| w.end > w.start);
    if timed {
        cue.words
            .iter()
            .map(|w| Tok {
                text: w.text.clone(),
                start: w.start,
                end: w.end,
            })
            .collect()
    } else {
        spread(&texts, cue.start, cue.end)
    }
}

/// Lay `texts` end to end across `[start, end]`, each word getting a share of
/// the time proportional to its length.
pub(crate) fn spread(texts: &[String], start: f64, end: f64) -> Vec<Tok> {
    let total: usize = texts.iter().map(|t| t.chars().count().max(1)).sum();
    let span = (end - start).max(0.0);
    let mut done = 0usize;
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        let at = |chars: usize| start + span * (chars as f64 / total as f64);
        let begin = at(done);
        done += text.chars().count().max(1);
        out.push(Tok {
            text: text.clone(),
            start: begin,
            end: at(done),
        });
    }
    out
}

/// `spread`'s tokens as the words a cue carries.
pub(crate) fn into_words(toks: Vec<Tok>) -> Vec<Word> {
    toks.into_iter()
        .map(|t| Word {
            text: t.text,
            start: t.start,
            end: t.end,
            score: 1.0,
        })
        .collect()
}

/// The last index (exclusive) whose run still fits, per `fits(end)`. Always
/// advances at least one word, so a word larger than the whole budget is a cue
/// of its own rather than an infinite loop.
pub(crate) fn furthest_fit(toks: &[Tok], start: usize, fits: impl Fn(usize) -> bool) -> usize {
    let mut end = start + 1;
    while end < toks.len() && fits(end + 1) {
        end += 1;
    }
    end
}

/// Where to cut a cue that must break at or before `end`.
///
/// The strongest boundary inside the window wins — clause punctuation, then a
/// comma, then a conjunction, then a subordinate clause or relative pronoun,
/// then a preposition — and ties go to the latest one, so the line is used
/// rather than wasted. A boundary that would leave a phrase hanging is never
/// taken; with no boundary at all the cue breaks at `end`, the furthest word
/// that still fits.
fn best_gap(toks: &[Tok], start: usize, end: usize) -> usize {
    let mut natural: Option<(u8, usize)> = None;
    let mut fallback: Option<usize> = None;
    for k in (start + 1)..=end {
        if boundary(toks, k).is_none() {
            continue;
        }
        if chars(&toks[start..k]) < MIN_CHUNK_CHARS {
            continue;
        }
        fallback = Some(k);
        if chars(&toks[k..]) < MIN_CHUNK_CHARS {
            continue;
        }
        let score = score(toks, k);
        if score == 0 {
            continue;
        }
        if natural.map_or(true, |(best, _)| score >= best) {
            natural = Some((score, k));
        }
    }
    natural.map(|(_, k)| k).or(fallback).unwrap_or(end)
}

/// Whether a line may end between `toks[k - 1]` and `toks[k]`.
pub(crate) fn boundary(toks: &[Tok], k: usize) -> Option<()> {
    let next = stem(&toks[k].text);
    if toks[k].text.starts_with('\'') || next == "not" || next == "n't" {
        return None;
    }
    let prev = stem(&toks[k - 1].text);
    if DANGLING.contains(&prev.as_str()) || PREPOSITIONS.contains(&prev.as_str()) {
        return None;
    }
    Some(())
}

/// How natural a break after `toks[k - 1]` is; `0` is a plain word boundary.
pub(crate) fn score(toks: &[Tok], k: usize) -> u8 {
    let prev = toks[k - 1].text.trim_end();
    if [':', ';', '—', '–'].iter().any(|c| prev.ends_with(*c)) {
        return 5;
    }
    if prev.ends_with(',') {
        return 4;
    }
    let next = stem(&toks[k].text);
    if COORDINATORS.contains(&next.as_str()) {
        return 3;
    }
    if SUBORDINATORS.contains(&next.as_str()) {
        return 2;
    }
    if PREPOSITIONS.contains(&next.as_str()) {
        return 1;
    }
    0
}

/// A word stripped of surrounding punctuation and lower-cased, so `"And"` and
/// `"and"` are the same conjunction.
pub(crate) fn stem(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

pub(crate) fn chars(toks: &[Tok]) -> usize {
    toks.iter().map(|t| t.text.chars().count()).sum::<usize>() + toks.len().saturating_sub(1)
}

/// Build the cue for `toks[a..b]`. Its end is the next chunk's start, so the
/// pieces of one sentence join seamlessly; the last piece ends where the
/// sentence did.
fn chunk(cue: &Cue, toks: &[Tok], a: usize, b: usize, index: usize) -> Cue {
    let text = toks[a..b]
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let start = toks[a].start;
    let end = match toks.get(b) {
        Some(next) => next.start.max(start),
        None => cue.end.max(start),
    };
    Cue {
        index,
        start,
        end,
        text,
        words: toks[a..b]
            .iter()
            .map(|t| Word {
                text: t.text.clone(),
                start: t.start,
                end: t.end,
                score: 1.0,
            })
            .collect(),
        source_path: cue.source_path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(text: &str, start: f64, end: f64) -> Cue {
        let words: Vec<Word> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| Word {
                text: w.to_string(),
                start: start + i as f64 * 0.5,
                end: start + i as f64 * 0.5 + 0.4,
                score: 1.0,
            })
            .collect();
        Cue {
            index: 0,
            start,
            end,
            text: text.to_string(),
            words,
            source_path: None,
        }
    }

    /// A cue parsed from a subtitle carries no word timings, so its words are
    /// spread across the span.
    fn untimed(text: &str, start: f64, end: f64) -> Cue {
        Cue {
            index: 0,
            start,
            end,
            text: text.to_string(),
            words: Vec::new(),
            source_path: None,
        }
    }

    fn reassembled(cues: &[Cue]) -> String {
        cues.iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_cue_within_the_cap_is_untouched() {
        let original = cue("A short sentence, kept whole.", 1.0, 3.0);
        let out = finalize(vec![original.clone()], 100.0, "/media/lecture.mp4");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start, original.start);
        assert_eq!(out[0].end, original.end);
        assert_eq!(out[0].text, original.text);
        assert_eq!(out[0].source_path.as_deref(), Some("/media/lecture.mp4"));
    }

    #[test]
    fn a_cue_past_the_cap_is_cut_and_keeps_every_word() {
        // 30 words spread over 48 s, punctuated only by a comma in the middle.
        let text = (0..30)
            .map(|i| {
                if i == 15 {
                    "however,".to_string()
                } else {
                    format!("word{i}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let out = finalize(vec![untimed(&text, 0.0, 48.0)], 100.0, "/media/lecture.mp4");

        assert!(out.len() > 1, "a 48 s cue must be cut, got {out:?}");
        assert_eq!(reassembled(&out), text, "no word may be lost");
        for c in &out {
            assert!(c.end - c.start <= MAX_CUE_SECS + 1e-9, "{c:?}");
            assert_eq!(c.source_path.as_deref(), Some("/media/lecture.mp4"));
        }
        // Indices are rewritten over the pieces.
        let indices: Vec<usize> = out.iter().map(|c| c.index).collect();
        assert_eq!(indices, (0..out.len()).collect::<Vec<_>>());
        // Pieces join seamlessly: each ends where the next begins.
        for pair in out.windows(2) {
            assert!((pair[1].start - pair[0].end).abs() < 1e-9, "{pair:?}");
        }
    }

    #[test]
    fn the_cut_prefers_a_natural_boundary() {
        // The comma sits before the last word that fits; the cut lands there.
        let words: Vec<String> = (0..30).map(|i| format!("w{i}")).collect();
        let mut text = words.clone();
        text[19] = "w19,".to_string();
        let text = text.join(" ");
        let out = finalize(vec![untimed(&text, 0.0, 30.0)], 100.0, "/media/lecture.mp4");
        assert!(out.len() > 1);
        assert_eq!(out[0].text.split_whitespace().last(), Some("w19,"));
    }
}