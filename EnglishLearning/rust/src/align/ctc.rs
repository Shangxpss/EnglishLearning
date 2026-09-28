//! CTC decoding and Viterbi forced alignment.
//!
//! wav2vec2-base-960h is a CTC model over a 32-symbol vocabulary: one blank,
//! three special tokens, a word separator, and the 26 letters plus the
//! apostrophe. Two things are useful here:
//!
//! * [`greedy_decode`] — the plain "collapse repeats, drop blanks" decode, used
//!   to check that the hand-written encoder emits sensible acoustic evidence at
//!   all before trusting it for timing.
//! * [`forced_align`] — given a *known* transcript, the Viterbi path through the
//!   CTC trellis says which frames each symbol occupies, which is what turns a
//!   transcript into word timings.
//!
//! Everything is computed in log space with `f32::NEG_INFINITY` for
//! unreachable states, so no explicit guard is needed for the impossible
//! transitions.

use super::wav2vec2::VOCAB;

/// The CTC blank (`<pad>`).
pub const BLANK: usize = 0;
/// The `|` symbol that separates words in the 960h vocabulary.
pub const WORD_SEP: usize = 4;

/// Vocabulary index → character, exactly as `vocab.json` lists it.
///
/// The four leading entries are control tokens and map to `None`:
/// `0 <pad>` (blank), `1 <s>`, `2 </s>`, `3 <unk>`.
const VOCAB_CHARS: [char; VOCAB] = [
    '\u{0}', '\u{0}', '\u{0}', '\u{0}', ' ', //
    'E', 'T', 'A', 'O', 'N', 'I', 'H', 'S', 'R', 'D', 'L', 'U', 'M', 'W', 'C', 'F', 'G', 'Y', 'P',
    'B', 'V', 'K', '\'', 'X', 'J', 'Q', 'Z',
];

/// Index of the first letter symbol; indices below it are control tokens.
const FIRST_LETTER: usize = 5;

/// The character for a vocabulary index, or `None` for the control tokens
/// (`<pad>`, `<s>`, `</s>`, `<unk>`). [`WORD_SEP`] decodes to a space.
pub fn token_char(token: usize) -> Option<char> {
    if token >= VOCAB || (token < FIRST_LETTER && token != WORD_SEP) {
        return None;
    }
    Some(VOCAB_CHARS[token])
}

/// The vocabulary index for a character (case-insensitive), or `None` if the
/// symbol is not in the 960h vocabulary (digits, most punctuation).
pub fn letter_token(c: char) -> Option<usize> {
    let c = c.to_ascii_uppercase();
    (FIRST_LETTER..VOCAB).find(|&t| VOCAB_CHARS[t] == c)
}

/// One word's tokens within [`WordTokens::tokens`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordRange {
    /// Index of the word in the slice passed to [`encode_words`].
    pub word: usize,
    /// First token of the word.
    pub start: usize,
    /// One past the last token of the word.
    pub end: usize,
}

/// A transcript split into per-word token sequences.
pub struct WordTokens {
    /// Every token in order, words separated by [`WORD_SEP`].
    pub tokens: Vec<usize>,
    /// The token span of each *alignable* word, in order. Words with no
    /// vocabulary characters are absent, so the caller can tell which input
    /// words produced no tokens.
    pub word_ranges: Vec<WordRange>,
}

/// Turn words into vocabulary tokens, inserting [`WORD_SEP`] between them.
///
/// Characters outside the vocabulary are dropped rather than failing: a
/// transcript carrying digits or punctuation still has an alignable skeleton,
/// and refusing the whole utterance over one comma would be worse. `Err` is
/// reserved for the case where nothing survives.
pub fn encode_words<S: AsRef<str>>(words: &[S]) -> Result<WordTokens, String> {
    let mut tokens = Vec::new();
    let mut word_ranges: Vec<WordRange> = Vec::new();

    for (word, text) in words.iter().enumerate() {
        let start = tokens.len();
        for c in text.as_ref().chars() {
            if let Some(t) = letter_token(c) {
                tokens.push(t);
            }
        }
        if tokens.len() == start {
            // Nothing alignable in this word (e.g. "—"): skip it entirely so the
            // ranges only ever describe real words.
            continue;
        }
        if !word_ranges.is_empty() {
            // The separator goes *at* `start`, so it belongs to neither word:
            // the previous word's range is unchanged and the new one begins one
            // token later.
            tokens.insert(start, WORD_SEP);
            word_ranges.push(WordRange {
                word,
                start: start + 1,
                end: tokens.len(),
            });
        } else {
            word_ranges.push(WordRange {
                word,
                start,
                end: tokens.len(),
            });
        }
    }

    if tokens.is_empty() {
        return Err("the transcript contains no characters in the ASR vocabulary".to_string());
    }
    Ok(WordTokens {
        tokens,
        word_ranges,
    })
}

/// CTC greedy decode: best token per frame, drop repeats, drop blanks.
///
/// The result is the raw letter stream — no case, no punctuation, no spaces
/// beyond where the model actually emitted `|`.
pub fn greedy_decode(logits: &[f32], frames: usize) -> String {
    let mut out = String::new();
    let mut prev = BLANK;
    for t in 0..frames {
        let row = &logits[t * VOCAB..(t + 1) * VOCAB];
        let (best, _) =
            row.iter()
                .enumerate()
                .fold((BLANK, f32::NEG_INFINITY), |(bi, bv), (i, &v)| {
                    if v > bv {
                        (i, v)
                    } else {
                        (bi, bv)
                    }
                });
        // A repeated symbol needs a blank between it and itself.
        if best != prev && best != BLANK {
            out.push(token_char(best).unwrap_or('?'));
        }
        prev = best;
    }
    out.trim().to_string()
}

/// Per-frame confidence: the largest softmax probability in each frame.
///
/// This is the natural score for a forced alignment — a frame whose evidence is
/// ambiguous has no single dominant symbol and scores low.
pub fn frame_confidence(logits: &[f32], frames: usize) -> Vec<f32> {
    let mut out = vec![0f32; frames];
    for (t, slot) in out.iter_mut().enumerate() {
        let row = &logits[t * VOCAB..(t + 1) * VOCAB];
        let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
        *slot = 1.0 / sum;
    }
    out
}

/// One transcript token, placed on the frames it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignedToken {
    /// Vocabulary index of the symbol.
    pub token: usize,
    /// First frame (inclusive) the symbol occupies.
    pub start: usize,
    /// Last frame (inclusive) the symbol occupies.
    pub end: usize,
}

/// Viterbi forced alignment of `targets` against CTC `logits`.
///
/// Builds the standard extended sequence `blank, t₀, blank, t₁, …, blank` and
/// finds the highest-scoring monotonic path through it. Transitions are exactly
/// CTC's: stay, advance one (blank→symbol), or advance two (symbol→different
/// symbol); a symbol can only repeat after an intervening blank.
///
/// Returns one entry per target token, in order.
pub fn forced_align(
    logits: &[f32],
    frames: usize,
    targets: &[usize],
) -> Result<Vec<AlignedToken>, String> {
    if logits.len() != frames * VOCAB {
        return Err(format!(
            "emissions hold {} values, expected {} (frames × vocab)",
            logits.len(),
            frames * VOCAB
        ));
    }
    if frames == 0 {
        return Err("cannot align: the audio produced no output frames".to_string());
    }
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(&bad) = targets.iter().find(|&&t| t >= VOCAB) {
        return Err(format!("target token {bad} is outside the vocabulary"));
    }
    if targets.len() > frames {
        return Err(format!(
            "cannot align {} tokens onto only {frames} frames",
            targets.len()
        ));
    }

    let logp = log_softmax(logits, frames);
    let states = 2 * targets.len() + 1;

    // `alpha[s]` for the current frame, and the backpointer per (frame, state):
    // 0 = stay, 1 = came from s-1, 2 = came from s-2.
    let mut prev = vec![f32::NEG_INFINITY; states];
    let mut cur = vec![f32::NEG_INFINITY; states];
    let mut back = vec![0u8; frames * states];

    prev[0] = logp[BLANK];
    prev[1] = logp[targets[0]];

    for t in 1..frames {
        let row = &logp[t * VOCAB..(t + 1) * VOCAB];
        let b = &mut back[t * states..(t + 1) * states];
        for s in 0..states {
            let token = if s % 2 == 0 { BLANK } else { targets[s / 2] };
            let mut best = prev[s];
            let mut from = 0u8;
            if s > 0 && prev[s - 1] > best {
                best = prev[s - 1];
                from = 1;
            }
            // Two-symbol jump, only between *different* symbols (CTC requires a
            // blank between repeats); skipped for a blank state, whose s-2
            // predecessor would be a repeat too.
            if s % 2 == 1 && s >= 2 && targets[s / 2] != targets[s / 2 - 1] && prev[s - 2] > best {
                best = prev[s - 2];
                from = 2;
            }
            cur[s] = best + row[token];
            b[s] = from;
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    // Backtrace from the better of the two legal final states.
    let mut state = if prev[states - 1] >= prev[states - 2] {
        states - 1
    } else {
        states - 2
    };
    let mut path = vec![0usize; frames];
    for t in (0..frames).rev() {
        path[t] = state;
        if t == 0 {
            break;
        }
        state = match back[t * states + state] {
            1 => state - 1,
            2 => state - 2,
            _ => state,
        };
    }

    // Symbol states are odd; record the span each one covers.
    let mut first = vec![usize::MAX; targets.len()];
    let mut last = vec![0usize; targets.len()];
    for (t, &s) in path.iter().enumerate() {
        if s % 2 == 1 {
            let i = s / 2;
            if first[i] == usize::MAX {
                first[i] = t;
            }
            last[i] = t;
        }
    }

    targets
        .iter()
        .enumerate()
        .map(|(i, &token)| {
            if first[i] == usize::MAX {
                return Err(format!(
                    "the alignment path never reaches token {i}; the transcript and the audio disagree"
                ));
            }
            Ok(AlignedToken {
                token,
                start: first[i],
                end: last[i],
            })
        })
        .collect()
}

/// Row-wise log-softmax, numerically stable.
fn log_softmax(logits: &[f32], frames: usize) -> Vec<f32> {
    let mut out = logits.to_vec();
    for t in 0..frames {
        let row = &mut out[t * VOCAB..(t + 1) * VOCAB];
        let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let sum: f32 = row.iter().map(|v| (v - max).exp()).sum();
        let log_sum = sum.ln();
        for v in row.iter_mut() {
            *v = (*v - max) - log_sum;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-hot emissions: 10 over the chosen symbol, 0 elsewhere.
    fn one_hot(seq: &[usize]) -> Vec<f32> {
        let mut out = vec![0f32; seq.len() * VOCAB];
        for (t, &token) in seq.iter().enumerate() {
            out[t * VOCAB + token] = 10.0;
        }
        out
    }

    #[test]
    fn vocabulary_round_trips_letters() {
        assert_eq!(letter_token('A'), Some(7));
        assert_eq!(letter_token('a'), Some(7));
        assert_eq!(letter_token('z'), Some(31));
        assert_eq!(letter_token('1'), None);
        assert_eq!(token_char(7), Some('A'));
        assert_eq!(token_char(BLANK), None);
        assert_eq!(token_char(WORD_SEP), Some(' '));
    }

    #[test]
    fn greedy_decode_collapses_repeats_and_drops_blanks() {
        // A A blank A B blank → "AAB"
        let logits = one_hot(&[7, 7, BLANK, 7, 24, BLANK]);
        assert_eq!(greedy_decode(&logits, 6), "AAB");
    }

    #[test]
    fn encode_words_marks_every_word_range() {
        let w = encode_words(&["HELLO", "world!"]).unwrap();
        // HELLO | WORLD
        assert_eq!(w.tokens.len(), 5 + 1 + 5);
        assert_eq!(w.tokens[5], WORD_SEP);
        assert_eq!(
            w.word_ranges,
            vec![
                WordRange {
                    word: 0,
                    start: 0,
                    end: 5
                },
                WordRange {
                    word: 1,
                    start: 6,
                    end: 11
                },
            ]
        );
        // A word with nothing alignable is skipped, not turned into an empty range.
        let w = encode_words(&["HI", "--", "OK"]).unwrap();
        assert_eq!(w.word_ranges.len(), 2);
        assert_eq!(
            w.word_ranges[1].word, 2,
            "the skipped word must not be reported"
        );
    }

    #[test]
    fn forced_align_places_symbols_on_their_frames() {
        // Frames: A A blank B B blank
        let logits = one_hot(&[7, 7, BLANK, 24, 24, BLANK]);
        let targets = [7, 24];
        let aligned = forced_align(&logits, 6, &targets).unwrap();
        assert_eq!(
            aligned[0],
            AlignedToken {
                token: 7,
                start: 0,
                end: 1
            }
        );
        assert_eq!(
            aligned[1],
            AlignedToken {
                token: 24,
                start: 3,
                end: 4
            }
        );
    }

    #[test]
    fn forced_align_handles_a_repeated_symbol() {
        // "LL": the second L needs the intervening blank to be reachable.
        let logits = one_hot(&[15, 15, BLANK, 15, 15]);
        let aligned = forced_align(&logits, 5, &[15, 15]).unwrap();
        assert_eq!(aligned.len(), 2);
        assert!(aligned[0].end < aligned[1].start, "{aligned:?}");
    }

    #[test]
    fn forced_align_rejects_impossible_targets() {
        let logits = one_hot(&[BLANK, BLANK]);
        // Three symbols cannot occupy two frames.
        assert!(forced_align(&logits, 2, &[7, 24, 15]).is_err());
        assert!(forced_align(&logits, 3, &[VOCAB]).is_err());
    }

    #[test]
    fn frame_confidence_is_a_probability() {
        let logits = one_hot(&[7, BLANK]);
        let conf = frame_confidence(&logits, 2);
        assert!(conf.iter().all(|&c| c > 0.0 && c <= 1.0), "{conf:?}");
        assert!(conf[0] > 0.9, "{conf:?}");
    }
}
