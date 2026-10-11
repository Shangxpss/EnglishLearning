//! Subtitles for a video platform.
//!
//! The subtitle the app segments with (`<stem>_full_sentence.srt`) keeps whole
//! sentences, which is what a learner needs. A video platform is a different
//! audience: a cue has to fit a line-length limit and be readable in one
//! glance. This module turns sentence cues into those short cues — cutting a
//! long sentence at the nearest natural boundary instead of mid-phrase — and
//! writes them as `<video stem>.srt` beside the source video, deliberately
//! separate from the `_full_sentence` file.
//!
//! The boundary machinery lives in [`crate::segmenter`], which also uses it to
//! cap a sentence cue's length; this module supplies the character budget.
//!
//! Timings come from the voice the export is meant to accompany. A dubbed
//! track replaces every sentence with a freshly synthesised one, so the
//! original word timings no longer describe it; [`retime_to_dubbed`] measures
//! the sentences against the dubbed audio instead.

use crate::models::{Cue, Word};
use crate::segmenter::{self, into_words, spread};

/// Per-cue character budget: YouTube's single-line limit.
pub const DEFAULT_MAX_CHARS: usize = 42;

/// Where the platform subtitle for `media_path` goes: the video's own name in
/// the video's own folder, e.g. `/videos/lecture.mp4` → `/videos/lecture.srt`.
pub fn platform_subtitle_path(media_path: &str) -> String {
    std::path::Path::new(media_path)
        .with_extension("srt")
        .to_string_lossy()
        .into_owned()
}

/// Split `cues` into cues of at most `max_chars` characters and write them as
/// an `.srt` beside the media, returning the path written.
pub fn export(cues: &[Cue], media_path: &str, max_chars: usize) -> Result<String, String> {
    let path = platform_subtitle_path(media_path);
    crate::subtitle::write_srt(&split_cues(cues, max_chars), &path)?;
    Ok(path)
}

/// A sentence cue cut into platform-sized cues.
///
/// Each cut goes at the best boundary inside the text that still fits; a cue
/// that already fits is passed through untouched. See
/// [`segmenter::split_by`] for the splitting itself.
pub fn split_cues(cues: &[Cue], max_chars: usize) -> Vec<Cue> {
    let max_chars = max_chars.max(8);
    segmenter::split_by(cues, |toks, start, end| {
        segmenter::chars(&toks[start..end]) <= max_chars
    })
}

/// Re-measure the session's sentences against the dubbed track.
///
/// `dubbed` is a word-level transcript of the dubbed audio, which runs on the
/// same timeline: every sentence starts where the original cue started. Only
/// its *timings* are used — the text stays the session's own, so a name or a
/// technical term whisper mis-hears cannot leak into the subtitle. When whisper
/// heard exactly as many words as the sentence has, its per-word timings are
/// used as-is; otherwise they are spread across the sentence in proportion to
/// word length, which is close enough for a speech synthesiser's steady rate.
pub fn retime_to_dubbed(cues: &[Cue], dubbed: &[Word]) -> Vec<Cue> {
    let mut out = Vec::with_capacity(cues.len());
    let mut cursor = 0usize;
    for (i, cue) in cues.iter().enumerate() {
        let window_end = cues.get(i + 1).map(|c| c.start).unwrap_or(f64::INFINITY);
        while cursor < dubbed.len() && dubbed[cursor].start < cue.start {
            cursor += 1;
        }
        let begin = cursor;
        while cursor < dubbed.len() && dubbed[cursor].start < window_end {
            cursor += 1;
        }
        let heard = &dubbed[begin..cursor];
        let texts: Vec<String> = cue.text.split_whitespace().map(|w| w.to_string()).collect();

        let (span, words) = match heard.first() {
            Some(first) => {
                let end = heard
                    .last()
                    .map(|w| w.end)
                    .unwrap_or(first.end)
                    .max(first.start);
                if heard.len() == texts.len() {
                    let words = texts
                        .iter()
                        .zip(heard)
                        .map(|(text, w)| Word {
                            text: text.clone(),
                            start: w.start,
                            end: w.end,
                            score: w.score,
                        })
                        .collect();
                    ((first.start, end), words)
                } else {
                    (
                        (first.start, end),
                        into_words(spread(&texts, first.start, end)),
                    )
                }
            }
            // Nothing was heard in this sentence's slot — a clip that failed or
            // came out silent — so the sentence keeps the original voice's slot.
            None => (
                (cue.start, cue.end),
                into_words(spread(&texts, cue.start, cue.end)),
            ),
        };

        out.push(Cue {
            index: i,
            start: span.0,
            end: span.1,
            text: cue.text.clone(),
            words,
            source_path: cue.source_path.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segmenter::{boundary, score, stem, tokens, Tok, DANGLING, PREPOSITIONS};

    fn cue(text: &str, start: f64, end: f64) -> Cue {
        let mut words = Vec::new();
        let step = (end - start) / text.split_whitespace().count().max(1) as f64;
        for (i, w) in text.split_whitespace().enumerate() {
            words.push(Word {
                text: w.to_string(),
                start: start + step * i as f64,
                end: start + step * (i + 1) as f64,
                score: 1.0,
            });
        }
        Cue {
            index: 0,
            start,
            end,
            text: text.to_string(),
            words,
            source_path: Some("/media/lecture.mp4".to_string()),
        }
    }

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

    fn words_of(text: &str) -> Vec<Tok> {
        tokens(&cue(text, 0.0, 1.0))
    }

    fn reassembled(cues: &[Cue]) -> String {
        cues.iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_cue_that_already_fits_is_untouched() {
        let out = split_cues(&[cue("Hello there, world.", 1.0, 3.0)], DEFAULT_MAX_CHARS);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Hello there, world.");
        assert_eq!(out[0].start, 1.0);
        assert_eq!(out[0].end, 3.0);
    }

    #[test]
    fn a_long_sentence_breaks_at_its_comma() {
        let original = cue(
            "The report was late, because the train broke down near the station.",
            2.0,
            12.0,
        );
        let out = split_cues(&[original.clone()], DEFAULT_MAX_CHARS);
        assert!(out.len() > 1, "expected a split, got {out:?}");
        assert_eq!(reassembled(&out), original.text);
        // The comma is the strongest boundary in the first line's window, so
        // that is where the first cue ends.
        assert_eq!(out[0].text, "The report was late,");
        for c in &out {
            assert!(c.text.chars().count() <= DEFAULT_MAX_CHARS, "{c:?}");
        }
    }

    #[test]
    fn a_break_lands_before_a_conjunction() {
        let original = cue(
            "We walked to the market and bought some fresh apples for the party today.",
            0.0,
            9.0,
        );
        let out = split_cues(&[original.clone()], DEFAULT_MAX_CHARS);
        assert!(out.len() > 1);
        assert_eq!(reassembled(&out), original.text);
        for c in &out {
            assert!(c.text.chars().count() <= DEFAULT_MAX_CHARS, "{c:?}");
            assert!(c.text.len() > 0);
        }
    }

    #[test]
    fn a_line_never_ends_on_a_hanging_word() {
        let original = cue(
            "The quick brown fox jumped over the lazy dog and ran away into the woods today.",
            0.0,
            8.0,
        );
        let out = split_cues(&[original.clone()], DEFAULT_MAX_CHARS);
        assert_eq!(reassembled(&out), original.text);
        assert!(out.len() > 1);
        for c in &out {
            assert!(c.text.chars().count() <= DEFAULT_MAX_CHARS, "{c:?}");
            let last = stem(c.text.split_whitespace().last().unwrap());
            assert!(!DANGLING.contains(&last.as_str()), "hanging word in {c:?}");
            assert!(
                !PREPOSITIONS.contains(&last.as_str()),
                "hanging word in {c:?}"
            );
        }
    }

    #[test]
    fn boundaries_reject_hanging_words_and_accept_clauses() {
        let toks = words_of("we walked all the way to the sea");
        assert!(boundary(&toks, 4).is_none(), "ends on \"the\"");
        assert!(boundary(&toks, 6).is_none(), "ends on \"to\"");
        assert!(boundary(&toks, 2).is_some());

        let toks = words_of("we rested, and then we swam");
        assert_eq!(score(&toks, 2), 4, "a comma outranks a conjunction");
        let toks = words_of("we rested and then we swam");
        assert_eq!(score(&toks, 2), 3, "a coordinating conjunction");
        let toks = words_of("it failed because the disk was full");
        assert_eq!(score(&toks, 2), 2, "a subordinating conjunction");
        let toks = words_of("the book which she wrote");
        assert_eq!(score(&toks, 2), 2, "a relative pronoun");
        assert!(boundary(&words_of("we said not a word"), 2).is_none());
    }

    #[test]
    fn cue_timings_stay_inside_the_sentence_and_move_forward() {
        let out = split_cues(
            &[cue(
                "Your session is running low and you should save your work before the browser closes.",
                4.0,
                14.0,
            )],
            DEFAULT_MAX_CHARS,
        );
        assert!(out.len() > 1);
        assert!(out[0].start >= 4.0);
        for c in &out {
            assert!(c.end > c.start, "{c:?}");
            assert!(c.start >= 4.0 && c.end <= 14.0 + 1e-9, "{c:?}");
        }
        for pair in out.windows(2) {
            assert!(pair[1].start >= pair[0].end - 1e-9, "{pair:?}");
        }
        assert_eq!(out.last().unwrap().end, 14.0);
    }

    #[test]
    fn a_cue_without_word_timings_is_spread_across_its_span() {
        let out = split_cues(
            &[untimed(
                "This cue came from a hand-written subtitle file.",
                6.0,
                9.0,
            )],
            DEFAULT_MAX_CHARS,
        );
        assert_eq!(
            reassembled(&out),
            "This cue came from a hand-written subtitle file."
        );
        assert!((out[0].start - 6.0).abs() < 1e-9);
        assert!((out.last().unwrap().end - 9.0).abs() < 1e-9);
        let words: Vec<&Word> = out.iter().flat_map(|c| c.words.iter()).collect();
        assert_eq!(words.len(), 8);
        for pair in words.windows(2) {
            assert!(pair[1].start >= pair[0].start, "{pair:?}");
        }
    }

    #[test]
    fn a_dubbed_sentence_keeps_its_text_and_takes_the_dubbed_timing() {
        let session = vec![
            cue("Hello there world.", 0.0, 4.0),
            cue("Goodbye for now.", 4.0, 8.0),
        ];
        // The dubbed track says the same words, but the synthesiser needed less
        // time: the first sentence ends at 2.5 s, not 4.0.
        let heard = vec![
            Word {
                text: "Hello".into(),
                start: 0.1,
                end: 0.7,
                score: 1.0,
            },
            Word {
                text: "there".into(),
                start: 0.7,
                end: 1.2,
                score: 1.0,
            },
            Word {
                text: "world.".into(),
                start: 1.2,
                end: 2.5,
                score: 1.0,
            },
            Word {
                text: "Goodbye".into(),
                start: 4.0,
                end: 4.8,
                score: 1.0,
            },
            Word {
                text: "for".into(),
                start: 4.8,
                end: 5.0,
                score: 1.0,
            },
            Word {
                text: "now.".into(),
                start: 5.0,
                end: 5.9,
                score: 1.0,
            },
        ];
        let out = retime_to_dubbed(&session, &heard);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "Hello there world.");
        assert!((out[0].start - 0.1).abs() < 1e-9);
        assert!((out[0].end - 2.5).abs() < 1e-9);
        // The timings are whisper's own, not the original voice's.
        assert!((out[0].words[2].start - 1.2).abs() < 1e-9);
        assert_eq!(out[1].text, "Goodbye for now.");
        assert!((out[1].end - 5.9).abs() < 1e-9);
    }

    #[test]
    fn a_dubbed_span_is_still_split_into_platform_cues() {
        let session = vec![cue(
            "The report was late, because the train broke down near the station.",
            0.0,
            12.0,
        )];
        let heard: Vec<Word> = session[0]
            .words
            .iter()
            .map(|w| Word {
                text: w.text.clone(),
                start: w.start / 3.0,
                end: w.end / 3.0,
                score: 1.0,
            })
            .collect();
        let retimed = retime_to_dubbed(&session, &heard);
        assert!(
            (retimed[0].end - 4.0).abs() < 1e-6,
            "dubbed speech is faster"
        );
        let out = split_cues(&retimed, DEFAULT_MAX_CHARS);
        assert!(out.len() > 1);
        assert_eq!(reassembled(&out), session[0].text);
        assert!(out.last().unwrap().end <= 4.0 + 1e-9, "{:?}", out.last());
    }

    #[test]
    fn a_sentence_the_dubbed_track_missed_keeps_the_original_slot() {
        let session = vec![cue("Nothing was heard here at all.", 5.0, 8.0)];
        let out = retime_to_dubbed(&session, &[]);
        assert_eq!(out[0].start, 5.0);
        assert_eq!(out[0].end, 8.0);
        assert_eq!(out[0].text, "Nothing was heard here at all.");
        assert_eq!(out[0].words.len(), 6);
    }

    #[test]
    fn the_subtitle_is_named_after_the_video() {
        assert_eq!(
            platform_subtitle_path("/videos/lecture.mp4"),
            "/videos/lecture.srt"
        );
        assert_eq!(
            platform_subtitle_path("/videos/lecture.MKV"),
            "/videos/lecture.srt"
        );
        // Deliberately *not* the segmented subtitle's name.
        assert_ne!(
            platform_subtitle_path("/videos/lecture.mp4"),
            "/videos/lecture_full_sentence.srt"
        );
    }
}