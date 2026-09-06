# Subtitle Generation Mechanism

This document describes, end-to-end, how subtitles (SRT files) are
generated from a source video in the English Learning backend. It covers
the full pipeline — from raw audio alignment down to the exact rules that
decide *where one subtitle entry ends and the next begins* — so you can
understand why a given subtitle looks the way it does, and where to tune
it.

---

## 1. Pipeline Overview

Subtitle generation is a **by-product** of the dubbing alignment pass.
The system does **one** Whisper transcription/alignment run per video and
derives everything else from that single pass — no extra model load, no
extra network round-trip.

```
 source video (mp4)
        │
        ▼
 ┌──────────────────────────────────────────────────────┐
 │ 1. WordAligner.align(video)                          │  ← faster-whisper
 │      → List[Word]                                    │     (word-level
 │        each Word = {text, start, end, score}         │      timestamps)
 └──────────────────────────────────────────────────────┘
        │
        ▼
 ┌──────────────────────────────────────────────────────┐
 │ 2. build_aligned_cues(words, duration)               │  ← cue_builder.py
 │      → List[AlignedCue]                              │
 │        each Cue = {text, start, end, words,          │     (group words
 │                    pause_after}                      │      into sentences)
 └──────────────────────────────────────────────────────┘
        │
        ├──▶ write_srt(cues, path, max_words=10)        ← sentence-level SRT
        │       │
        │       └─▶ split_cues_for_subtitles(cues, 10)  ← THIS is where the
        │               │                                 "subtitle split" rules
        │               ▼                                 live
        │           List[AlignedCue]  (long cues split into shorter segments)
        │               │
        │               ▼
        │           <stem>.srt          (one entry per segment)
        │
<<<<<<< HEAD
        └─▶ write_word_srt(words, path, group_size=1)  ← word-level SRT
                │                                       (karaoke-style)
                ▼
            <stem>.words.srt          (one entry per word)
```

Two SRT files are always produced together:

| File                      | Purpose                                  | Granularity      |
|---------------------------|------------------------------------------|------------------|
| `<stem>.srt`              | Reading subtitles                        | Sentence / segment |
| `<stem>.words.srt`        | Karaoke-style word highlighting in the app | 1 word per entry  |
=======
        ├──▶ write_word_srt(words, path, group_size=1)  ← word-level SRT
        │       │                                       (karaoke-style)
        │       ▼
        │       <stem>.words.srt          (one entry per word)
        │
        └──▶ write_segments_srt(aligner.last_segments, path)  ← Whisper-raw SRT
                │                                               (unaltered segments)
                ▼
            <stem>.segments.srt          (one entry per Whisper segment)
```

Three SRT files are produced together:

| File                      | Purpose                                  | Granularity      | Source                         |
|---------------------------|------------------------------------------|------------------|--------------------------------|
| `<stem>.srt`              | Reading subtitles                        | Sentence / segment | Rebuilt cues + splitter        |
| `<stem>.words.srt`        | Karaoke-style word highlighting in the app | 1 word per entry  | Word stream directly           |
| `<stem>.segments.srt`     | Whisper's **unaltered** segment boundaries | 1 Whisper segment per entry | `WordAligner.last_segments` |

The third file (`.segments.srt`) is **Whisper's native output preserved
verbatim** — no cue-rebuilding, no splitting, no fragment-merging. It is
useful for diagnosing what our `cue_builder` and `split_cues_for_subtitles`
actually changed (diff it against `<stem>.srt`), and as a fallback when the
rebuilt cues feel worse than Whisper's original pause-aligned boundaries.
>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b

The naming convention (from `batch_transform.py` / `run_batch_dub.py`) makes
the subtitle name **match the converted video name**:

```
<<<<<<< HEAD
source:   10 - What is KIP--[koudaizy.com].mp4
video:   10 - What is KIP--[koudaizy.com]_nowm.mp4
srt:     10 - What is KIP--[koudaizy.com]_nowm.srt
words:   10 - What is KIP--[koudaizy.com]_nowm.words.srt
=======
source:    10 - What is KIP--[koudaizy.com].mp4
video:    10 - What is KIP--[koudaizy.com]_nowm.mp4
srt:      10 - What is KIP--[koudaizy.com]_nowm.srt
words:    10 - What is KIP--[koudaizy.com]_nowm.words.srt
segments: 10 - What is KIP--[koudaizy.com]_nowm.segments.srt
>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b
```

### 1.1 Why group into sentences, then split again? (Design rationale)

Looking at the pipeline, this looks redundant:

```
WordAligner → List[Word]  ──▶  build_aligned_cues  ──▶  split_cues_for_subtitles
                              (group into              (split long ones
                               sentences)              back into chunks)
```

> *"If WordAligner already produces word timestamps, why not emit subtitle
> chunks directly from the word stream? Why group into sentences and then
> split again?"*

The two passes solve **different problems** with **different rules**, and
the sentence grouping carries information that pure word-chunking would
lose:

| Pass        | Question it answers                  | Boundary signal             | Operates on  |
|-------------|---------------------------------------|-----------------------------|--------------|
| Cue builder | "Where does a *thought* end?"         | `. ! ?` punctuation         | Words        |
| Splitter    | "What *fits on screen*?"              | 84 chars / 10 words         | Sentences    |

The first is **linguistic**; the second is **typographic**. Skipping either
one produces worse subtitles:

#### What we'd lose by skipping `build_aligned_cues` (chunk words directly):

1. **Thought boundaries.** A naive 10-word chunker would happily cut a
   sentence mid-thought: `"I want to tell you about this because" | "we
   need to handle the case where..."`. The first segment ends on
   `"because"` — grammatically incomplete, hard to read. Grouping into
   sentences first means splits only happen *inside* sentences that are
   too long, and even then the splitter prefers comma/conjunction break
   points (§4.2).

2. **The `pause_after` signal.** Sentence boundaries align with where the
   speaker actually paused (Whisper's silence detection — §2.3). The
   splitter uses this: it extends the **last segment of each sentence**
   into the silence gap before the next sentence, so the subtitle
   doesn't vanish the instant speech ends (§4.4). With raw word chunks
   there is no "last segment of a sentence" — every chunk would either
   always or never get the extension, neither of which is right.

3. **Hallucinated-period cleanup.** Whisper frequently inserts a period
   mid-sentence (`"block."` before `"of code"`). The cue builder merges
   tiny `<3-word` fragments back into the previous sentence and strips
   the hallucinated punctuation (§3.2). A word-chunker would emit
   `"block."` and `"of code"` as separate subtitle entries.

#### What we'd lose by skipping `split_cues_for_subtitles` (emit sentences directly):

1. **Screen overflow.** A 30-word run-on sentence (~180 chars) would be
   one subtitle entry. The Netflix Timed Text Style Guide caps entries
   at 84 chars (42 per line × 2 lines) — anything longer overflows the
   player. Long sentences *must* be split for readability.

2. **Reading-speed compliance.** Subtitles that stay on screen too long
   with too much text fatigue the reader. Splitting at natural break
   points keeps each entry at a readable density.

#### The cost is essentially zero

Both passes are **pure Python over already-aligned words** — no model
re-run, no I/O, no audio re-decode. On a 45-minute video with ~6000
words, both passes combined take < 50 ms. The "extra" stage costs nothing
computationally; it only adds information.

#### The one case where it *is* redundant

If `max_words_per_segment = 0` (the default in `dubbing.py`'s API),
`split_cues_for_subtitles` returns cues unchanged — the splitter is a
no-op. In that mode, yes, the sentence grouping is the only thing that
matters and each sentence becomes exactly one SRT entry. The splitter
exists only for the `max_words_per_segment > 0` case (used by the batch
run with `= 10`).

<<<<<<< HEAD
=======
### 1.2 Why preserve Whisper's original segments? (`.segments.srt`)

The rebuilt + split pipeline (Stages 2 and 3) is good for **reading**, but
it necessarily *changes* what Whisper produced:

- `cue_builder` merges tiny fragments and strips hallucinated punctuation
- `split_cues_for_subtitles` breaks long cues at comma/conjunction points
- The interpolation backend fabricates word timestamps inside each segment

Sometimes you want to see **exactly what Whisper produced**, with no
post-processing. Two concrete use cases:

1. **Quality diagnosis** — when a subtitle feels wrong, diff `.segments.srt`
   against `.srt`:
   - If both have the same wrong word → Whisper transcription error
     (fix by upgrading model size, see §6.3).
   - If `.segments.srt` is correct but `.srt` is wrong → our cue_builder
     or splitter introduced the problem (tune §3.2 / §4.2).
   - If `.segments.srt` has a wrong boundary but right words → Whisper's
     silence detection failed (try `stable_ts` or `whisperx` backend).

2. **Direct use** — in some cases Whisper's natural pause-aligned segments
   feel better than the rebuilt cues (especially for short sentences where
   splitting/merging is unnecessary). The `.segments.srt` file is a
   ready-to-use subtitle in its own right.

### 1.3 How segments are preserved (implementation)

The `WordAligner` previously discarded Whisper's segment boundaries — it
kept only the flattened `List[Word]`. As of this change, each backend
populates `self.last_segments: List[AlignedCue]` during `align()`:

| Backend       | Where segments come from                                        |
|---------------|-----------------------------------------------------------------|
| `interpolation` | `segments, _ = self._model.transcribe(...)` — Whisper's raw output |
| `whisperx`    | `aligned["segments"]` — after forced alignment                 |
| `stable_ts`   | `result.segments` — stable-ts's segment objects                 |
| Chunked path  | Aggregated across chunks, timestamps offset to original timeline |

Each `AlignedCue` in `last_segments` carries only `text`, `start`, `end`
(no `words` list — Whisper segments don't have word-level detail in this
representation; the words live in the separate `List[Word]` return value).

The new `write_segments_srt(segments, path)` function in
[subtitle_writer.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py)
writes these as a plain SRT — no splitting, no merging, no clamping beyond
the standard `end >= start + 0.1s` safety check.

Usage in a batch script:

```python
aligner = WordAligner(model_size="tiny", language="en",
                      device="cpu", compute_type="int8")
words = aligner.align(video_path)
cues = build_aligned_cues(words, duration)

# Three subtitle outputs:
write_srt(cues, f"{base}.srt", max_words_per_segment=10)
write_word_srt(words, f"{base}.words.srt", group_size=1)
write_segments_srt(aligner.last_segments, f"{base}.segments.srt")
```

>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b
---

## 2. Stage 1 — Word Alignment (`word_aligner.py`)

### 2.1 Goal

Turn the audio track into a flat, time-ordered list of `Word` objects:

```python
@dataclass
class Word:
    text: str
    start: float   # seconds from start of audio
    end:   float   # seconds from start of audio
    score: float   # confidence in [0, 1]; 0.0 = interpolated fallback
```

### 2.2 Backends (priority order)

The aligner tries three backends in order and uses the **first one that
imports successfully**:

| # | Backend         | Accuracy        | Extra deps                  |
|---|-----------------|-----------------|-----------------------------|
| 1 | `whisperx`      | <100 ms / word  | whisperx + wav2vec2         |
| 2 | `stable_ts`     | good            | stable-whisper              |
| 3 | `interpolation` | segment-level   | (none — uses faster-whisper) |

In the current batch run, only `faster-whisper` is installed, so the
**`interpolation` backend** is used. This is the single biggest source of
"weird" subtitle timing — see §6.1.

### 2.3 Where do segments come from? (Not random)

The segments returned by `self._model.transcribe(...)` are produced by
**Whisper's own internal segmentation**, which combines three signals:

1. **Silence detection (natural pauses)** — Whisper's audio encoder detects
   low-energy regions in the mel spectrogram. When the energy drops for long
   enough (a natural breath / sentence pause), the current segment is closed
   and a new one begins. This is the primary signal — segments align to where
   the speaker actually paused.

2. **Language-model sentence boundaries** — as Whisper decodes the text
   token-by-token, its language model predicts sentence-ending punctuation
   (`.`, `?`, `!`). When it emits one, the current segment is closed. This
   is why a segment's text almost always ends with terminal punctuation,
   even when the speaker didn't pause.

3. **Max segment length cap** — Whisper enforces a hard ceiling (default
   `max_segment_length ≈ 30 s`). If a speaker talks for > 30 s without
   pausing, the segment is force-closed at the cap. This is the only
   "non-natural" boundary source.

So **segment boundaries are natural** (speech pauses + predicted sentence
ends), **not random**. The randomness people associate with "weird
subtitles" comes from what happens *inside* each segment — see §2.4.

### 2.4 The interpolation fallback (what we actually run, inside a segment)

While the segment boundaries are natural, the **word-level timestamps
inside each segment** are fabricated by our fallback code:

Whisper gives a **segment-level** timestamp `[seg_start, seg_end]` and the
segment's text. The fallback **divides that span evenly** across the words
in the segment. Each word gets the same duration, regardless of its real
spoken length. `score = 0.0` marks these as low-confidence.

### 2.5 Chunking for long audio

For audio ≥ 60 s, the aligner splits the audio into **~1-minute chunks**
(max 100 chunks), aligns each independently, and offsets the timestamps
back to the original timeline. A checkpoint (`.align.json`) is written
after every chunk so interrupted runs can resume.

> **Note on "chunks" vs "segments":** these are two different things.
> **Segments** (§2.3) are Whisper-internal, based on natural pauses. **Chunks**
> (this section) are our own pre-processing cuts of the audio file, made
> purely to keep each Whisper call short and resumable. A chunk contains
> many segments.

```
60-min video → 60 chunks → 60 Whisper calls → concatenate word lists
```

This is why you'll see log lines like:
```
Processing audio with duration 01:00.133   ← one chunk
WordAligner: aligned 5748 words in 45 chunks (2706.0s audio)
```

### 2.6 Config used by the batch run

```python
WHISPER_MODEL   = "tiny"     # small model → less accurate transcription
WHISPER_LANG    = "en"
WHISPER_DEVICE  = "cpu"
WHISPER_COMPUTE = "int8"     # fastest on CPU, lowest accuracy
```

The `tiny` model + `int8` + `interpolation` backend is the fastest but
least accurate combination. Transcription errors and timestamp errors
both flow downstream into the subtitles.

---

## 3. Stage 2 — Sentence Grouping (`cue_builder.py`)

### 3.1 Rule: a sentence ends when a word ends with `.`, `?`, or `!`

```python
_SENTENCE_END = re.compile(r"[.!?]\s*$")
```

The builder walks the word stream, accumulating words into the current
sentence. The moment a word's text matches `[.!?]` at the end, the
sentence is **flushed** — its `start` is the first word's start, its `end`
is the last word's end, and its `text` is the space-joined words.

```
Words:  ["So", "this", "is", "Kafka.", "It", "is", "a", "distributor."]
            └──── sentence 1 ────┘     └──── sentence 2 ────────┘
```

### 3.2 Rule: tiny trailing cues are merged back

If a flushed cue has **fewer than 3 words** (`_MIN_CUE_WORDS = 3`), it is
merged into the previous cue. This handles the common Whisper hallucination
where a period is inserted mid-sentence:

```
Whisper output:  "block."  "of"  "code"        ← "block." looks like a sentence end
Without merge:   cue1="...block."   cue2="of code"   ← "of code" is a 2-word fragment
With merge:      cue1="...block of code"              ← single natural sentence
```

When merging, the **trailing punctuation is stripped** from the previous
cue's last word (the period was likely a hallucination), and the merged
cue's `end` extends to the fragment's `end`.

### 3.3 Inter-sentence pause

After all cues are built, each cue's `pause_after` is computed as the
silence between its `end` and the next cue's `start`:

```python
cues[i].pause_after = max(0.0, cues[i+1].start - cues[i].end)
```

This is used by the dubbing pipeline; subtitle generation mostly ignores
it (it extends the *last* segment of a split cue into this gap — see §4.4).

---

## 4. Stage 3 — Subtitle Splitting (`subtitle_writer.py`)

This is the stage that decides **how many SRT entries a sentence becomes**
and **where the split falls**. It is governed by `split_cues_for_subtitles`,
called from `write_srt(cues, path, max_words_per_segment=10)`.

### 4.1 The two limits (hybrid word + character)

Two independent limits must **both** be exceeded for a cue to be split:

| Limit                     | Constant               | Value   |
|---------------------------|------------------------|---------|
| Max words per segment     | `max_words_per_segment`| `10` (batch) |
| Max characters per entry  | `MAX_CHARS_PER_ENTRY`  | `84`    |

A cue is split **only if**:
```python
len(words) > max_words_per_segment  AND  _count_chars(words) > MAX_CHARS_PER_ENTRY
```

> **Why 84?** Netflix Timed Text Style Guide: 42 chars/line × 2 lines = 84.
> A subtitle that fits in 84 chars will display on two lines without
> overflow on virtually every player.

**Consequence:** A 12-word sentence that is only 70 characters long
(e.g. "go to the store and buy some milk for me please now") is **not
split**, even though it exceeds the 10-word limit. This is intentional —
splitting it would create a 2-word fragment.

### 4.2 The split-point search (`_find_split_index`)

When a cue *does* need splitting, the algorithm searches for the **best
break point** near the midpoint:

1. **Soft break preferred** — a word ending with `, ; : — -`. If found, the
   punctuation word stays in the **first** segment (`split_at = i + 1`).
2. **Conjunction fallback** — a word in `_CONJUNCTIONS`:
   `{and, but, or, so, because, when, while, if, though, although,
   whereas, as}`. The conjunction goes in the **second** segment
   (`split_at = i`).
3. **Hard midpoint** — if neither is found, split at `min(max_words, len)`.

Among all candidates in `[1, max_words]`, the one **closest to the
midpoint** (`max_words // 2`) wins.

```python
mid = max_words // 2  # = 5 when max_words = 10
for i in range(1, upper):
    if _SOFT_BREAK.search(words[i].text) or words[i].text.lower() in _CONJUNCTIONS:
        split_at = i + 1 if _SOFT_BREAK.search(words[i].text) else i
        if abs(split_at - mid) < best_dist:
            best = split_at
```

### 4.3 Iterative splitting

Splitting repeats until the remainder fits:

```python
while len(remaining) > max_words and _count_chars(remaining) > MAX_CHARS:
    split_at = _find_split_index(remaining, max_words)
    emit(remaining[:split_at])
    remaining = remaining[split_at:]
emit(remaining)   # last segment
```

So a 30-word, 200-char cue with `max_words=10` becomes **3 segments**
(~10 words each), each split at the best local break point.

### 4.4 Last-segment tail extension

The **last** segment of each original cue gets its `end` extended forward
into the silence before the next cue, so it doesn't vanish the instant
speech ends:

```python
def _compute_tail_end(cue_end, next_start, max_extend=2.5):
    gap = next_start - cue_end
    if gap <= 0: return cue_end
    return cue_end + min(gap - 0.1, max_extend)
```

Rules:
- Extend by at most `gap - 0.1s` (leave a 100 ms gap so consecutive
  subtitles don't visually overlap).
- Never extend more than `2.5 s` total.
- If the next cue starts immediately (`gap <= 0`), no extension.

### 4.5 Tiny-fragment merge (post-split)

After splitting, any segment with **< 3 words OR < 15 chars** is merged
back into the previous segment. This cleans up fragments like `"whole."`
or `"of code."` that would otherwise appear alone on screen.

```python
def _merge_tiny_fragments(segments):
    for seg in segments:
        if (len(seg.words) < 3 or len(seg.text) < 15) and merged:
            merge_into_previous(seg)
```

### 4.6 Minimum duration clamp

When writing the final SRT, every entry's `end` is clamped to be at least
`start + 0.1s`, because some SRT players drop zero-length or near-zero
entries:

```python
end = max(cue.end, start + 0.1)
```

---

## 5. Worked Example

Take a long run-on cue produced by Stage 2:

```
Cue: "So when you call the consume method on the consumer,
      it's going to go ahead and fetch the messages from the broker,
      and then it returns them back to you as a list of records."
words: 28, chars: 165  → both limits exceeded, must split
```

### Split pass 1 (remaining = all 28 words)

- `max_words = 10`, `mid = 5`
- Scan words 1..9 for a soft break:
  - word 6 = `"method,"` → ends with `,` → soft break candidate, `split_at = 7`
  - word 14 = `"broker,"` → but 14 > 9, out of search window
- Best = 7 (closest to mid=5 among candidates in window)
- **Emit segment 1:** `"So when you call the consume method,"` (7 words)
- `remaining` = 21 words

### Split pass 2 (remaining = 21 words)

- Scan words 1..9:
  - word 7 = `"broker,"` → soft break, `split_at = 8`
- **Emit segment 2:** `"it's going to go ahead and fetch the messages from the broker,"` (8 words)
- `remaining` = 13 words

### Split pass 3 (remaining = 13 words)

- Scan words 1..9:
  - word 7 = `"you,"`? No — wait, `"you as a list of records."` has no comma.
  - No soft break, no conjunction in window → hard midpoint = 10
- **Emit segment 3:** `"and then it returns them back to you as a list"` (10 words)
- `remaining` = 3 words: `"of records."`

### Post-split merge

- `"of records."` → 3 words, 11 chars → **< 15 chars → tiny!**
- Merge into segment 3.
- **Final segment 3:** `"and then it returns them back to you as a list of records."`

### Resulting SRT

```
1
00:00:01,200 --> 00:00:03,800
So when you call the consume method,

2
00:00:03,800 --> 00:00:06,500
it's going to go ahead and fetch the messages from the broker,

3
00:00:06,500 --> 00:00:10,200
and then it returns them back to you as a list of records.
```

---

## 6. Why Some Subtitles Feel "Weird"

This section catalogs the common failure modes — the actual reasons a
specific subtitle looks off — and points at the code that produces them.

### 6.1 Evenly-spaced word timestamps (interpolation backend)

**Symptom:** Words appear at a metronomic, evenly-spaced cadence that
doesn't match the actual speech rhythm. A long word like
`"infrastructure"` gets the same on-screen duration as `"a"`.

**Cause:** `_segment_to_words_uniform` in
[word_aligner.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py)
divides each Whisper segment's span evenly across its words. This is the
fallback backend, used because `whisperx` and `stable_ts` aren't installed.

**Fix:** Install `whisperx` (requires `torch`, `torchaudio`, and a
wav2vec2 model download) or `stable-whisper`. The aligner will auto-select
them on the next run.

### 6.2 Whisper mis-segmentation → wrong sentence boundaries

**Symptom:** A sentence is cut in half, or two sentences are merged into
one giant cue.

**Cause:** Stage 2 splits strictly on `[.!?]` at the end of a word. If
Whisper hallucinates a period mid-sentence (`"block."` before `"of code"`),
the builder creates a tiny fragment. The `<3 words` merge rule catches
some of these, but not all — e.g. a 4-word fragment like `"of the new code."`
survives as its own cue.

**Fix:** Lower `_MIN_CUE_WORDS` in
[cue_builder.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py),
or add more aggressive punctuation cleanup (strip periods from
non-sentence-final words).

### 6.3 `tiny` model transcription errors

**Symptom:** A subtitle has a garbled or wrong word, or a technical term
is misrecognized (e.g. `"KRaft"` → `"craft"`, `"broker"` → `"Brookeer"`).

**Cause:** `WHISPER_MODEL = "tiny"` in the batch config. The tiny model
is ~39× realtime but has the highest word error rate. Errors in the
transcript propagate verbatim into the SRT.

**Fix:** Set `WHISPER_MODEL = "base"` or `"small"`. ~2-3× slower, fewer
transcription errors. This alone fixes most "weird word" subtitles.

### 6.4 Chunk-boundary artifacts

**Symptom:** Around multiples of ~60 seconds, a sentence is split across
two chunks and the timing jumps.

**Cause:** `_align_chunked` in
[word_aligner.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py)
cuts the audio into ~1-minute chunks and aligns each independently. A
sentence straddling a chunk boundary is transcribed twice (once per chunk)
and the two halves are concatenated. Whisper may transcribe each half
differently.

**Fix:** Use a single-pass aligner (`whisperx` or `stable_ts`), which
handle long audio internally without chunking. Or increase the chunk size
and overlap chunks by a few seconds.

### 6.5 Split falls at an awkward word

**Symptom:** A subtitle entry ends on a word like `"the"` or `"of"`,
leaving a fragment on the next line.

**Cause:** The split-point search in `_find_split_index` looks for soft
breaks (`[,;:—-]`) and conjunctions within `[1, max_words]`. If a sentence
has **no punctuation at all** in that window, it falls back to the hard
midpoint, which can land between any two words.

**Fix:** Add a list of "never end on" words (articles, prepositions) and
nudge the split point forward past them. Not currently implemented.

### 6.6 Subtitle disappears too fast / too slow

**Symptom:** A subtitle vanishes the moment speech ends, or lingers too
long into the next sentence.

**Cause:** The last segment of each cue gets its `end` extended by
`_compute_tail_end` (up to 2.5 s into the gap before the next cue).
Non-last segments end exactly when their last word ends — no extension.
So a split cue's first 2 segments can feel "cut short" while the 3rd feels
"lingering".

**Fix:** Apply a smaller tail extension (e.g. 0.3 s) to non-last segments
too. Currently non-last segments get `end = seg_words[-1].end` exactly.

### 6.7 Short sentence not split despite being long on screen

**Symptom:** A 9-word sentence stays on screen for 8 seconds (slow speech),
filling the screen with a single line.

**Cause:** The split gate is `len(words) > 10 AND chars > 84`. A 9-word,
70-char cue passes both checks → not split, even if it's on screen for a
long time. The algorithm is **length-based, not duration-based**.

**Fix:** Add a duration check (e.g. split if `end - start > 7s` regardless
of word count). Not currently implemented.

---

## 7. Configuration Reference

All tunable knobs, with their current batch-run values and where they
live:

| Knob                       | Value  | File                                      | Effect                                                    |
|----------------------------|--------|-------------------------------------------|-----------------------------------------------------------|
| `WHISPER_MODEL`            | `tiny` | `batch_transform.py` / `run_batch_dub.py` | Whisper model size. Bigger = more accurate, slower.       |
| `WHISPER_COMPUTE`          | `int8` | same                                      | Quantization. `int8` fastest, `int8_float16` more accurate.|
| `max_words_per_segment`    | `10`   | `batch_transform.py` (call site)         | Soft cap on words per SRT entry.                           |
| `MAX_CHARS_PER_ENTRY`      | `84`   | `subtitle_writer.py`                      | Hard cap on chars per SRT entry (Netflix standard).       |
| `_MIN_CUE_WORDS`           | `3`    | `cue_builder.py`                          | Cues with fewer words are merged into the previous.       |
| `max_tail_extend`          | `2.5s` | `subtitle_writer.py` (`split_cues_for_subtitles`) | Max time to extend a cue's last segment into the gap. |
| `_CONJUNCTIONS`            | set    | `subtitle_writer.py`                      | Words that trigger a split (conjunction goes to 2nd seg). |
| `_SOFT_BREAK` regex        | `[,;:—\-]$` | `subtitle_writer.py`                | Trailing punctuation that triggers a split (stays in 1st).|
| `group_size` (word SRT)    | `1`    | `batch_transform.py` (call site)          | Words per entry in the `.words.srt` file.                 |
<<<<<<< HEAD
=======
| `aligner.last_segments`   | auto  | `word_aligner.py`                         | Whisper's raw segments, written to `.segments.srt` via `write_segments_srt`. |
>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b

---

## 8. File Reference

| File | Role |
|------|------|
<<<<<<< HEAD
| [word_aligner.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py) | Stage 1: audio → `List[Word]` |
| [cue_builder.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py) | Stage 2: `List[Word]` → `List[AlignedCue]` (sentences) |
| [subtitle_writer.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py) | Stage 3: `List[AlignedCue]` → SRT (split + format) |
=======
| [word_aligner.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py) | Stage 1: audio → `List[Word]` + `self.last_segments` (Whisper segments preserved) |
| [cue_builder.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py) | Stage 2: `List[Word]` → `List[AlignedCue]` (sentences) |
| [subtitle_writer.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py) | Stage 3: `List[AlignedCue]` → SRT (`write_srt`, `write_word_srt`, `write_segments_srt`) |
>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b
| [models.py](file:///workspace/services/EnglishLearning/backend/app/services/sync/models.py) | `Word`, `Chunk`, `AlignedCue` dataclasses |
| [run_batch_dub.py](file:///workspace/services/EnglishLearning/backend/run_batch_dub.py) | Batch runner (dubbing + subtitles) |
| `batch_transform.py` | Batch runner (watermark + subtitles only) |

---

## 9. Summary of the Split Decision

In one sentence: **a sentence becomes one SRT entry unless it has more
than 10 words AND more than 84 characters, in which case it is split at
the nearest comma/conjunction to the midpoint, repeatedly, and any
resulting fragment shorter than 3 words or 15 chars is glued back onto
the previous segment.**

<<<<<<< HEAD
=======
Three subtitle files are produced per video:
- **`.srt`** — rebuilt + split sentences (for reading)
- **`.words.srt`** — one entry per word (for karaoke highlighting)
- **`.segments.srt`** — Whisper's raw segments, preserved unaltered (for
  diagnosis and direct use when the rebuilt cues feel worse)

>>>>>>> fc51b1473158018d18ca892e5e63a5e03b554e2b
The single most impactful improvement for subtitle quality would be
**upgrading from the `tiny` + `interpolation` backend to `base` +
`whisperx`** — this fixes both transcription errors (§6.3) and timestamp
jitter (§6.1) at the source, and every downstream stage benefits.
