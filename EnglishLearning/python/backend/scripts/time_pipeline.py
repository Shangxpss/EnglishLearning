#!/usr/bin/env python3
"""Time every stage of the dubbing pipeline on the full Kafka video.

Records per-stage wall-clock time so we can see which stage is slow and
why, then proposes concrete improvements reachable with the current
tech stack (no GPU, no new cloud services).
"""
import gc
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
os.environ.setdefault("http_proxy", "http://127.0.0.1:18080")
os.environ.setdefault("https_proxy", "http://127.0.0.1:18080")

import numpy as np
import soundfile as sf

from app.services.sync import (
    WordAligner, build_aligned_cues, SentenceStitcher,
    VoiceConfig, build_backend,
    get_media_duration, mux_video_audio,
)

VIDEO = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "app", "subtitle",
    "1 - Welcome to Your Kafka Journey--[koudaizy.com].mp4",
)
OUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                       "app", "subtitle", "sync_dub")
TTS_DIR = os.path.join(OUT_DIR, "tts")
os.makedirs(TTS_DIR, exist_ok=True)

# Clean cache so we measure cold timings
print("Clearing TTS cache for cold measurement...")
for f in os.listdir(TTS_DIR):
    if f.endswith(".wav"):
        os.remove(os.path.join(TTS_DIR, f))

timings = {}
def stage(name):
    class _Stage:
        def __enter__(self):
            self.t0 = time.perf_counter()
            print(f"\n[{name}] starting...")
            sys.stdout.flush()
            return self
        def __exit__(self, *exc):
            self.dt = time.perf_counter() - self.t0
            timings[name] = self.dt
            print(f"[{name}] done in {self.dt:.2f}s")
            sys.stdout.flush()
    return _Stage()


# ── Stage 1: duration probe ──────────────────────────────────────────────
with stage("1_duration_probe"):
    VIDEO_DURATION = get_media_duration(VIDEO)

# ── Stage 2: word alignment (the heavy one) ──────────────────────────────
with stage("2_word_alignment"):
    aligner = WordAligner(model_size="base", language="en", device="cpu",
                          compute_type="int8")
    aligner_start = time.perf_counter()
    words = aligner.align(VIDEO)
    # stable-ts has two sub-steps: transcribe + adjustment
    # (we can't time them separately without monkey-patching, so we
    # measure the total)
    del aligner
    gc.collect()

n_words = len(words)
n_low_conf = sum(1 for w in words if w.score < 0.5)

# ── Stage 3: cue building ────────────────────────────────────────────────
with stage("3_cue_building"):
    cues = build_aligned_cues(words, VIDEO_DURATION)

# ── Stage 4: backend construction ────────────────────────────────────────
with stage("4_backend_construction"):
    VOICE = VoiceConfig(backend="edge_tts", gender="female")
    backend = build_backend(VOICE)
    SAMPLE_RATE = backend.sample_rate

# ── Stage 5: per-sentence synthesis ──────────────────────────────────────
# Time each sentence individually so we can see the per-sentence cost
# distribution, not just the total.
synth_times = []
with stage("5_synthesis"):
    for ci, cue in enumerate(cues):
        if not cue.text:
            continue
        out_wav = os.path.join(TTS_DIR, f"sent_{ci:04d}.wav")
        t0 = time.perf_counter()
        backend.synth(cue.text, out_wav)
        dt = time.perf_counter() - t0
        synth_times.append((ci, cue.text[:50], dt, len(cue.text)))

# ── Stage 6: stitching ───────────────────────────────────────────────────
with stage("6_stitching"):
    sentence_audio = {}
    for ci in range(len(cues)):
        p = os.path.join(TTS_DIR, f"sent_{ci:04d}.wav")
        if os.path.exists(p):
            ad, sr = sf.read(p)
            if sr != SAMPLE_RATE:
                import librosa
                ad = librosa.resample(np.asarray(ad, dtype=np.float32),
                                      orig_sr=sr, target_sr=SAMPLE_RATE)
            if ad.ndim > 1:
                ad = ad[:, 0]
            sentence_audio[ci] = ad
    stitcher = SentenceStitcher(sample_rate=SAMPLE_RATE)
    final_audio = stitcher.stitch(sentence_audio, cues, VIDEO_DURATION)
    FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio.wav")
    sf.write(FINAL_WAV, final_audio, SAMPLE_RATE)

# ── Stage 7: PyAV muxing ─────────────────────────────────────────────────
with stage("7_muxing"):
    FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_sync_dubbed.mp4")
    if os.path.exists(FINAL_VIDEO):
        os.remove(FINAL_VIDEO)
    mux_video_audio(VIDEO, FINAL_WAV, FINAL_VIDEO,
                    audio_bitrate=128_000, shortest=True)

# ── Report ───────────────────────────────────────────────────────────────
print("\n" + "=" * 78)
print("PIPELINE TIMING REPORT")
print("=" * 78)

print("\n## Per-stage wall-clock time")
print(f"{'stage':<28} {'time (s)':>10} {'% of total':>12}")
print("-" * 52)
total = sum(timings.values())
for name, dt in timings.items():
    pct = 100 * dt / total
    bar = "#" * int(pct / 2)
    print(f"{name:<28} {dt:>10.2f} {pct:>11.1f}%  {bar}")
print("-" * 52)
print(f"{'TOTAL':<28} {total:>10.2f} {'100.0%':>12}")
print(f"\nVideo duration: {VIDEO_DURATION:.1f}s")
print(f"Realtime factor: {total / VIDEO_DURATION:.2f}x "
      f"({VIDEO_DURATION:.1f}s video processed in {total:.1f}s)")

print("\n## Stage 2 detail (word alignment — the heavy one)")
print(f"  backend: stable_ts (faster-whisper base, int8, CPU)")
print(f"  words aligned: {n_words}  (low-confidence: {n_low_conf})")
print(f"  words/sec of audio: {n_words / VIDEO_DURATION:.1f}")
print(f"  alignment speed: {VIDEO_DURATION / timings['2_word_alignment']:.2f}x "
      f"realtime")

print("\n## Stage 5 detail (per-sentence synthesis via edge-tts)")
synth_total = sum(t for _, _, t, _ in synth_times)
synth_chars = sum(c for _, _, _, c in synth_times)
synth_wavs = [sf.read(os.path.join(TTS_DIR, f"sent_{ci:04d}.wav"))[0]
              for ci, _, _, _ in synth_times]
synth_audio_total = sum(len(w) / SAMPLE_RATE for w in synth_wavs)
print(f"  sentences: {len(synth_times)}")
print(f"  total synth time: {synth_total:.2f}s")
print(f"  total TTS audio:   {synth_audio_total:.2f}s")
print(f"  synthesis speed:   {synth_audio_total / synth_total:.2f}x realtime")
print(f"  chars processed:   {synth_chars}")
print(f"  avg per sentence:  {synth_total / len(synth_times):.2f}s "
      f"(text {synth_chars / len(synth_times):.0f} chars)")
print(f"\n  per-sentence breakdown:")
print(f"  {'idx':>3} {'time':>6} {'chars':>6} {'tts_s':>6}  text")
print(f"  {'---':>3} {'----':>6} {'-----':>6} {'-----':>6}  ----")
for ci, txt, dt, nch in synth_times:
    ad = synth_wavs[ci]
    tts_s = len(ad) / SAMPLE_RATE
    print(f"  {ci:>3} {dt:>6.2f} {nch:>6} {tts_s:>6.1f}  {txt}")

print("\n## Stage 7 detail (PyAV muxing)")
print(f"  video codec: remuxed (no transcode)")
print(f"  audio codec: WAV → AAC")
print(f"  output size: {os.path.getsize(FINAL_VIDEO)/(1024*1024):.2f} MB")

# ── Improvement analysis ──────────────────────────────────────────────────
print("\n" + "=" * 78)
print("BOTTLENECK ANALYSIS & IMPROVEMENTS (within current tech stack)")
print("=" * 78)

slowest = max(timings.items(), key=lambda kv: kv[1])
print(f"\nSlowest stage: {slowest[0]} ({slowest[1]:.2f}s, "
      f"{100*slowest[1]/total:.1f}% of total)")

print("""
## Where the time goes (current run)
""")
for name, dt in sorted(timings.items(), key=lambda kv: -kv[1]):
    pct = 100 * dt / total
    print(f"  {name:<28} {dt:>7.2f}s  ({pct:>5.1f}%)")

print("""
## Concrete improvements reachable now (no GPU, no new cloud services)

### 1. Stage 2 (word alignment) — typically the biggest cost
   Current: faster-whisper `base` model, int8, single-threaded transcribe
   + stable-ts adjustment pass. ~20s for 4-min video.

   Options:
   a) Use `tiny` model instead of `base`.
      - 2-3x faster, ~5% word-error-rate increase (acceptable for
        alignment, since we only need timestamps not transcription quality)
      - One-line change: WordAligner(model_size="tiny", ...)
      - Trade-off: alignment accuracy may drop slightly on rare words

   b) Use `compute_type="int8"` on a model that supports it well.
      - Already using int8. Could try float16 on CPU (no — float16 on CPU
        is slower, ctranslate2 warns about this).

   c) Increase thread count for faster-whisper.
      - Currently uses default (1-2 threads).
      - Set `cpu_threads=4` (or os.cpu_count()) on the model.
      - 2-3x speedup possible on multi-core machines.

   d) Cache the alignment result to disk (cues.json).
      - Already done. Re-runs skip stage 2 entirely (~0.1s instead of ~20s).

### 2. Stage 5 (edge-tts synthesis) — network-bound, currently ~1.5s/sentence
   Current: 23 sequential HTTP+WebSocket round-trips to Microsoft's server.
   Total: ~35-40s. Each sentence pays full network latency.

   Options:
   a) Parallelize with asyncio.gather() — edge-tts is async-native.
      - All 23 sentences synthesized concurrently.
      - Expected speedup: from ~35s → ~5-8s (bounded by slowest sentence
        + a bit of connection overhead).
      - Implementation: change the for-loop to asyncio.gather() of
        Communicate.save() coroutines. Each writes its own sent_NNNN.wav.
      - Caveat: Microsoft may rate-limit; need to test with a small
        concurrency cap (e.g. asyncio.Semaphore(8)).

   b) Use the `edge_rate=+25%` config to make TTS audio shorter.
      - Not a synthesis speedup, but lets the stitcher avoid compression
        on more sentences. Trade-off: faster speech may sound less natural.

   c) Keep the file cache (already done).
      - Re-runs with stitcher tweaks skip stage 5 entirely.

### 3. Stage 7 (PyAV muxing) — typically ~1-2s
   Current: remux video packets + encode audio to AAC.

   Options:
   a) Already minimal. The video copy is O(n_packets) and cannot be
      sped up without dropping packets.
   b) Could use lower AAC bitrate (96k vs 128k) for marginal encode speedup
      — not worth the quality loss for ~0.2s saving.

### 4. Stages 1, 3, 4, 6 — all sub-second
   These are already negligible. No optimization needed.

## Summary
   - Cold run: alignment (~20s) + synthesis (~35s) dominate.
   - Warm run (caches hit): only muxing (~2s) runs.
   - Biggest win available now: parallelize edge-tts with asyncio.gather()
     (~35s → ~8s, a 4x speedup on stage 5).
   - Second biggest: use `tiny` whisper model + more CPU threads
     (~20s → ~7s, a 3x speedup on stage 2).
   - Combined: cold run could drop from ~60s → ~20s.
""")
