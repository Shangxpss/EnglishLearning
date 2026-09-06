#!/usr/bin/env python3
"""Time stage 2 (word alignment) with different model sizes and thread counts.

Measures 4 configurations:
  1. base + default threads (current)
  2. base + 4 threads
  3. tiny + default threads
  4. tiny + 4 threads

Reports word count + accuracy so we can see the speed/accuracy trade-off.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")

from app.services.sync import WordAligner, get_media_duration

VIDEO = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "app", "subtitle",
    "1 - Welcome to Your Kafka Journey--[koudaizy.com].mp4",
)

DUR = get_media_duration(VIDEO)
print(f"Video: {VIDEO}")
print(f"Duration: {DUR:.1f}s")
print()

# Baseline words for accuracy comparison
print("Building baseline (base model, default threads)...")
t0 = time.perf_counter()
base_aligner = WordAligner(model_size="base", language="en",
                           device="cpu", compute_type="int8")
base_words = base_aligner.align(VIDEO)
base_t = time.perf_counter() - t0
del base_aligner
import gc; gc.collect()
print(f"  base: {len(base_words)} words in {base_t:.2f}s")
print()

configs = [
    ("tiny",  None),    # tiny model, default threads
    ("base",  4),       # base model, 4 threads
    ("tiny",  4),       # tiny model, 4 threads
]

print(f"{'config':<25} {'time':>8} {'words':>7} {'diff':>6} {'speedup':>9}")
print("-" * 60)
print(f"{'base (baseline)':<25} {base_t:>7.2f}s {len(base_words):>7} {'--':>6} {'1.00x':>9}")

base_set = [(w.text.lower(), round(w.start, 2), round(w.end, 2))
            for w in base_words]

for model_size, threads in configs:
    label = f"{model_size}, threads={threads or 'default'}"
    t0 = time.perf_counter()
    aligner = WordAligner(model_size=model_size, language="en",
                          device="cpu", compute_type="int8",
                          cpu_threads=threads)
    words = aligner.align(VIDEO)
    dt = time.perf_counter() - t0
    # Compare word set to baseline
    cur_set = [(w.text.lower(), round(w.start, 2), round(w.end, 2))
               for w in words]
    diff = abs(len(cur_set) - len(base_set))
    speedup = base_t / dt
    print(f"{label:<25} {dt:>7.2f}s {len(words):>7} {diff:>6} {speedup:>8.2f}x")
    del aligner
    gc.collect()
