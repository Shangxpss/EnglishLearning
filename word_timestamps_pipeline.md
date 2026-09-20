# Word-level timestamps → sentence-segmented transcripts (Rust)

> **Goal.** audio/video → highly accurate words + word timestamps → reconstruct
> natural sentences → put punctuation in the right places → produce a final
> timestamped transcript.

If that is your goal, then I would not try to solve everything with Whisper's
native timestamps. The best architecture is a multi-stage pipeline:

```text
Audio / Video
     │
     ▼
  FFmpeg
     │
     ▼
VAD / speech segmentation
     │
     ▼
ASR: Whisper / Qwen
     │
     │ text + rough segments
     ▼
Forced alignment
     │
     │ accurate word timestamps
     ▼
Word normalization
     │
     ▼
Sentence boundary detection
     │
     ▼
Punctuation restoration
     │
     ▼
Timestamp redistribution
     │
     ▼
Final transcript
```

This is essentially the important idea behind WhisperX: ASR generates the words,
while a separate phoneme-based forced aligner determines when those words were
actually spoken. WhisperX specifically uses VAD + ASR + forced phoneme alignment
to improve word-level timing.

## 1. Don't use Whisper's word timestamps as your final timestamps

This is probably the most important point.

Whisper can produce token/word-level timestamps, and whisper.cpp even exposes
token timestamps and experimental DTW timestamps.

But if you want *"as accurate as possible"*, I'd treat those timestamps as rough
alignment, not ground truth.

The better approach is:

```text
Whisper
   ↓
"I really want to learn Rust because it is interesting"
   ↓
Forced alignment
   ↓
I            0.20 - 0.31
really       0.32 - 0.61
want         0.62 - 0.79
to           0.80 - 0.86
learn        0.87 - 1.12
Rust         1.13 - 1.38
because      1.51 - 1.79
it           1.80 - 1.88
is           1.89 - 2.00
interesting  2.01 - 2.42
```

WhisperX's central technique is exactly this: use ASR for the transcript and
forced alignment for more precise word timing.

## 2. Your Rust program should be an orchestrator

I wouldn't try to implement the speech models themselves in Rust initially.
Rust should manage the pipeline:

```text
Rust
 ├── audio extraction
 ├── VAD
 ├── ASR
 ├── alignment
 ├── text normalization
 ├── sentence segmentation
 ├── punctuation
 ├── timestamp reconciliation
 └── JSON / SRT / VTT output
```

This is much more maintainable than trying to implement Whisper, wav2vec2,
phoneme alignment, punctuation models, etc. yourself.

There are now native Rust options worth considering. For example `native-whisperx`
is specifically a Rust workflow library combining transcription, wav2vec2
alignment, optional diarization, and output writing. There is also a Rust
whisper.cpp binding with token timestamps and DTW timestamp support.

## 3. Recommended data structure

I would not immediately create sentences. Keep the lowest-level representation
first:

```rust
#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    pub start: f32,
    pub end: f32,
    pub confidence: Option<f32>,
}
```

Then:

```rust
#[derive(Debug, Clone)]
pub struct Sentence {
    pub words: Vec<Word>,
    pub start: f32,
    pub end: f32,
    pub text: String,
}
```

And finally:

```rust
#[derive(Debug)]
pub struct Transcript {
    pub sentences: Vec<Sentence>,
}
```

The important design principle is: **never throw away the word-level
information.** For example:

```json
{
  "text": "I really want to learn Rust.",
  "start": 1.24,
  "end": 3.82,
  "words": [
    { "text": "I",      "start": 1.24, "end": 1.31 },
    { "text": "really", "start": 1.32, "end": 1.67 },
    { "text": "want",   "start": 1.68, "end": 1.87 }
  ]
}
```

This makes everything afterward much easier.

## 4. The hardest part isn't timestamps

The really interesting problem in your question is:

> How do I determine where one sentence ends and another begins?

Don't use silence alone. For example:

```text
I went to the store ... because I needed some milk.
```

The speaker might pause:

```text
store [pause] because
```

but that's still one sentence. And sometimes:

```text
Well ... I don't know.
```

is two logical units despite the pause. So sentence detection should combine:

```text
                     ┌── silence
                     │
                     ├── ASR segmentation
Sentence boundary ───┼── punctuation model
                     │
                     ├── linguistic structure
                     │
                     └── maximum sentence length
```

## 5. Don't add punctuation before alignment

This is another important architectural decision.

Suppose ASR gives:

```text
i went to the store because i needed some milk
```

Don't immediately turn it into:

```text
I went to the store, because I needed some milk.
```

Instead:

**Step 1 — ASR**

```text
i went to the store because i needed some milk
```

**Step 2 — alignment**

```text
i        0.10 - 0.18
went     0.19 - 0.42
to       0.43 - 0.51
the      0.52 - 0.60
store    0.61 - 0.92
because  1.05 - 1.31
i        1.32 - 1.38
needed   1.39 - 1.72
some     1.73 - 1.90
milk     1.91 - 2.21
```

**Step 3 — sentence/punctuation analysis**

```text
I went to the store because I needed some milk.
```

**Step 4 — attach punctuation to the existing words**

```text
I        0.10 - 0.18
went     0.19 - 0.42
to       0.43 - 0.51
the      0.52 - 0.60
store,   0.61 - 0.92
because  1.05 - 1.31
I        1.32 - 1.38
needed   1.39 - 1.72
some     1.73 - 1.90
milk.    1.91 - 2.21
```

Notice: **punctuation doesn't have its own timestamp.** The comma/period belongs
to a word boundary.

## 6. Punctuation should be a separate stage

This is especially important if you're building an English-learning application.

You might receive:

```text
i don't know what you're talking about
```

The punctuation model could produce:

```text
I don't know what you're talking about.
```

But sometimes:

```text
I don't know. What are you talking about?
```

Those are different sentence structures.

Therefore punctuation restoration is really a small NLP problem rather than a
simple regex problem.

There are models specifically designed for punctuation
restoration/capitalization; Hugging Face's Speechbox, for example, has a
punctuation-restoration workflow using Whisper.

## 7. I would use a two-pass sentence algorithm

Rather than asking an LLM to blindly reconstruct the entire transcript, I'd do:

**Pass 1 — candidate boundaries.** Create candidate sentence boundaries based on:

- long silence
- ASR segment boundary
- speaker change
- existing punctuation
- linguistic boundary probability

For example:

```text
I went to the store
        ↑
     candidate

because I needed some milk
```

**Pass 2 — sentence scoring.** Give each possible boundary a score:

```text
boundary_score =
      w1 * silence_score
    + w2 * punctuation_score
    + w3 * linguistic_score
    + w4 * asr_segment_score
    + w5 * speaker_change_score
```

Then choose the segmentation with the best global score.

This is much better than:

```rust
if silence > 500 {
    new_sentence();
}
```

## 8. Use an LLM only for difficult cases

I wouldn't make an LLM responsible for timestamps. Instead:

```text
ASR
 ↓
alignment
 ↓
sentence candidates
 ↓
punctuation model
 ↓
LLM only when ambiguous
```

The LLM receives something like:

```text
Raw transcript:

i think we should go there because
we don't have much time

Candidate boundaries:

1. after "there"
2. after "because"
3. after "time"
```

and returns:

```text
I think we should go there because we don't have much time.
```

But the timestamps remain owned by your alignment layer. This is crucial.

Never let an LLM rewrite the transcript and then attempt to match the rewritten
text back to audio if you can avoid it.

## 9. The alignment layer should be authoritative

Think of your system as having two independent dimensions:

```text
                TEXT
                 │
        ┌────────┴────────┐
        │                 │
     ASR text        punctuation
        │                 │
        └────────┬────────┘
                 │
                 ▼
            final text


                AUDIO
                 │
                 ▼
          forced alignment
                 │
                 ▼
           word timestamps
```

Then combine them:

```rust
Word {
    text: "hello",
    start: 1.32,
    end: 1.74,
    punctuation: ",",
}
```

This separation makes your system much more robust.

## 10. For English, forced alignment is particularly attractive

For English, WhisperX uses wav2vec2-based phoneme alignment, and its
documentation explicitly describes this as the mechanism for obtaining accurate
word-level timestamps.

The conceptual pipeline is:

```text
Audio
 │
 ▼
Whisper
 │
 │ "I want to learn Rust"
 ▼
Known transcript
 │
 ▼
phoneme recognizer
 │
 ▼
forced alignment
 │
 ├── I     0.20–0.31
 ├── want  0.32–0.52
 ├── to    0.53–0.61
 ├── learn 0.62–0.91
 └── Rust  0.92–1.22
```

That's considerably more appropriate for your requirement than trying to infer
exact word boundaries from Whisper's timestamp tokens.

## 11. Rust technology stack I'd choose

If I were building this today, I'd structure it roughly like:

```text
Rust application
 │
 ├── ffmpeg
 │    └── audio extraction/resampling
 │
 ├── VAD
 │    └── speech/non-speech detection
 │
 ├── ASR
 │    ├── Whisper
 │    └── or Qwen ASR
 │
 ├── forced alignment
 │    └── wav2vec2 / dedicated aligner
 │
 ├── NLP
 │    ├── word normalization
 │    ├── sentence segmentation
 │    └── punctuation restoration
 │
 └── output
      ├── JSON
      ├── SRT
      ├── VTT
      └── ASS
```

Interestingly, there is now a Rust `qwen-asr` crate that supports CPU-only
Qwen3-ASR and also exposes a forced-alignment model variant for word-level
timestamps.

So I'd investigate that seriously if your goal is a mostly native Rust/CPU
implementation.

## 12. If CPU-only matters

Since you've previously been interested in running speech/TTS locally on a
normal PC without a GPU, I'd distinguish two goals.

**Maximum accuracy**

```text
Whisper large
  + wav2vec2 forced alignment
  + punctuation model

GPU strongly preferred.
```

**Practical CPU application**

```text
Qwen3-ASR 0.6B
  + forced alignment
  + Rust processing
```

could be much easier to deploy locally. The current Rust `qwen-asr` crate
specifically advertises CPU-only inference and automatic SIMD/BLAS optimization.

## 13. Don't confuse word timestamp with sentence timestamp

You ultimately want:

```text
Sentence
 ├── start = first word.start
 ├── end   = last word.end
 └── words
```

For example:

```json
{
  "text": "I really want to learn Rust.",
  "start": 0.82,
  "end": 3.21,
  "words": [
    { "text": "I",       "start": 0.82, "end": 0.91 },
    { "text": "really",  "start": 0.92, "end": 1.31 },
    { "text": "want",    "start": 1.32, "end": 1.56 },
    { "text": "to",      "start": 1.57, "end": 1.66 },
    { "text": "learn",   "start": 1.67, "end": 2.01 },
    { "text": "Rust.",   "start": 2.02, "end": 3.21 }
  ]
}
```

This gives you a very useful structure for an English-learning application:

```text
sentence
  ↓
word
  ↓
audio range
```

You can then implement:

- click a word → play that word
- highlight word while audio plays
- click sentence → replay sentence
- repeat sentence
- slow sentence playback
- show translation
- show pronunciation
- vocabulary extraction

## 14. One thing I would NOT do

I would not do this:

```text
Whisper
 ↓
sentence text
 ↓
LLM rewrite
 ↓
split rewritten text
 ↓
guess timestamps
```

You will eventually encounter:

```text
ASR: I wanna go to the store
LLM: I want to go to the store.
```

Now:

```text
wanna
```

doesn't exist in the rewritten text anymore, and your timestamp alignment
becomes problematic.

Instead:

```text
Audio
 ↓
ASR
 ↓
"I wanna go to the store"
 ↓
ALIGN
 ↓
timestamps
 ↓
PUNCTUATION
 ↓
"I wanna go to the store."
```

If you need text correction, keep both:

```text
spoken_text:     "I wanna go to the store."
normalized_text: "I want to go to the store."
```

and retain the mapping:

```text
wanna → want to
```

That is especially valuable for an English-learning system.

## My recommended architecture

If accuracy is your #1 priority, I'd build this:

```text
                    ┌───────────────┐
                    │ Audio / Video │
                    └───────┬───────┘
                            │
                         FFmpeg
                            │
                            ▼
                          VAD
                            │
                    speech segments
                            │
                            ▼
                     Whisper / Qwen
                            │
                       raw words
                            │
                            ▼
                  ┌───────────────────┐
                  │ Forced Alignment  │
                  │   wav2vec2 etc.   │
                  └─────────┬─────────┘
                            │
                   accurate word times
                            │
                            ▼
                  ┌───────────────────┐
                  │ Word Normalizer   │
                  └─────────┬─────────┘
                            │
                            ▼
                  ┌───────────────────┐
                  │ Sentence Boundary │
                  │     Detection     │
                  └─────────┬─────────┘
                            │
                            ▼
                  ┌───────────────────┐
                  │    Punctuation    │
                  │    Restoration    │
                  └─────────┬─────────┘
                            │
                            ▼
                  ┌───────────────────┐
                  │ Timestamp Mapper  │
                  └─────────┬─────────┘
                            │
                            ▼
                    ┌──────────────┐
                    │ Rust structs │
                    └──────┬───────┘
                           │
                ┌──────────┼──────────┐
                ▼          ▼          ▼
               JSON       VTT        SRT
```

The most important technical decision is this: **use ASR for "what was said,"
forced alignment for "when each word was said," and a separate NLP/punctuation
stage for "how the transcript should be written."**

That separation gives you much better accuracy and makes the Rust implementation
substantially easier to reason about. WhisperX's published approach supports this
architecture, and its paper specifically reports improved word segmentation using
VAD plus forced phoneme alignment.

---

# Fit for this repository (`EnglishLearning`)

The pipeline above is a reasonable target, but most of it is **not** what this
repo does today, and two of the suggested crates collide with its hard
constraints. Read this before acting on anything above.

## What already exists here

`rust/src/` already implements the **word → sentence** half of the pipeline, in
pure Rust and with no models at all:

| Piece | Where | Notes |
| --- | --- | --- |
| `Word`, `Cue`, `Session` | `rust/src/models.rs` | `Cue { index, start, end, text, words[], source_path }`, `Word { text, start, end, score }` |
| `build_cues(words, duration)` | `rust/src/pipeline/transcribe.rs` | word → sentence grouper; port of Python `cue_builder.py`; `MAX_CUE_SECS = 12.0`, `MAX_CUE_WORDS = 40` |
| `is_sentence_end(word)` | `rust/src/segmenter.rs` | sentence-final `.` / `!` / `?` test |
| `finalize(cues, duration, path)` | `rust/src/segmenter.rs` | re-indexes, clamps times, fills `words` and `source_path` |
| `transcribe_words(media)` | `rust/src/pipeline/transcribe.rs` | **stub** — returns a descriptive error until an ASR backend is bound |
| `cues_from_media(...)` | `rust/src/pipeline/transcribe.rs` | orchestrator: explicit subtitle → adjacent subtitle |

So "word normalization / sentence segmentation / timestamp mapping" are largely
already coded; the missing half is exactly the **audio → words** front end.

## Constraints this document collides with

- **Single static binary, no Python, no ONNX, dependency-light.** FFmpeg is
  already statically embedded — the most expensive part of the build. Adding a
  model runtime is a step change, not an increment.
- **`qwen-asr` needs a system BLAS** — OpenBLAS on Linux, Accelerate on macOS —
  and downloads ~1.3 GB models (ASR *and* a separate aligner) from Hugging Face.
  Both break the "no user-installed dependencies" property.
- **`native-whisperx` is 0.1.x with roughly 6% documentation coverage** and pulls
  in candle, ONNX/pyannote bundles, FFmpeg, and a young `moenarch-*` crate family.
  High API-churn and supply-chain risk.
- **The network here cannot reach GitHub** (the FFmpeg clone fails — see
  `AGENTS.md`). Multi-hundred-MB Hugging Face downloads are a real risk, not a
  footnote.

## Gaps in the design as written

1. **"Best global score" is not directly implementable.** Choosing the best
   segmentation over word positions is a combinatorial search; it needs a
   DP/Viterbi pass over word indices, not the local rule shown in §7. The
   document never says so.
2. **Token stability applies to the punctuation stage too.** §14 correctly warns
   that an LLM rewrite breaks alignment, but a punctuation model can equally
   change casing/spacing (`wanna` → `want to`). Every stage that touches text
   needs a check that it only *inserted* punctuation.
3. **The data model is inconsistent.** `Word` is defined with `confidence` in §3
   and with `punctuation` in §9; capitalization is never addressed. This must be
   reconciled with the existing `models::Word` / `models::Cue`.
4. **VAD vs word boundaries is unhandled** — what happens when a VAD split lands
   inside a word.
5. **No licensing, disk footprint, CPU RTF, or offline story** for the models.

## Suggested path for this repo

Keep the current pure-Rust core as the default path and make transcription an
**opt-in extension** rather than a rewrite:

1. Keep the subtitle-supplied path as-is — it already works with zero models
   (`POST /api/session` → cues → SQLite → `/media` + `/audio`).
2. Introduce a small trait, e.g.
   `trait Asr { fn transcribe(&self, wav: &Path) -> Result<Vec<Word>, String>; }`,
   and bind the existing `transcribe_words()` stub to it.
3. Put each backend behind a cargo feature (`asr-whisper`, `asr-qwen`) so a
   default build stays static, model-free, and fast to compile.
4. Feed the backend's `Vec<Word>` into the **existing** `build_cues()` +
   `segmenter::finalize()` so the cue shape is identical whether words come from
   a subtitle or from ASR — no downstream changes needed.
5. Store word timings in the existing `cues.words` JSON column (already
   persisted); no schema change required.
6. Verify model licence, download size, and network feasibility **before**
   adopting `qwen-asr` or `native-whisperx`.
