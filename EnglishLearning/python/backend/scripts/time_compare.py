#!/usr/bin/env python3
"""Time both sequential and parallel synthesis, then compare.

Measures the same pipeline twice on the same video:
  Run A: stage 5 uses sequential synth() calls (the old path)
  Run B: stage 5 uses parallel synth_many() (the new path)

Reports the per-stage timings side-by-side so we can see the actual
improvement from parallelizing edge-tts.
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


def clear_tts_cache():
    for f in os.listdir(TTS_DIR):
        if f.endswith(".wav"):
            os.remove(os.path.join(TTS_DIR, f))


def time_stage(name):
    class _S:
        def __enter__(self):
            self.t0 = time.perf_counter()
            return self
        def __exit__(self, *exc):
            self.dt = time.perf_counter() - self.t0
            print(f"  {name:<32} {self.dt:>7.2f}s")
            timings[name] = self.dt
    timings = {}
    return _S(), timings


def run_pipeline(use_parallel: bool):
    """Run the full pipeline once, return per-stage timings dict."""
    timings = {}
    def stage(name):
        class _S:
            def __enter__(self):
                self.t0 = time.perf_counter()
                return self
            def __exit__(self, *exc):
                self.dt = time.perf_counter() - self.t0
                timings[name] = self.dt
                print(f"  {name:<32} {self.dt:>7.2f}s")
        return _S()

    print(f"\n{'='*60}")
    print(f"RUN: {'PARALLEL synth_many()' if use_parallel else 'SEQUENTIAL synth()'}")
    print(f"{'='*60}")

    with stage("1_duration_probe"):
        dur = get_media_duration(VIDEO)

    with stage("2_word_alignment"):
        aligner = WordAligner(model_size="base", language="en", device="cpu",
                              compute_type="int8")
        words = aligner.align(VIDEO)
        del aligner
        gc.collect()

    with stage("3_cue_building"):
        cues = build_aligned_cues(words, dur)

    with stage("4_backend_construction"):
        VOICE = VoiceConfig(backend="edge_tts", gender="female")
        backend = build_backend(VOICE)
        SAMPLE_RATE = backend.sample_rate

    clear_tts_cache()
    with stage("5_synthesis"):
        if use_parallel:
            items = [(os.path.join(TTS_DIR, f"sent_{ci:04d}.wav"), cue.text)
                     for ci, cue in enumerate(cues) if cue.text]
            results = backend.synth_many(items, max_concurrency=8)
            n_ok = sum(1 for ok in results.values() if ok)
            print(f"    synthesized {n_ok}/{len(items)} sentences (parallel)")
        else:
            n_ok = 0
            for ci, cue in enumerate(cues):
                if not cue.text:
                    continue
                out_wav = os.path.join(TTS_DIR, f"sent_{ci:04d}.wav")
                if backend.synth(cue.text, out_wav):
                    n_ok += 1
            print(f"    synthesized {n_ok}/{len(cues)} sentences (sequential)")

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
        final = stitcher.stitch(sentence_audio, cues, dur)
        FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio.wav")
        sf.write(FINAL_WAV, final, SAMPLE_RATE)

    with stage("7_muxing"):
        FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_sync_dubbed.mp4")
        if os.path.exists(FINAL_VIDEO):
            os.remove(FINAL_VIDEO)
        mux_video_audio(VIDEO, FINAL_WAV, FINAL_VIDEO,
                        audio_bitrate=128_000, shortest=True)

    timings["_total"] = sum(timings.values())
    timings["_video_dur"] = dur
    return timings


# ── Run both modes ────────────────────────────────────────────────────────
seq = run_pipeline(use_parallel=False)
par = run_pipeline(use_parallel=True)

# ── Comparison report ─────────────────────────────────────────────────────
print("\n" + "=" * 78)
print("COMPARISON: SEQUENTIAL vs PARALLEL synthesis")
print("=" * 78)
print(f"\nVideo duration: {seq['_video_dur']:.1f}s")
print()
print(f"{'stage':<32} {'sequential':>12} {'parallel':>12} {'speedup':>10}")
print("-" * 70)
for k in ["1_duration_probe", "2_word_alignment", "3_cue_building",
         "4_backend_construction", "5_synthesis", "6_stitching", "7_muxing"]:
    s = seq[k]
    p = par[k]
    speedup = s / p if p > 0 else float("inf")
    delta = p - s
    sign = "+" if delta >= 0 else ""
    print(f"{k:<32} {s:>10.2f}s  {p:>10.2f}s  {speedup:>7.2f}x  ({sign}{delta:+.2f}s)")
print("-" * 70)
seq_total = seq["_total"]
par_total = par["_total"]
speedup_total = seq_total / par_total
print(f"{'TOTAL':<32} {seq_total:>10.2f}s  {par_total:>10.2f}s  {speedup_total:>7.2f}x")
print(f"\nRealtime factor: {seq_total/seq['_video_dur']:.2f}x → "
      f"{par_total/par['_video_dur']:.2f}x")
