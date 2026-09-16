#!/usr/bin/env python3 -u
"""Standalone F5-TTS dubbing script — avoids double Whisper loading.

Pre-transcribes the reference audio with the same Whisper model used for
the video, then passes ref_text to F5-TTS so it skips its internal
transcription step, saving ~500 MB of memory.
"""

import os, sys, gc, subprocess, json, tempfile, time, shutil
from pathlib import Path

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# ── paths ──────────────────────────────────────────────────────────────────
BACKEND = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VIDEO = os.path.join(BACKEND, "app", "subtitle",
                     "1 - Welcome to Your Kafka Journey--[koudaizy.com].mp4")
REF_VOICE = os.path.join(BACKEND, "app", "assets", "reference_voices",
                         "london_default.wav")
OUT_DIR = os.path.join(BACKEND, "app", "subtitle", "f5_dub")
os.makedirs(OUT_DIR, exist_ok=True)

print(f"Video       : {VIDEO}")
print(f"Reference   : {REF_VOICE}")
print(f"Output dir  : {OUT_DIR}")

# ── 1. Extract audio from video ─────────────────────────────────────────────
EXTRACTED_WAV = os.path.join(OUT_DIR, "source_audio.wav")
if not os.path.exists(EXTRACTED_WAV):
    print("\n[1/6] Extracting audio from video...")
    subprocess.run(
        ["ffmpeg", "-y", "-i", VIDEO, "-vn", "-ar", "16000", "-ac", "1",
         "-f", "wav", EXTRACTED_WAV],
        check=True, capture_output=True)
    print("  done")
else:
    print(f"\n[1/6] Audio already extracted: {EXTRACTED_WAV}")

# Get video duration
result = subprocess.run(
    ["ffprobe", "-v", "error", "-show_entries", "format=duration",
     "-of", "default=noprint_wrappers=1:nokey=1", VIDEO],
    capture_output=True, text=True)
VIDEO_DURATION = float(result.stdout.strip())
print(f"  Video duration: {VIDEO_DURATION:.1f}s")

# ── 2. Transcribe video audio with faster-whisper ───────────────────────────
print("\n[2/6] Transcribing video audio with faster-whisper...")
from faster_whisper import WhisperModel
whisper = WhisperModel("base", device="cpu", compute_type="int8")
segments, info = whisper.transcribe(EXTRACTED_WAV, beam_size=5, language="en")
cues = []
for seg in segments:
    cues.append({
        "start": seg.start,
        "end": seg.end,
        "text": seg.text.strip(),
    })
print(f"  Transcribed {len(cues)} segments (lang={info.language}, conf={info.language_probability:.2f})")

# ── 3. Transcribe reference audio (so F5-TTS doesn't load another Whisper) ─
print("\n[3/6] Transcribing reference audio...")
ref_segments, _ = whisper.transcribe(REF_VOICE, beam_size=1, language="en")
ref_text = " ".join(s.text.strip() for s in ref_segments)
print(f"  Reference text: \"{ref_text[:120]}...\"")
# Free Whisper to save memory before loading F5-TTS
del whisper
gc.collect()
print("  Whisper freed from memory")

# ── 4. Load F5-TTS (ONCE) ──────────────────────────────────────────────────
print("\n[4/6] Loading F5-TTS model (this loads once)...")
from f5_tts.api import F5TTS
f5 = F5TTS(device="cpu")
print("  F5-TTS model loaded")

# ── 5. Synthesize each cue ────────────────────────────────────────────────
print(f"\n[5/6] Synthesizing {len(cues)} cues with F5-TTS...")
TTS_DIR = os.path.join(OUT_DIR, "tts")
os.makedirs(TTS_DIR, exist_ok=True)

import soundfile as sf
import numpy as np

all_audio = []
sample_rate = 24000  # F5-TTS output sample rate

for i, cue in enumerate(cues):
    text = cue["text"]
    if not text:
        continue
    out_wav = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    print(f"  [{i+1}/{len(cues)}] {text[:60]}... ", end="", flush=True)

    try:
        wav, sr, _ = f5.infer(
            ref_file=REF_VOICE,
            ref_text=ref_text,
            gen_text=text,
            file_wave=out_wav,
            speed=1.0,
            nfe_step=16,       # fewer steps for CPU speed
            cfg_strength=2.0,
            sway_sampling_coef=-1,
        )
        print(f"OK ({len(wav)/sr:.1f}s)")
    except Exception as e:
        print(f"FAILED: {e}")
        continue

# ── 6. Stitch audio + mux video ──────────────────────────────────────────
print(f"\n[6/6] Stitching audio and muxing final video...")

# Build a silence-padded track from individual cues
import wave
FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio.wav")

# Create the full-length audio at 24kHz mono
final_samples = int(VIDEO_DURATION * sample_rate) + sample_rate  # +1s padding
final_audio = np.zeros(final_samples, dtype=np.float32)

for i, cue in enumerate(cues):
    wav_path = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    if not os.path.exists(wav_path):
        continue
    audio_data, sr = sf.read(wav_path)
    if sr != sample_rate:
        # Resample if needed
        import librosa
        audio_data = librosa.resample(audio_data, orig_sr=sr, target_sr=sample_rate)

    start_sample = int(cue["start"] * sample_rate)
    end_sample = min(start_sample + len(audio_data), len(final_audio))

    # If cue audio is longer than the slot, speed it up slightly
    slot_samples = int((cue["end"] - cue["start"]) * sample_rate)
    if len(audio_data) > slot_samples and len(audio_data) > 0:
        ratio = len(audio_data) / slot_samples
        print(f"    cue {i}: audio {len(audio_data)/sr:.1f}s vs slot {slot_samples/sr:.1f}s (ratio={ratio:.2f}), trimming")
        audio_data = audio_data[:slot_samples]

    final_audio[start_sample:start_sample + len(audio_data)] += audio_data[:len(final_audio) - start_sample]

# Write final audio
sf.write(FINAL_WAV, final_audio, sample_rate)
print(f"  Final audio: {FINAL_WAV} ({len(final_audio)/sample_rate:.1f}s)")

# Mux video + new audio
FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_f5_dubbed.mp4")
subprocess.run([
    "ffmpeg", "-y",
    "-i", VIDEO,
    "-i", FINAL_WAV,
    "-c:v", "copy",
    "-c:a", "aac", "-b:a", "128k",
    "-map", "0:v:0", "-map", "1:a:0",
    "-shortest",
    FINAL_VIDEO
], check=True, capture_output=True)

print(f"\n{'='*60}")
print(f"DONE! Output video: {FINAL_VIDEO}")
print(f"Size: {os.path.getsize(FINAL_VIDEO)/(1024*1024):.1f} MB")
print(f"Cues synthesized: {len([f for f in os.listdir(TTS_DIR) if f.endswith('.wav')])}")
print(f"{'='*60}")
