"""Test script for ChatTTS model loading and speech generation (CPU only)."""

import os
import sys
import time

# Ensure real-time output
os.environ["PYTHONUNBUFFERED"] = "1"

# Use HF mirror for model downloads
os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")

# Force CPU only
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")

import numpy as np
import soundfile as sf

import ChatTTS

TEXT = (
    "Hello everyone, I'm genuinely thrilled to be your guide on what I promise "
    "will be an exhilarating deep dive into Apache Kafka."
)

OUTPUT_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "app",
    "subtitle",
    "chattts_test.wav",
)


def main():
    print("=" * 60, flush=True)
    print("ChatTTS Test Script", flush=True)
    print("=" * 60, flush=True)

    print(f"[1/5] Importing ChatTTS ... OK (version={getattr(ChatTTS, '__version__', 'unknown')})", flush=True)

    # Initialize and load model from HuggingFace (uses HF_ENDPOINT mirror)
    print("[2/5] Initializing ChatTTS.Chat and loading model (source=huggingface, CPU) ...", flush=True)
    t0 = time.time()
    chat = ChatTTS.Chat()
    ok = chat.load(source="huggingface", compile=False, device="cpu")
    t1 = time.time()
    print(f"    load() returned: {ok} (took {t1 - t0:.1f}s)", flush=True)
    if not ok:
        print("ERROR: chat.load() returned False", flush=True)
        sys.exit(1)

    # Sample a random speaker
    print("[3/5] Sampling a random speaker ...", flush=True)
    rand_spk = chat.sample_random_speaker()
    print(f"    speaker emb (head): {rand_spk[:40]}...", flush=True)

    params_infer_code = ChatTTS.Chat.InferCodeParams(
        spk_emb=rand_spk,
        temperature=0.3,
    )
    params_refine_text = ChatTTS.Chat.RefineTextParams(
        temperature=0.7,
    )

    # Generate speech
    print("[4/5] Generating speech ...", flush=True)
    print(f"    text: {TEXT}", flush=True)
    t2 = time.time()
    wavs = chat.infer(
        TEXT,
        params_infer_code=params_infer_code,
        params_refine_text=params_refine_text,
    )
    t3 = time.time()
    print(f"    infer() took {t3 - t2:.1f}s", flush=True)
    print(f"    returned {len(wavs)} wav(s)", flush=True)

    # Save the first wav
    print("[5/5] Saving output ...", flush=True)
    os.makedirs(os.path.dirname(OUTPUT_PATH), exist_ok=True)
    audio = np.array(wavs[0], dtype=np.float32)
    # ChatTTS outputs at 24kHz
    sr = 24000
    sf.write(OUTPUT_PATH, audio, sr)
    print(f"    saved to: {OUTPUT_PATH}", flush=True)

    # Report stats
    samples = audio.shape[-1]
    duration = samples / sr
    size_bytes = os.path.getsize(OUTPUT_PATH)
    print("-" * 60, flush=True)
    print(f"Sample rate : {sr} Hz", flush=True)
    print(f"Samples      : {samples}", flush=True)
    print(f"Duration     : {duration:.2f} s", flush=True)
    print(f"File size    : {size_bytes} bytes ({size_bytes / 1024:.1f} KiB)", flush=True)
    print(f"Audio stats  : min={audio.min():.4f} max={audio.max():.4f} mean={audio.mean():.4f}", flush=True)
    print("=" * 60, flush=True)
    print("DONE", flush=True)


if __name__ == "__main__":
    main()
