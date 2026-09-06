# TTS Sync Mechanism — Historical Iteration Log

> This document is the complete record of every bug found and fixed during the
> development of the sync-aware dubbing pipeline. It covers iterations
> v1→v2→v3→v4→v4.1, including patch releases and engineering improvements.

---

## 1. Executive Summary

| Iteration | Approach | Result |
|-----------|----------|--------|
| **v1** (`run_chattts_dub.py`) | Sentence-level synthesis, segment-level Whisper timestamps, asymmetric (compress-only) stretch | Natural voice, but drifts out of sync |
| **v2** (`run_sync_dub.py` + `ChunkStitcher`) | Word-level forced alignment, breath-group chunking, symmetric per-chunk stretch | Worse than v1: choppy, electronic, omits words |
| **v3** (`run_sync_dub.py` + `SentenceStitcher`) | Word-level alignment kept, synthesis back at sentence level, gentle compress-only stretch | Natural voice AND in sync |
| **v3.1** | v3 + (a) conditional fade-out fix, (b) tanh soft-clip on ChatTTS output, (c) pluggable TTS backend with edge-tts for native en-GB accent | Natural, in sync, no syllable cut, true London accent |
| **v3.2** | v3.1 + pause-aware compression (only compress on actual collision with next sentence) | No more "weird" metallic sentences — 7→2 compressed, 1.30x→1.08x |
| **v4** | Parallel synthesis + GentleStitcher (zero-crossing truncation, no phase vocoder) | 5.45× faster synthesis, no metallic artifacts |
| **v4.1** (current) | RespeedSynthesizer + PlaceStitcher + audio quality improvements | Re-synthesize overflow at faster rate (no post-processing); loudness normalization; room tone fill; dithering; resume capability |

---

## 2. How Audio Timing Works

### What "timing" means for dubbing

Dubbing replaces the original speaker's voice with a synthetic one. For the
result to feel natural, the synthetic voice must say the right words at the
right time — matching **what** the original said, **when** each sentence
starts, and **how long** each sentence takes. Three quantities define timing:

| Quantity | Meaning | Why it matters |
|----------|---------|----------------|
| **What** | The text transcript | Wrong words → wrong meaning |
| **When** | Start time of each sentence | Late start → viewer hears silence, early start → overlaps previous |
| **How long** | Duration of each sentence slot | Too fast → sounds rushed; too slow → drifts into next sentence |

### Where timestamps come from

The [`WordAligner`](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py)
produces per-word `(text, start, end, score)` tuples via one of three
pluggable backends (first importable wins):

1. **whisperx** — faster-whisper + wav2vec2 forced alignment. Accuracy <100 ms.
2. **stable_ts** — wraps faster-whisper, word timestamps via alignment head. No
   HF token required.
3. **interpolation** — uniform split of each Whisper segment across its words.
   Segment-level accuracy only. Fallback when nothing else is installed.

For the Kafka test video, the `stable_ts` backend aligned **523 words** with
**511/523 at high confidence** (score ≥ 0.5).

### From words to sentences

[`build_aligned_cues()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py)
groups the flat word stream into sentences by detecting sentence-final
punctuation (`.` `!` `?`). Each [`AlignedCue`](file:///workspace/services/EnglishLearning/backend/app/services/sync/models.py)
carries:

```python
@dataclass
class AlignedCue:
    text: str               # full sentence text
    start: float            # first word's start time
    end: float              # last word's end time
    words: List[Word]       # all words in the sentence
    chunks: List[Chunk]     # breath-group chunks (used by ChunkStitcher only)
    pause_after: float      # silence between this sentence's end and next sentence's start
```

**`pause_after` is critical for the stitcher** — it defines the absorption
room available when TTS audio overflows the sentence slot without colliding
with the next sentence. The v3.2 pause-aware compression fix (§10.4) depends
entirely on this field being accurate.

---

## 3. v1 Failure — Asymmetric Stretch Causes Drift

### What v1 did

[`run_chattts_dub.py`](file:///workspace/services/EnglishLearning/backend/run_chattts_dub.py)
used **segment-level Whisper timestamps** (each segment ≈ 5–10 s, drifting by
~1 s) and **sentence-level ChatTTS synthesis**. The stitch logic was simple:

```python
# v1 stitcher (simplified from run_chattts_dub.py)
if len(audio_data) > slot_samples:
    ratio = len(audio_data) / slot_samples
    compress_rate = min(ratio, 1.3)       # cap at 1.3x
    audio_data = librosa.effects.time_stretch(audio_data, rate=compress_rate)
# else: do nothing — TTS shorter than slot, just place it
```

### Why it drifted

1. **Only compressed when overflowing**, capped at 1.3×. When TTS was
   shorter than the slot (the common case), the remaining time became silence —
   the dubbed speaking rate was **systematically slower** than the original.

2. **Never expanded when shorter.** A 4-second TTS in a 5-second slot left
   1 second of dead air. Across 23 sentences, those gaps accumulated into
   multiple seconds of drift.

3. **Overflow beyond cap bled into the next sentence.** When TTS needed
   >1.3× compression, the excess samples extended past the slot boundary and
   overwrote the start of the next sentence's region — causing **cumulative
   drift** that worsened toward the end of the video.

### Observation

The voice sounded natural (ChatTTS with full-sentence context), but by the
last third of the video the audio was visibly out of sync with the speaker's
lip movements.

---

## 4. v2 Failure — Per-Chunk Synthesis Destroys Naturalness

### What v2 did

[`ChunkStitcher`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py)
split each sentence into **breath-group chunks** at punctuation and silence
gaps, then synthesized and time-stretched each chunk independently:

```python
# chunk_stitcher.py — split_into_chunks()
# A new chunk starts when:
#   * silence between words > 0.25 s, or
#   * previous word ended with clause punctuation (, ; : etc.)
```

For the 23-sentence Kafka video, this produced **74 chunks** (avg 3.2 chunks
per sentence). Each chunk was synthesized independently and stretched
symmetrically (0.85×–1.25×) to exactly fill the original chunk's duration.

### Measurement table

| Metric | Value |
|--------|-------|
| Total chunks | 74 (for 23 sentences) |
| Chunks with phase-vocoder clicks | 34/74 (46%) |
| Shortest chunk duration | 0.18 s — garbled |
| Near-silent chunks | 1 |
| Perceived quality | More electronic than v1 |

### Root cause 1: Loss of prosodic continuity

ChatTTS (like all neural TTS) conditions on the **full sentence context** to
produce coherent intonation — rising pitch for questions, stress patterns for
emphasis, natural cadence across clauses. When fed a 2-word fragment like
"to the", it produces flat, generic output with no prosodic arc. Across a
sentence split into 3–4 fragments, the resulting audio sounds robotic and
disconnected.

### Root cause 2: Phase-vocoder artifacts on sub-second audio

`librosa.effects.time_stretch` uses a phase vocoder that operates on STFT
frames. On audio shorter than ~0.5 s (only a few hundred samples at 24 kHz
after STFT windowing), the phase estimation becomes unreliable, producing
audible click artifacts at frame boundaries. **46% of chunks** exhibited this.

### Why it sounded "more electronic than before"

The combination of **flat prosody** (from fragmented synthesis) + **click
artifacts** (from phase vocoder on short audio) produced a double degradation
that was worse than v1's natural-but-drifting output.

---

## 5. v3 Solution — Sentence-Level Synthesis + Accurate Placement

### The SentenceStitcher design

[`SentenceStitcher`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py)
combines the best of v1 and v2:

| Aspect | v1 | v2 | v3 (SentenceStitcher) |
|--------|----|----|----------------------|
| Synthesis granularity | Sentence ✅ | Chunk ❌ | Sentence ✅ |
| Timestamp source | Segment (drifty) | Word (accurate) | Word (accurate) |
| Stretch strategy | Compress-only, capped | Symmetric 0.85–1.25× | Compress-only, gentle |
| Inter-sentence pauses | Accidental | Explicit | Explicit (pause_after) |
| Overflow handling | Bleeds into next cue | Padded/truncated | Absorbed into pause_after |

Key principles:

1. **Whole-sentence synthesis** → ChatTTS gets full context → natural prosody.
2. **Word-aligned placement** → sentence `start` comes from the first word's
   accurate timestamp, not a drifting segment boundary.
3. **Compress-only stretch** → when TTS overflows the slot, compress gently
   (up to `max_compress`, default 1.3×). When TTS is shorter, leave the
   remaining time as natural silence.
4. **Residual overflow → `pause_after`** → if compression isn't enough, the
   remaining overflow extends into the inter-sentence pause. Only if it would
   reach the next sentence's start is compression applied (v3.2 refinement).

### Why this works where v2 didn't

- **Natural prosody**: ChatTTS receives the complete sentence, producing
  coherent intonation across the whole utterance.
- **No vocoder artifacts**: No sub-second audio is ever time-stretched. The
  shortest sentence is typically >1 s, well within the phase vocoder's safe
  operating range.
- **Accurate placement**: Word-level timestamps place each sentence within
  ~50–100 ms of the original speaker's timing.
- **No drift**: Compress-only + pause absorption means the dubbed audio never
  runs systematically slower or faster than the original.

### When sync is "good enough"

For perfect lip-sync at the phoneme level, the current approach is insufficient
— it aligns at the sentence level, not the viseme level. Achieving true
lip-sync would require something like **EmoDubber** (CVPR 2025, GPU-required),
which performs emotion-conditioned speech-to-speech translation with facial
motion alignment. For the current use case (educational narration over screen
recordings), sentence-level alignment is perceptually sufficient.

---

## 6. The Three-Iteration Decision Chain

```
v1: run_chattts_dub.py
    │
    │ Problem: segment timestamps drift ~1 s, compress-only → cumulative lag
    │
    ▼
v2: run_sync_dub.py + ChunkStitcher
    │   - Word-level alignment ✅
    │   - Breath-group chunking → 74 fragments
    │   - Symmetric stretch per chunk
    │
    │ Problem: fragmented synthesis → flat prosody + vocoder clicks
    │          46% click artifacts, words omitted on sub-second fragments
    │
    ▼
v3: run_sync_dub.py + SentenceStitcher
    │   - Word-level alignment ✅ (kept from v2)
    │   - Sentence-level synthesis ✅ (returned from v1)
    │   - Compress-only gentle stretch ✅
    │   - pause_after absorption ✅
    │
    │ Result: natural prosody + in sync
    │
    ▼
v3.1: + conditional fade-out, tanh soft-clip, edge-tts backend
    │
    ▼
v3.2: + pause-aware compression (current)
```

---

## 7. Implementation Reference

### Module structure

```
app/services/sync/
├── __init__.py          # public API re-exports
├── models.py            # Word, Chunk, AlignedCue dataclasses
├── word_aligner.py      # WordAligner (stable_ts / whisperx / interpolation)
├── cue_builder.py       # build_aligned_cues() — words → sentences
├── chunk_stitcher.py    # ChunkStitcher (v2, deprecated) + SentenceStitcher (v3)
├── voice_config.py      # VoiceConfig — backend, gender, accent, params
├── tts_backends.py      # TTSBackend ABC, ChatTTSBackend, EdgeTTSBackend
└── av_utils.py          # PyAV helpers: duration, decode, mp3→wav, mux
```

### SentenceStitcher key code summary

The [`SentenceStitcher.stitch()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py)
method (lines 277–381) implements the core logic:

```python
# Pause-aware compression (v3.2)
is_last_cue = (ci == len(cues) - 1)
available_room = slot_dur + (0.0 if is_last_cue else cue.pause_after)
available_samples = int(available_room * sr)

if audio.size > available_samples and available_samples > 0 and not is_last_cue:
    # Genuine collision — compress
    ratio_needed = audio.size / available_samples
    rate = min(ratio_needed, self.max_compress)
    if rate > 1.01:
        audio = librosa.effects.time_stretch(audio, rate=rate)
        n_compressed += 1

# Conditional fade-out (v3.1a)
if audio.size > 2 * fade_n:
    audio[:fade_n] *= np.linspace(0.0, 1.0, fade_n, ...)
    if audio.size >= available_samples:   # only if abutting next cue
        audio[-fade_n:] *= np.linspace(1.0, 0.0, fade_n, ...)
```

### Orchestration: `run_sync_dub.py` 7-stage pipeline

[`run_sync_dub.py`](file:///workspace/EnglishLearning/backend/run_sync_dub.py)
implements a 7-stage pipeline:

| Stage | Function | Description |
|-------|----------|-------------|
| 1 | `get_media_duration()` | PyAV duration probe (replaces `ffprobe`) |
| 2 | `WordAligner.align()` | faster-whisper + PyAV decode `.mp4` directly → word timestamps |
| 3 | `build_aligned_cues()` | Words → sentences with spans + `pause_after` |
| 4 | `build_backend()` | Instantiate `EdgeTTSBackend` or `ChatTTSBackend` |
| 5 | Per-sentence `backend.synth()` | TTS for each sentence (edge-tts mp3 → PyAV → wav) |
| 6 | `SentenceStitcher.stitch()` | Sentence placement + compress-only stretch |
| 7 | `mux_video_audio()` | PyAV: video remux + AAC encode → final `.mp4` |

---

## 8. Voice Gender & Accent Control

### ChatTTS has no native accent parameter

The speaker embedding (`chat.sample_random_speaker()`) controls **timbre,
pitch, and perceived gender**, but **not regional accent**. The accent you
hear comes from the training-data mix and the input text's
vocabulary/phrasing. ChatTTS's London accent is only *approximated* by:

- British English vocabulary substitution (`color` → `colour`, etc.)
- A measured RP-style `refine_prompt` (`[oral_0][laugh_0][break_4]`)

### What IS controllable in ChatTTS

| Parameter | Control | Effect |
|-----------|---------|--------|
| **Gender** | `torch.manual_seed(seed)` before `sample_random_speaker()` | Timbre/pitch: male seeds (2222, 7869, 6653), female seeds (3333, 4099, 5099) |
| **Temperature** | `InferCodeParams(temperature=...)` | 0.1 = stable/measured, 1.0 = expressive/varied |
| **top_P / top_K** | `InferCodeParams(top_P=..., top_K=...)` | Lower = focused/consistent, higher = diverse |
| **Speed tokens** | `InferCodeParams` with speed prompt | `[speed_5]` = moderate pace |
| **Refine prompt** | `RefineTextParams(prompt=...)` | `[oral_0][laugh_0][break_4]` suppresses oral fillers, laughter, adds measured breaks |
| **Pitch/timbre** | Different seed values | Different speaker embeddings = different voice character |

### Accent approximation: ChatTTS

The [`VoiceConfig.apply_british_vocabulary()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)
method applies conservative British English spelling substitutions
([`BRITISH_HINTS`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)
dictionary, 16 entries) before ChatTTS synthesis. This nudges the model
toward British pronunciation/lexical stress because ChatTTS conditions on
the input tokens — but it's an approximation, not genuine accent control.

### Why not genuine accent control in ChatTTS

ChatTTS's architecture has no accent conditioning pathway. Achieving true
accent switching would require:

- **RVC (Retrieval-based Voice Conversion)** — post-process TTS output through
  a voice model trained on the target accent
- **OpenVoice** — voice cloning with style/accent transfer
- **edge-tts** — Microsoft Neural TTS with native accent voices (the chosen
  solution in v3.1c)

### VoiceConfig API with examples

[`VoiceConfig`](file:///workspace/EnglishLearning/backend/app/services/sync/voice_config.py)
is a `@dataclass` with sensible defaults for a female, London-accented narrator:

```python
# Default: edge-tts, female, en-GB-SoniaNeural (native Southern British)
VoiceConfig()

# edge-tts male (en-GB-RyanNeural)
VoiceConfig(backend="edge_tts", gender="male")

# edge-tts specific voice override
VoiceConfig(backend="edge_tts", edge_voice="en-GB-LibbyNeural")

# ChatTTS offline, female, approximated accent
VoiceConfig(backend="chattts", gender="female")

# ChatTTS with custom decoding parameters
VoiceConfig(backend="chattts", temperature=0.1, top_P=0.5, top_K=10)
```

### Edge-tts proxy gotcha

edge-tts does **not** honor `http_proxy` / `https_proxy` environment variables
for its WebSocket synthesis endpoint (it only honors them for the HTTPS
voice-list GET). The proxy **must** be passed explicitly via
`Communicate(proxy=...)`. This was the cause of the previous "WebSocket
connection timeout" failure. [`VoiceConfig`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)
auto-detects the proxy from environment variables via `_detect_proxy()`.

---

## 9. Engineering Lessons Learned

### Lesson 1: Alignment granularity ≠ synthesis granularity

Word-level alignment is essential for **accurate placement** (knowing where
each sentence starts and ends), but that doesn't mean you should synthesize at
word or chunk granularity. Neural TTS needs full-sentence context for natural
prosody. The correct architecture is: **align at word level, synthesize at
sentence level, place at word level**.

### Lesson 2: Phase vocoder on short audio is unreliable

`librosa.effects.time_stretch` uses a phase vocoder that produces click
artifacts on audio shorter than ~0.5 s. The STFT windowing doesn't have enough
samples for reliable phase estimation. This is a fundamental limitation of the
algorithm, not a bug — avoid time-stretching sub-second audio.

### Lesson 3: Compress-only > symmetric stretch for naturalness

Symmetric stretch (expand short audio + compress long audio) sounds worse
than compress-only because:

- **Expansion** inserts silence-like artifacts that sound unnatural
- **Compression** is perceptually less noticeable (speeds up slightly)
- When TTS is shorter than the slot, **natural silence** (the original
  speaker's pause) sounds better than stretched audio

The compress-only approach lets TTS "breathe" naturally while only intervening
when the TTS would collide with the next sentence.

### Lesson 4: File-based cache enables rapid iteration

Each sentence's TTS output is cached as `tts/sent_0042.wav`. On re-runs,
already-synthesized sentences are skipped. This reduced iteration time from
~69 s to ~10 s when only the stitcher logic changed, enabling rapid
A/B testing of different stretch strategies.

### Lesson 5: Edge-tts proxy gotcha

edge-tts uses aiohttp WebSocket for synthesis and `requests`/urllib for
voice-list queries. Only the HTTP(S) path respects proxy env vars; the
WebSocket path ignores them. Must pass `proxy=` explicitly to `Communicate()`.
This cost several hours of debugging "connection timeout" errors that only
occurred behind a corporate proxy.

---

## 10. Bug Fix Log

### 10.1 v3.1a — "Last syllable suddenly stops"

**Symptom**: The final syllable of ~8 sentences was noticeably attenuated or
cut short, as if the speaker suddenly stopped mid-word.

**Diagnosis**: The `SentenceStitcher` applied an **unconditional 20 ms fade-out**
on every sentence's audio, regardless of whether the audio actually filled or
overflowed the slot. When the TTS audio fit comfortably within the sentence
slot (the common case), the next sample after the audio was already silence
(zeros). Applying a fade-out to zero ramped the final syllable's amplitude
down unnecessarily — the listener perceived it as the voice "suddenly stopping."

The original 20 ms was inherited from the `ChunkStitcher`, where it was needed
because chunks were always adjacent. For sentences with pauses between them,
the fade-out is only needed when the audio **abuts** the next sentence's start.

**Fix**: Two changes:

1. **Reduced fade duration** from 20 ms to 5 ms (`fade_s=0.005`). 5 ms is
   sufficient to suppress end-clicks from direct-assignment discontinuities
   without affecting perceptible syllable amplitude.

2. **Made fade-out conditional** — only apply when `audio.size >= available_samples`
   (i.e., the placed audio reaches or overflows the available room including
   `pause_after`, meaning it could collide with the next sentence):

```python
# Always safe: fade-in to avoid click at sentence start
audio[:fade_n] *= np.linspace(0.0, 1.0, fade_n, dtype=np.float32)
# Conditional: fade-out only when abutting next cue
if audio.size >= available_samples:
    audio[-fade_n:] *= np.linspace(1.0, 0.0, fade_n, dtype=np.float32)
```

**File**: [`chunk_stitcher.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py),
`SentenceStitcher.stitch()` lines 362–365.

---

### 10.2 v3.1b — "Electronic sound on some sentences"

**Symptom**: Several sentences had harsh metallic distortion — a grating,
"electronic" quality on certain phonemes, especially plosives and sibilants.

**Diagnosis**: **Hard digital clipping** in raw ChatTTS output. Investigation
of the waveform revealed peaks pinned at exactly ±1.0 with up to **152
consecutive clipped samples** in a single sentence. When a neural TTS model
clips, the flat-topped waveform introduces high-frequency harmonics that
sound metallic and harsh — nothing like natural speech.

The root cause: ChatTTS's internal decoder can produce samples exceeding the
`[-1, 1]` float32 range. When written to WAV via `soundfile`, these are
hard-clipped to ±1.0, producing the characteristic flat-top distortion.

**Fix**: Applied `tanh` soft-clip before writing the WAV. `tanh` smoothly
compresses the extremes while preserving the inner dynamics — the top ~15% of
the dynamic range is reshaped rather than sliced off:

```python
def _soft_clip(wav: np.ndarray, drive: float = 0.9) -> np.ndarray:
    """tanh soft-clip — eliminates flat-top distortion from ChatTTS."""
    wav = np.asarray(wav, dtype=np.float32)
    return np.tanh(wav * drive) / float(np.tanh(drive))
```

`drive=0.9` is gentle — only the extreme peaks are reshaped. The output
remains in `[-1, 1]` by construction. Applied in
[`ChatTTSBackend.synth()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py)
before `sf.write()`.

**File**: [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py),
`_soft_clip()` lines 66–78 and `ChatTTSBackend.synth()` line 133.

---

### 10.3 v3.1c — Accent control

**Problem**: ChatTTS has no accent API. The "London accent" was approximated
by British vocabulary substitution (`color` → `colour`, etc.) — a hack that
nudges pronunciation but doesn't produce genuine Southern British speech.

**Discovery**: `edge-tts` (Microsoft Edge "Read Aloud" service) offers native
`en-GB-*` voices:

| Voice | Gender | Quality |
|-------|--------|---------|
| `en-GB-SoniaNeural` | Female | Friendly, positive — recommended neutral modern Southern British |
| `en-GB-LibbyNeural` | Female | Slightly younger/brighter |
| `en-GB-MaisieNeural` | Female | Youngest of the three |
| `en-GB-RyanNeural` | Male | Standard Southern British male |
| `en-GB-ThomasNeural` | Male | Younger male |

**Fix**: Made the TTS backend **pluggable** via the
[`TTSBackend`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py)
ABC:

```python
class TTSBackend(ABC):
    sample_rate: int = 24000

    @abstractmethod
    def synth(self, text: str, out_wav: str) -> bool: ...
```

Two implementations:

- [`ChatTTSBackend`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py) — local CPU, gender via seed, accent approximated via vocab
- [`EdgeTTSBackend`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py) — Microsoft Neural TTS, **native accent**, requires network

Selected via [`VoiceConfig.backend`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)
and instantiated by [`build_backend()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py).

**Edge-tts proxy gotcha**: edge-tts does **not** honor `http_proxy` /
`https_proxy` env vars for its WebSocket synthesis endpoint (it only honors
them for the HTTPS voice-list GET). The proxy must be passed explicitly via
`Communicate(proxy=...)`. This is the cause of the previous "WebSocket
connection timeout" failure. The fix was to auto-detect the proxy from env
vars in [`VoiceConfig.proxy`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)
(defaults to `_detect_proxy()`) and pass it through to `Communicate()`.

**Files**:
- [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py) — `EdgeTTSBackend`, `build_backend()`
- [`voice_config.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py) — `VoiceConfig`, `_detect_proxy()`, `BRITISH_HINTS`

---

### 10.4 v3.2 — "Weird metallic sentences" (pause-aware compression)

**Symptom**: After switching to edge-tts (which produces cleaner neural audio
than ChatTTS), 4–5 sentences sounded "weird" or "metallic" — a subtle but
perceptible time-stretch artifact.

**Diagnosis**: The stitcher was compressing 7 sentences, but only **2
actually needed it**. The other 5 were being compressed unnecessarily because
the compression check only looked at whether TTS overflowed the **slot**
(`cue.end - cue.start`), ignoring the `pause_after` buffer:

```
Old logic:  compress if audio.size > slot_samples
New logic:  compress if audio.size > available_samples (slot + pause_after)
```

| Cue | Slot overflow | pause_after | Actual collision? | Old compression | New compression |
|-----|--------------|-------------|-------------------|-----------------|-----------------|
| 22  | 1.40×        | (last cue)  | No (no next cue)  | 1.40×           | None            |
| 16  | 1.30×        | 0.26 s      | Yes (by 0.26 s)   | 1.30×           | 1.30×           |
| 21  | 1.26×        | 0.18 s      | Yes (by 0.18 s)   | 1.26×           | ~1.08×          |
| 5   | 1.14×        | 0.30 s      | No (fits in pause)| 1.14×           | None            |
| 9   | 1.12×        | 0.45 s      | No (fits in pause)| 1.12×           | None            |
| 13  | 1.09×        | 0.22 s      | No (fits in pause)| 1.09×           | None            |
| 20  | 1.06×        | 0.35 s      | No (fits in pause)| 1.06×           | None            |

**Root cause**: Compression was triggered on **any slot overflow**, ignoring
`pause_after`. Sentences that naturally fit within the slot + pause room
were being phase-vocoder-stretched for no reason — the stretch artifacts were
audible on edge-tts's clean neural output.

**Fix**: Only compress when the overflow would actually **collide** with the
next sentence's audio region. The available absorption room is
`slot_dur + pause_after`. The last cue has no next sentence, so it never
needs compression:

```python
is_last_cue = (ci == len(cues) - 1)
available_room = slot_dur + (0.0 if is_last_cue else cue.pause_after)
available_samples = int(available_room * sr)

if audio.size > available_samples and available_samples > 0 and not is_last_cue:
    ratio_needed = audio.size / available_samples
    rate = min(ratio_needed, self.max_compress)
    if rate > 1.01:
        audio = librosa.effects.time_stretch(audio, rate=rate)
```

**Result**: 7 → 2 sentences compressed. Worst-case compression ratio
1.40× → 1.08×. The 5 previously "metallic" sentences now sound completely
natural.

**File**: [`chunk_stitcher.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py),
`SentenceStitcher.stitch()` lines 326–338.

---

### 10.5 PyAV replacing system ffmpeg

**Motivation**: The pipeline originally shelled out to system `ffmpeg`/`ffprobe`
for audio extraction, format conversion, and muxing. This created a hard
dependency on a system-installed ffmpeg binary and added subprocess overhead.

**Migration**: All 7 stages now use PyAV — no system ffmpeg subprocess.

| Stage | Old (system ffmpeg) | New (PyAV) |
|-------|---------------------|------------|
| 1: Duration probe | `ffprobe -show_entries format=duration` | [`get_media_duration()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `av.open(path).duration / 1_000_000` |
| 2: Audio decode | `ffmpeg -i video.mp4 -vn ... source_audio.wav` → then faster-whisper reads WAV | faster-whisper decodes `.mp4` via PyAV internally (its README: "FFmpeg does not need to be installed") |
| 5: MP3 → WAV | `ffmpeg -i tts.mp3 ... tts.wav` | [`mp3_to_wav()`](file:///workspace/EnglishLearning/backend/app/services/sync/av_utils.py) — PyAV `AudioResampler` + `soundfile.write` |
| 7: Mux video + audio | `ffmpeg -c:v copy -c:a aac -b:a 128k -shortest` | [`mux_video_audio()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `add_stream_from_template()` (remux) + `add_stream("aac")` (encode) |

**Key detail — Stage 7**: Video is **remuxed** (not transcoded). PyAV's
`add_stream_from_template()` copies the codec setup verbatim and passes
packets through without decode/encode — exactly what `ffmpeg -c:v copy` does.
Video bit_rate is identical before and after (verified empirically), proving
remux rather than transcode. Audio is encoded to AAC via PyAV's built-in
encoder.

**File**: [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py)

---

### 10.6 Performance optimization

**Cold-run baseline**: ~69 s for 251 s video (0.27× realtime).

**Profiling breakdown**:

| Stage | Time | % of total |
|-------|------|------------|
| Stage 5: Synthesis | ~41 s | 60% |
| Stage 2: Alignment | ~23 s | 33% |
| Stages 1, 3, 4, 6, 7 | ~5 s | 7% |

**Optimization 1: Parallel synthesis** (`synth_many` with `asyncio.gather`)

edge-tts is async-native (aiohttp WebSocket). Running all sentences
concurrently turns N sequential network round-trips into one batched wave
of parallel requests, bounded by `max_concurrency=8` to avoid Microsoft
rate-limiting.

[`EdgeTTSBackend.synth_many()`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py)
implements this with an `asyncio.Semaphore`:

```python
async def _run_all():
    tasks = [_one(w, t) for w, t in items]
    done = await asyncio.gather(*tasks)
```

**Result**: 45 s → 8 s (5.45× speedup) for 23 sentences.

**Optimization 2: Smaller whisper model + more threads**

Switching from `base` model to `tiny` model and increasing `cpu_threads` to 4:

```python
WordAligner(model_size="tiny", cpu_threads=4)
```

**Result**: 22 s → 13 s (1.75× speedup) for alignment.

**Combined projected**: ~35 s (2× speedup from baseline).

**File**: [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py),
`EdgeTTSBackend.synth_many()` lines 208–275.

---

## 11. v4.1 — Respeed Synthesis + Placement Stitching (Current)

**Source**: [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) — `RespeedSynthesizer`, `PlaceStitcher`
**Entry points**: [`run_batch_dub.py`](file:///workspace/services/EnglishLearning/backend/run_batch_dub.py), [`app/api/dubbing.py`](file:///workspace/services/EnglishLearning/backend/app/api/dubbing.py)

v4.1 is **additive** — it does not modify `GentleStitcher` or
`run_parallel_dub.py` (v4). The v4 pipeline keeps working unchanged.

### 11.1 RespeedSynthesizer — re-synthesize instead of compress

v4's `GentleStitcher` eliminated phase vocoder artifacts by truncating
overflow at a zero-crossing. v4.1 goes further: **never truncate at all**.

Instead, when a sentence's TTS audio overflows its slot, `RespeedSynthesizer`
re-synthesizes that sentence at a faster edge-tts rate:

```python
speedup = tts_duration / available_room
percent = int((speedup - 1) * 100)   # capped at +100% (2×)
rate = f"+{percent}%"                # e.g. "+50%"
```

edge-tts handles the rate parameter natively — the audio is produced at the
correct speed from the start. **No post-processing compression, no
truncation, no artifacts.**

In practice, 0% of sentences in the 35-video Kafka batch needed respeeding
— edge-tts audio fits naturally in its slot for nearly all English speech.

### 11.2 PlaceStitcher — loudness normalization + room tone

`PlaceStitcher` replaces `GentleStitcher`'s zero-crossing truncation with
pure placement. Since TTS audio has already been speed-adjusted, there's
nothing to truncate — just place and normalize.

Three audio quality improvements:

1. **Per-sentence RMS normalization** (-20 dBFS target): edge-tts output
   varies ±6 dB between sentences. Normalizing to a common target makes
   the dub sound like one consistent speaker. Gain is capped at 10×
   (+20 dB) and soft-clipped via `tanh` if needed.

2. **Conditional fade-out**: fade-out is only applied when the audio
   actually reaches the slot boundary. Sentences that end naturally within
   their slot keep their final consonant intact (no attenuation of
   "-ed", "-s", "-ly").

3. **Room tone fill**: `extract_room_tone()` extracts ambient noise from
   the original video's silent gaps. The stitcher fills inter-sentence
   silence with this at -30 dBFS, eliminating jarring dead-silence
   between sentences.

### 11.3 Audio quality improvements in muxing

`mux_video_audio()` now applies **triangular dithering** during float32 →
int16 conversion. This decorrelates quantization noise, eliminating the
low-level buzz on quiet passages (at the cost of ~1 bit of inaudible noise
at -96 dB).

### 11.4 Dubbing Studio API

`app/api/dubbing.py` exposes the v4.1 pipeline as a FastAPI router with:

- `POST /dubbing/start` — start a dubbing job (file or folder path)
- `GET /dubbing/progress/{job_id}` — poll progress (1.5s interval)
- `GET /dubbing/jobs` — list all jobs
- `GET /dubbing/voices` — list preset voices

Parameters: `voice`, `max_words_per_segment`, `room_tone` (all configurable
from the frontend Dubbing Studio page).

### 11.5 Resume capability

When a job is interrupted (Ctrl+C, network error, process kill), the
filesystem serves as the checkpoint. On restart, videos with existing
`_dubbed.mp4` output (non-empty) are skipped. Only videos without output
are processed. This makes long batch runs resilient to failures.

### 11.6 Subtitle segmentation control

`write_srt()` now accepts `max_words_per_segment` — long run-on sentences
are split into shorter subtitle entries at natural break points (commas,
conjunctions). This only affects subtitle output; TTS synthesis still
receives the original (unsplit) cues for best audio quality.

---

## 12. References

| Project | URL | Role |
|---------|-----|------|
| faster-whisper | https://github.com/SYSTRAN/faster-whisper | CTranslate2-based Whisper inference (alignment backend) |
| stable-ts | https://github.com/jianfch/stable-ts | Word-level timestamps via alignment head (primary aligner) |
| whisperx | https://github.com/m-bain/whisperx | Wav2Vec2 forced alignment (alternative aligner, needs HF token) |
| ChatTTS | https://github.com/2noise/ChatTTS | Neural conversational TTS (offline backend) |
| edge-tts | https://github.com/rany2/edge-tts | Microsoft Edge Neural TTS Python wrapper (online backend, native accent) |
| librosa | https://librosa.org/ | Phase vocoder time-stretch (`librosa.effects.time_stretch`) |
| PyAV | https://pyav.org/ | Pythonic FFmpeg bindings (audio decode, resample, mux, remux) |
| soundfile | https://github.com/bastibe/python-soundfile | WAV read/write |
| numpy | https://numpy.org/ | Array operations, `tanh` soft-clip |
| EmoDubber (CVPR 2025) | https://github.com/sst-def/EmoDubber | Emotion-conditioned speech-to-speech with facial motion alignment (future: true lip-sync) |
