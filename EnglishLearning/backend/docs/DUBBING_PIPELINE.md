# Dubbing Pipeline — Technical Reference

> **Scope**: End-to-end technical reference for the sync-aware dubbing pipeline
> defined in `app/services/sync/`. Covers every stage from media probe to final
> mux, including design rationale, data models, and performance characteristics.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Repository Layout — File Map](#2-repository-layout--file-map)
3. [Stage 1 — Media Duration Probe](#3-stage-1--media-duration-probe)
4. [Stage 2 — Word-Level Alignment](#4-stage-2--word-level-alignment)
5. [Stage 3 — Cue Building](#5-stage-3--cue-building)
6. [Stage 4 — Subtitle Generation](#6-stage-4--subtitle-generation)
7. [Stage 5 — Respeed Synthesis](#7-stage-5--respeed-synthesis)
8. [Stage 6 — Placement Stitching](#8-stage-6--placement-stitching)
9. [Stage 7 — Muxing](#9-stage-7--muxing)
10. [Key Invariants](#10-key-invariants)
11. [Performance](#11-performance)
12. [Dependency Stack](#12-dependency-stack)
13. [VoiceConfig Reference](#13-voiceconfig-reference)
14. [Available en-GB Voices](#14-available-en-gb-voices)
15. [Iteration History](#15-iteration-history)

---

## 1. Overview

The pipeline replaces the original speaker's audio with a TTS-generated track
that runs at the same speed and rhythm as the original, with natural prosody
and preserved inter-sentence pauses.

Seven stages, invoked by `run_batch_dub.py` (CLI) or the Dubbing Studio API
(`app/api/dubbing.py`):

| Stage | Function                 | Source                                                                                                                                                                                                                                                    |
| ----- | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1     | Media duration probe     | [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `get_media_duration()`                                                                                                                                |
| 2     | Word-level alignment     | [`word_aligner.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py) — `WordAligner.align()`                                                                                                                         |
| 3     | Cue building             | [`cue_builder.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py) — `build_aligned_cues()`                                                                                                                          |
| 4     | Subtitle generation      | [`subtitle_writer.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py) — `write_srt()`, `write_word_srt()`, `split_cues_for_subtitles()`                                                                         |
| 5     | Respeed synthesis        | [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) — `RespeedSynthesizer` + [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py)          |
| 6     | Placement stitching      | [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) — `PlaceStitcher`                                                                                                                           |
| 7     | Muxing                   | [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `mux_video_audio()`                                                                                                                                   |

**Critical design principle**: No system `ffmpeg` binary is required at any
stage. All audio decoding, format conversion, and muxing is done via **PyAV**
(which bundles the FFmpeg libraries in its wheel). faster-whisper decodes `.mp4`
directly via PyAV — there is **no intermediate audio-extraction step**.

---

## 2. Repository Layout — File Map

```
backend/
├── run_batch_dub.py              # ★ PRODUCTION batch entry point (v4.1 respeed pipeline)
├── run_parallel_dub.py           # v4 entry point (parallel + gentle stitcher)
├── run_sync_dub.py               # v3 entry point (sequential + SentenceStitcher)
├── app/api/dubbing.py            # ★ Dubbing Studio API (FastAPI router + job runner)
│
├── scripts/                       # Historical + diagnostic scripts
│   ├── run_chattts_dub.py         # v1: ChatTTS, segment-level
│   ├── run_chatterbox_dub.py      # Chatterbox TTS (never worked)
│   ├── run_f5_dub.py              # F5-TTS v1
│   ├── run_f5_dub_v2.py           # F5-TTS v2
│   ├── run_stitch_v3.py           # F5-TTS v3
│   ├── test_chattts.py
│   ├── time_pipeline.py
│   ├── time_compare.py
│   └── time_aligner.py
│
├── docs/                          # Technical documentation
│   ├── DUBBING_PIPELINE.md        # This document
│   └── TTS_SYNC_MECHANISM.md      # Iteration history: v1→v2→v3→v4→v4.1
│
└── app/services/sync/             # ★ Production module
    ├── __init__.py                # Public API exports
    ├── models.py                  # Word, Chunk, AlignedCue dataclasses
    ├── word_aligner.py            # WordAligner (stable_ts / whisperx / interpolation)
    ├── cue_builder.py             # build_aligned_cues()
    ├── subtitle_writer.py         # write_srt(), write_word_srt(), split_cues_for_subtitles()
    ├── chunk_stitcher.py          # ChunkStitcher (v2, deprecated) + SentenceStitcher (v3)
    ├── parallel_synth.py          # ParallelSynthesizer, GentleStitcher, RespeedSynthesizer, PlaceStitcher
    ├── voice_config.py            # VoiceConfig
    ├── tts_backends.py            # EdgeTTSBackend + ChatTTSBackend + build_backend()
    └── av_utils.py               # PyAV utilities: duration, decode, mp3→wav, mux, extract_room_tone
```

### Production module (`app/services/sync/`)

| File                                                                                                          | Responsibility                                                | Key exports                                                                                                         |
| ------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| [`models.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/models.py)                 | Data structures passed between stages                         | `Word`, `Chunk`, `AlignedCue`                                                                                      |
| [`word_aligner.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py)     | Word-level forced alignment (auto-selects best backend)       | `WordAligner`                                                                                                      |
| [`cue_builder.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py)       | Groups words into sentences with pauses                       | `build_aligned_cues()`                                                                                             |
| [`subtitle_writer.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py) | SRT generation + segmentation control                         | `write_srt()`, `write_word_srt()`, `split_cues_for_subtitles()`                                                    |
| [`chunk_stitcher.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/chunk_stitcher.py) | v3 stitcher (phase vocoder compression)                      | `ChunkStitcher` (v2, deprecated), `SentenceStitcher` (v3)                                                          |
| [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) | v4/v4.1 synthesis + stitching                                | `ParallelSynthesizer`, `GentleStitcher`, `RespeedSynthesizer`, `PlaceStitcher`, `normalize_text()`, `validate_audio()` |
| [`voice_config.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)     | Voice configuration                                           | `VoiceConfig`, `FEMALE_SEEDS`, `MALE_SEEDS`, `BRITISH_HINTS`                                                       |
| [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py)     | TTS backend abstraction                                       | `EdgeTTSBackend`, `ChatTTSBackend`, `build_backend()`, `synth_many_with_rates()`                                   |
| [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py)             | PyAV-based audio/video utilities                              | `get_media_duration()`, `decode_audio()`, `mp3_to_wav()`, `mux_video_audio()`, `extract_room_tone()`              |

---

## 3. Stage 1 — Media Duration Probe

**Source**: [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `get_media_duration()`

Probes the video file's duration in seconds using **PyAV** — no system
`ffprobe` required. Reads the container-level duration first (reliable for
MP4/MKV/MP3), then falls back to deriving from the longest stream.

---

## 4. Stage 2 — Word-Level Alignment

**Source**: [`word_aligner.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/word_aligner.py) — `WordAligner`

Produces per-word timestamps (`start`, `end`, `score`) for the source audio,
accurate to ~50–100 ms. Three backends, tried in priority order:

| Backend                | Method                                     | HF token   |
| ---------------------- | ------------------------------------------ | ---------- |
| **stable_ts** (chosen) | Wraps faster-whisper + alignment           | Not needed |
| whisperx               | faster-whisper + wav2vec2 forced alignment | Required   |
| interpolation          | Uniform split of Whisper segments           | Not needed |

~60% of total pipeline time is spent in this stage (CPU-bound).

---

## 5. Stage 3 — Cue Building

**Source**: [`cue_builder.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/cue_builder.py) — `build_aligned_cues()`

Groups the flat word stream into complete sentences via punctuation-based
splitting, attaching each sentence's word span and the inter-sentence pause
(`pause_after`).

Output: `List[AlignedCue]` with `text`, `start`, `end`, `words`, `pause_after`.

---

## 6. Stage 4 — Subtitle Generation

**Source**: [`subtitle_writer.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/subtitle_writer.py)

Generates two SRT files:

1. **Sentence-level SRT** (`write_srt()`) — one entry per sentence (or per
   segment if `max_words_per_segment` > 0).
2. **Word-level SRT** (`write_word_srt()`) — one entry per word (karaoke-style).

### Subtitle segmentation control

Long run-on sentences produce subtitle entries that cover too much screen time.
`split_cues_for_subtitles(cues, max_words_per_segment)` breaks long cues into
shorter segments at natural break points (commas, conjunctions) near the
midpoint. This only affects subtitle output — TTS synthesis still receives the
original (unsplit) cues for best audio quality.

```python
# Write subtitles with max 10 words per entry
write_srt(cues, srt_path, max_words_per_segment=10)
```

---

## 7. Stage 5 — Respeed Synthesis

**Source**: [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) — `RespeedSynthesizer` + [`tts_backends.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/tts_backends.py) — `synth_many_with_rates()`

### The v4.1 approach: two-pass synthesis

Instead of synthesizing at normal speed and then compressing overflow in
post-processing (which caused metallic artifacts from the phase vocoder),
`RespeedSynthesizer` does:

**Pass 1** — Synthesize all sentences at normal rate (`+0%`) via async
`synth_many()` with `Semaphore(8)`.

**Pass 2** — For each sentence where TTS audio overflows its slot, compute
the needed speedup and re-synthesize at a faster edge-tts rate:

```python
speedup = tts_duration / available_room
percent = int((speedup - 1) * 100)   # capped at +100% (2×)
rate = f"+{percent}%"                # e.g. "+50%"
```

edge-tts handles the rate parameter natively — **no post-processing
compression needed, no metallic artifacts**.

### Why this works

- edge-tts rate parameter adjusts the speaking speed at the synthesis level,
  producing clean audio at any rate from -50% to +100%.
- In practice, 0% of sentences needed respeeding — edge-tts audio fits
  naturally in its slot for nearly all English speech.
- The network bottleneck is handled by async `synth_many()` — N sequential
  WebSocket round-trips become one batched wave (5.45× speedup measured).

---

## 8. Stage 6 — Placement Stitching

**Source**: [`parallel_synth.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/parallel_synth.py) — `PlaceStitcher`

The companion to `RespeedSynthesizer`. Since TTS audio has already been
speed-adjusted to fit its slot, there is no need for compression or
truncation. `PlaceStitcher` provides:

### Per-sentence loudness normalization

Each sentence is RMS-normalized to -20 dBFS (broadcast standard for speech)
**before** placement. This fixes the problem where some sentences sound
much quieter/louder than others (edge-tts output varies ±6 dB between
sentences).

```python
def _loudness_normalize(self, audio):
    rms = sqrt(mean(audio**2))
    gain = target_rms / rms       # capped at 10× (+20 dB)
    audio = audio * gain
    if peak > 0.99:               # soft-clip to prevent digital clipping
        audio = tanh(audio * 0.9) / tanh(0.9)
    return audio
```

### Conditional fade-out

Fade-out is only applied when the audio actually reaches the slot boundary.
Sentences that end naturally within their slot keep their final consonant
intact (no attenuation of "-ed", "-s", "-ly").

### Room tone fill

When `room_tone` audio is provided (extracted from the original video's
silent gaps via `extract_room_tone()`), inter-sentence silence is filled
with low-level ambient noise at -30 dBFS. This eliminates the jarring
"dead silence" between sentences that makes dubs sound choppy.

---

## 9. Stage 7 — Muxing

**Source**: [`av_utils.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/av_utils.py) — `mux_video_audio()`

Combines the original video stream (remuxed, no transcode) with the dubbed
audio track (AAC 128kbps) into a final `.mp4` file.

### Audio quality improvements

- **Triangular dithering** is applied during float32 → int16 conversion,
  decorrelating quantization noise (eliminates low-level buzz on quiet
  passages at the cost of ~1 bit of inaudible noise at -96 dB).
- **No system ffmpeg** — all muxing is via PyAV.

---

## 10. Key Invariants

| #   | Invariant                                                                              | Why it matters                                                                   |
| --- | -------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| 1   | **No system ffmpeg needed** — PyAV bundles all ffmpeg libraries                       | Zero external dependencies for deployment                                        |
| 2   | **No phase vocoder in production** — v4.1 uses respeed synthesis instead              | No metallic artifacts on clean neural TTS                                        |
| 3   | **Alignment granularity ≠ synthesis granularity**                                     | Word-level alignment for placement; sentence-level synthesis for prosody         |
| 4   **Loudness normalization per sentence** — RMS-based at -20 dBFS                      | Consistent perceived loudness across all sentences                               |
| 5   | **Subtitle segmentation is independent of TTS synthesis**                              | Long subtitles can be split without affecting audio quality                       |
| 6   | **Resume capability** — skips videos with existing `_dubbed.mp4` output               | Interrupted jobs pick up where they left off                                     |
| 7   | **Backend-agnostic stitching**                                                         | `PlaceStitcher` works with any TTS backend that produces `np.ndarray`            |

---

## 11. Performance

Measured on a 251-second video, CPU-only, with the edge-tts backend:

| Stage                   | Time (4-min video) | Bound   | Share |
| ----------------------- | ------------------ | ------- | ----- |
| 1. Duration probe       | <0.1s              | IO      | <1%   |
| 2. Word alignment       | ~20s               | **CPU** | ~60%  |
| 3. Cue building         | <0.1s              | CPU     | <1%   |
| 4. Subtitle generation  | <0.1s              | CPU     | <1%   |
| 5. Synthesis (parallel) | ~8s                | IO      | ~25%  |
| 6. Stitching            | <1s                | CPU     | ~3%   |
| 7. Muxing               | ~1-2s              | CPU     | ~10%  |

**Bottleneck**: Stage 2 (alignment) is CPU-bound. Stage 5 (synthesis) is
IO-bound (network) — solved by async `synth_many()`.

---

## 12. Dependency Stack

| Package            | Role                                                    |
| ------------------ | ------------------------------------------------------- |
| **av** (PyAV)      | Audio decoding + video remux + AAC encoding             |
| **edge-tts**       | Default TTS backend (Microsoft Neural TTS)              |
| **faster-whisper** | Whisper inference (CTranslate2 backend)                 |
| **stable-ts**      | Word-level alignment (wraps faster-whisper)             |
| **numpy**          | Array operations throughout pipeline                     |
| **librosa**        | Sample rate resampling (when TTS sr ≠ target sr)        |
| **soundfile**      | WAV read/write                                          |

> **NO system ffmpeg binary needed.**

---

## 13. VoiceConfig Reference

**Source**: [`voice_config.py`](file:///workspace/services/EnglishLearning/backend/app/services/sync/voice_config.py)

| Field                | Type            | Default                        | Applies to | Description                                                                                                                                      |
| -------------------- | --------------- | ------------------------------ | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `backend`            | `str`           | `"edge_tts"`                   | Both       | `"edge_tts"` (native accent) or `"chattts"` (offline)                                                                                           |
| `gender`             | `str`           | `"female"`                     | Both       | `"female"` or `"male"`. For edge_tts: picks voice pool. For chattts: picks seed pool.                                                            |
| `edge_voice`         | `Optional[str]` | `None`                         | edge_tts   | Explicit voice name e.g. `"en-GB-MaisieNeural"`                                                                                                 |
| `edge_rate`          | `str`           | `"+0%"`                        | edge_tts   | Speaking-rate adjustment. e.g. `"+50%"` for 1.5× speed                                                                                          |
| `edge_volume`        | `str`           | `"+0%"`                        | edge_tts   | Volume adjustment                                                                                                                                |
| `proxy`              | `Optional[str]` | `_detect_proxy()`              | edge_tts   | Proxy URL for edge-tts WebSocket                                                                                                                 |
| `british_vocabulary` | `bool`          | `True`                         | chattts    | Apply British vocabulary substitution. Ignored by edge_tts.                                                                                      |

---

## 14. Available en-GB Voices

### Female voices

| Voice                 | Character          | Notes                                   |
| --------------------- | ------------------ | --------------------------------------- |
| **en-GB-MaisieNeural** | Friendly, positive | **Current default** — youngest female   |
| en-GB-SoniaNeural     | Friendly, positive | Recommended neutral Southern British    |
| en-GB-LibbyNeural     | Friendly, positive | Slightly younger/brighter than Sonia    |

### Male voices

| Voice              | Character | Notes              |
| ------------------ | --------- | ------------------ |
| en-GB-RyanNeural   | —         | Default male voice |
| en-GB-ThomasNeural | —         | Alternative male   |

---

## 15. Iteration History

| Version | Stitcher             | Synthesis         | Overflow handling                  | Status     |
| ------- | -------------------- | ----------------- | ---------------------------------- | ---------- |
| v1      | Asymmetric stretch   | ChatTTS, segment  | Compress-only (cap 1.3×)           | Superseded |
| v2      | ChunkStitcher        | ChatTTS, chunk    | Symmetric per-chunk stretch        | Superseded |
| v3      | SentenceStitcher     | Sequential synth  | Phase vocoder compress-only        | Superseded |
| v4      | GentleStitcher       | Parallel synth    | Zero-crossing truncation           | Available  |
| **v4.1**| **PlaceStitcher**    | **RespeedSynth**  | **Re-synthesize at faster rate**   | **Current**|

See [`TTS_SYNC_MECHANISM.md`](file:///workspace/services/EnglishLearning/backend/docs/TTS_SYNC_MECHANISM.md) for the full iteration log.

---

_Document generated from source code in `app/services/sync/`. For implementation
details, refer to the individual module docstrings linked throughout._
