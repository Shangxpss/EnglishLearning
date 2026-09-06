#!/usr/bin/env python3 -u
"""Stitch v3 — preserve original rhythm.
Reuses TTS files from v2, only changes the stitching logic:
- Only compress when TTS overflows the slot (never expand)
- Leave natural silence when TTS is shorter than the slot
- Gentle fade edges instead of crossfade overlap
"""

import os, sys, subprocess, json, re, gc
import numpy as np
import soundfile as sf
import librosa

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

BACKEND = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VIDEO = os.path.join(BACKEND, "app", "subtitle",
                     "1 - Welcome to Your Kafka Journey--[koudaizy.com].mp4")
REF_VOICE = os.path.join(BACKEND, "app", "assets", "reference_voices",
                         "london_default.wav")
OUT_DIR = os.path.join(BACKEND, "app", "subtitle", "f5_dub_v2")
TTS_DIR = os.path.join(OUT_DIR, "tts")

# Get video duration
result = subprocess.run(
    ["ffprobe", "-v", "error", "-show_entries", "format=duration",
     "-of", "default=noprint_wrappers=1:nokey=1", VIDEO],
    capture_output=True, text=True)
VIDEO_DURATION = float(result.stdout.strip())
sample_rate = 24000
FADE = 0.03  # 30ms gentle fade

# ── Re-transcribe to get the merged sentence boundaries ─────────────────────
print("[1/3] Re-transcribing to get sentence boundaries...")
EXTRACTED_WAV = os.path.join(OUT_DIR, "source_audio.wav")
from faster_whisper import WhisperModel
whisper = WhisperModel("base", device="cpu", compute_type="int8")
raw_segments, info = whisper.transcribe(EXTRACTED_WAV, beam_size=5, language="en")
raw_cues = [{"start": s.start, "end": s.end, "text": s.text.strip()} for s in raw_segments]

# Merge into sentences
merged_cues = []
buffer_text = ""
buffer_start = 0.0
buffer_end = 0.0
for cue in raw_cues:
    if not buffer_text:
        buffer_start = cue["start"]
    buffer_text = (buffer_text + " " + cue["text"]).strip()
    buffer_end = cue["end"]
    if re.search(r'[.!?]\s*$', cue["text"].strip()):
        merged_cues.append({"start": buffer_start, "end": buffer_end, "text": buffer_text})
        buffer_text = ""
if buffer_text:
    merged_cues.append({"start": buffer_start, "end": buffer_end, "text": buffer_text})
print(f"  {len(merged_cues)} sentences")

# ── Stitch: preserve original rhythm ───────────────────────────────────────
print(f"\n[2/3] Stitching audio (preserve rhythm mode)...")
final_samples = int(VIDEO_DURATION * sample_rate) + sample_rate
final_audio = np.zeros(final_samples, dtype=np.float32)

for i, cue in enumerate(merged_cues):
    wav_path = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    if not os.path.exists(wav_path):
        print(f"  cue {i}: MISSING, skipping")
        continue

    audio_data, sr = sf.read(wav_path)
    if sr != sample_rate:
        audio_data = librosa.resample(audio_data, orig_sr=sr, target_sr=sample_rate)
    if audio_data.ndim > 1:
        audio_data = audio_data[:, 0]

    slot_dur = cue["end"] - cue["start"]
    slot_samples = int(slot_dur * sample_rate)
    tts_dur = len(audio_data) / sample_rate
    start_sample = int(cue["start"] * sample_rate)

    # Only compress if TTS overflows the slot (would overlap next sentence)
    if len(audio_data) > slot_samples:
        ratio = len(audio_data) / slot_samples
        # Compress, but cap at 1.3x to avoid chipmunk effect
        compress_rate = min(ratio, 1.3)
        print(f"  cue {i}: compress {tts_dur:.1f}s → {slot_dur:.1f}s (x{compress_rate:.2f})")
        audio_data = librosa.effects.time_stretch(audio_data, rate=compress_rate)
    else:
        # TTS fits — leave natural silence after it (preserves original rhythm)
        gap = slot_dur - tts_dur
        if gap > 0.1:
            print(f"  cue {i}: {tts_dur:.1f}s in {slot_dur:.1f}s slot, {gap:.1f}s natural pause")

    # Gentle fade in/out (3ms — just enough to avoid clicks)
    fade_samples = int(FADE * sample_rate)
    if len(audio_data) > 2 * fade_samples:
        audio_data[:fade_samples] *= np.linspace(0, 1, fade_samples)
        audio_data[-fade_samples:] *= np.linspace(1, 0, fade_samples)

    # Place audio at original start time
    end_sample = min(start_sample + len(audio_data), len(final_audio))
    audio_data = audio_data[:end_sample - start_sample]
    final_audio[start_sample:end_sample] = audio_data

# Normalize
peak = np.max(np.abs(final_audio))
if peak > 0.95:
    final_audio = final_audio * (0.95 / peak)

FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio_v3.wav")
sf.write(FINAL_WAV, final_audio, sample_rate)
print(f"\n  Final audio: {FINAL_WAV} ({len(final_audio)/sample_rate:.1f}s)")

# ── Mux ─────────────────────────────────────────────────────────────────────
print(f"\n[3/3] Muxing final video...")
FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_f5_dubbed_v3.mp4")
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
print(f"{'='*60}")
