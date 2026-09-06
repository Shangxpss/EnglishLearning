#!/usr/bin/env python3 -u
"""Chatterbox TTS dubbing — natural voice cloning with rhythm preservation.
Uses Chatterbox (0.5B Llama backbone) for natural-sounding speech with
voice cloning from a London accent reference, while preserving the original
video's rhythm by keeping natural silence gaps."""

import os, sys, gc, subprocess, re, time
import numpy as np
import soundfile as sf
import librosa

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# ── paths ──────────────────────────────────────────────────────────────────
BACKEND = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VIDEO = os.path.join(BACKEND, "app", "subtitle",
                     "1 - Welcome to Your Kafka Journey--[koudaizy.com].mp4")
REF_VOICE = os.path.join(BACKEND, "app", "assets", "reference_voices",
                         "london_default.wav")
OUT_DIR = os.path.join(BACKEND, "app", "subtitle", "chatterbox_dub")
os.makedirs(OUT_DIR, exist_ok=True)

print(f"Video       : {VIDEO}")
print(f"Reference   : {REF_VOICE}")
print(f"Output dir  : {OUT_DIR}")

# ── 1. Extract audio ────────────────────────────────────────────────────────
EXTRACTED_WAV = os.path.join(OUT_DIR, "source_audio.wav")
if not os.path.exists(EXTRACTED_WAV):
    print("\n[1/6] Extracting audio from video...")
    subprocess.run(
        ["ffmpeg", "-y", "-i", VIDEO, "-vn", "-ar", "16000", "-ac", "1",
         "-f", "wav", EXTRACTED_WAV],
        check=True, capture_output=True)
else:
    print(f"\n[1/6] Audio already extracted")

result = subprocess.run(
    ["ffprobe", "-v", "error", "-show_entries", "format=duration",
     "-of", "default=noprint_wrappers=1:nokey=1", VIDEO],
    capture_output=True, text=True)
VIDEO_DURATION = float(result.stdout.strip())
print(f"  Video duration: {VIDEO_DURATION:.1f}s")

# ── 2. Transcribe with Whisper ─────────────────────────────────────────────
print("\n[2/6] Transcribing video audio with faster-whisper...")
from faster_whisper import WhisperModel
whisper = WhisperModel("base", device="cpu", compute_type="int8")
raw_segments, info = whisper.transcribe(EXTRACTED_WAV, beam_size=5, language="en")
raw_cues = [{"start": s.start, "end": s.end, "text": s.text.strip()} for s in raw_segments]
print(f"  Raw segments: {len(raw_cues)} (lang={info.language}, conf={info.language_probability:.2f})")

# ── 3. Merge segments into complete sentences ──────────────────────────────
print("\n[3/6] Merging segments into complete sentences...")
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
    merged_cues.append({"start": buffer_start, "end": buffer_end if buffer_end > 0 else VIDEO_DURATION, "text": buffer_text})
print(f"  Merged into {len(merged_cues)} sentences (from {len(raw_cues)} raw segments)")
for i, c in enumerate(merged_cues[:5]):
    dur = c["end"] - c["start"]
    print(f"    [{i}] ({dur:.1f}s) {c['text'][:90]}...")

# Free whisper before loading Chatterbox (memory)
del whisper
gc.collect()
print("  Whisper freed from memory")

# ── 4. Load Chatterbox TTS ───────────────────────────────────────────────────
print("\n[4/6] Loading Chatterbox TTS model (CPU mode)...")
import torch
import torchaudio as ta
device = "cpu"
torch.set_num_threads(2)  # Limit threads to reduce memory pressure
print(f"  Device: {device}, threads: {torch.get_num_threads()}")

from chatterbox.tts import ChatterboxTTS
model = ChatterboxTTS.from_pretrained(device=device)
print("  Chatterbox model loaded")

# ── 5. Synthesize each sentence ──────────────────────────────────────────────
print(f"\n[5/6] Synthesizing {len(merged_cues)} sentences with Chatterbox...")
TTS_DIR = os.path.join(OUT_DIR, "tts")
os.makedirs(TTS_DIR, exist_ok=True)

sample_rate = model.sr
print(f"  Output sample rate: {sample_rate}")

for i, cue in enumerate(merged_cues):
    text = cue["text"]
    if not text:
        continue
    out_wav = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    # Skip already synthesized
    if os.path.exists(out_wav):
        audio_data, sr = sf.read(out_wav)
        print(f"  [{i+1}/{len(merged_cues)}] already done ({len(audio_data)/sr:.1f}s)")
        continue
    slot_dur = cue["end"] - cue["start"]
    print(f"  [{i+1}/{len(merged_cues)}] ({slot_dur:.1f}s slot) {text[:60]}... ", end="", flush=True)

    try:
        with torch.inference_mode():
            wav = model.generate(
                text,
                audio_prompt_path=REF_VOICE,
                exaggeration=0.5,
                cfg_weight=0.5,
            )
        ta.save(out_wav, wav, sample_rate)
        audio_data, sr = sf.read(out_wav)
        tts_dur = len(audio_data) / sr
        print(f"OK (tts={tts_dur:.1f}s)")
        # Free memory
        del wav, audio_data
        torch.empty_cache()
        gc.collect()
    except Exception as e:
        print(f"FAILED: {e}")
        torch.empty_cache()
        gc.collect()
        continue

# ── 6. Stitch with rhythm preservation ──────────────────────────────────────
print(f"\n[6/6] Stitching audio (preserve rhythm mode)...")
final_samples = int(VIDEO_DURATION * sample_rate) + sample_rate
final_audio = np.zeros(final_samples, dtype=np.float32)

FADE = 0.03  # 30ms gentle fade

for i, cue in enumerate(merged_cues):
    wav_path = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    if not os.path.exists(wav_path):
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
        compress_rate = min(ratio, 1.3)  # Cap at 1.3x to avoid chipmunk
        print(f"    cue {i}: compress {tts_dur:.1f}s → {slot_dur:.1f}s (x{compress_rate:.2f})")
        audio_data = librosa.effects.time_stretch(audio_data, rate=compress_rate)
    else:
        gap = slot_dur - tts_dur
        if gap > 0.1:
            print(f"    cue {i}: {tts_dur:.1f}s in {slot_dur:.1f}s slot, {gap:.1f}s natural pause")

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

FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio.wav")
sf.write(FINAL_WAV, final_audio, sample_rate)
print(f"\n  Final audio: {FINAL_WAV} ({len(final_audio)/sample_rate:.1f}s)")

# ── Mux video + new audio ─────────────────────────────────────────────────────
FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_chatterbox_dubbed.mp4")
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
print(f"Sentences synthesized: {len([f for f in os.listdir(TTS_DIR) if f.endswith('.wav')])}")
print(f"{'='*60}")
