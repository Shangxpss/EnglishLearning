#!/usr/bin/env python3 -u
"""F5-TTS dubbing v2 — fixes unnatural breaks by merging segments into
complete sentences before synthesis, time-stretching to fit slots, and
crossfading between cues."""

import os, sys, gc, subprocess, json, re, tempfile, time
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
OUT_DIR = os.path.join(BACKEND, "app", "subtitle", "f5_dub_v2")
os.makedirs(OUT_DIR, exist_ok=True)

print(f"Video       : {VIDEO}")
print(f"Reference   : {REF_VOICE}")
print(f"Output dir  : {OUT_DIR}")

# ── 1. Extract audio ────────────────────────────────────────────────────────
EXTRACTED_WAV = os.path.join(OUT_DIR, "source_audio.wav")
if not os.path.exists(EXTRACTED_WAV):
    print("\n[1/7] Extracting audio from video...")
    subprocess.run(
        ["ffmpeg", "-y", "-i", VIDEO, "-vn", "-ar", "16000", "-ac", "1",
         "-f", "wav", EXTRACTED_WAV],
        check=True, capture_output=True)
    print("  done")
else:
    print(f"\n[1/7] Audio already extracted")

result = subprocess.run(
    ["ffprobe", "-v", "error", "-show_entries", "format=duration",
     "-of", "default=noprint_wrappers=1:nokey=1", VIDEO],
    capture_output=True, text=True)
VIDEO_DURATION = float(result.stdout.strip())
print(f"  Video duration: {VIDEO_DURATION:.1f}s")

# ── 2. Transcribe with Whisper ─────────────────────────────────────────────
print("\n[2/7] Transcribing video audio with faster-whisper...")
from faster_whisper import WhisperModel
whisper = WhisperModel("base", device="cpu", compute_type="int8")
raw_segments, info = whisper.transcribe(EXTRACTED_WAV, beam_size=5, language="en")
raw_cues = []
for seg in raw_segments:
    raw_cues.append({
        "start": seg.start,
        "end": seg.end,
        "text": seg.text.strip(),
    })
print(f"  Raw segments: {len(raw_cues)} (lang={info.language}, conf={info.language_probability:.2f})")

# ── 3. Transcribe reference audio ──────────────────────────────────────────
print("\n[3/7] Transcribing reference audio...")
ref_segments, _ = whisper.transcribe(REF_VOICE, beam_size=1, language="en")
ref_text = " ".join(s.text.strip() for s in ref_segments)
print(f"  Reference text: \"{ref_text[:120]}...\"")
del whisper
gc.collect()
print("  Whisper freed from memory")

# ── 4. Merge raw segments into complete sentences ──────────────────────────
# Problem: Whisper breaks by VAD timing, not sentence boundaries.
# Solution: Accumulate segments until we hit a sentence-ending punctuation
# (., !, ?, and the text ends with it), then emit as one merged cue.
print("\n[4/7] Merging segments into complete sentences...")

merged_cues = []
buffer_text = ""
buffer_start = 0.0
buffer_end = 0.0

for cue in raw_cues:
    if not buffer_text:
        buffer_start = cue["start"]
    buffer_text = (buffer_text + " " + cue["text"]).strip()
    buffer_end = cue["end"]

    # Check if this cue ends a sentence
    ends_sentence = bool(re.search(r'[.!?]\s*$', cue["text"].strip()))

    if ends_sentence:
        merged_cues.append({
            "start": buffer_start,
            "end": buffer_end,
            "text": buffer_text,
        })
        buffer_text = ""
        buffer_start = 0.0
        buffer_end = 0.0

# Don't lose trailing text
if buffer_text:
    merged_cues.append({
        "start": buffer_start,
        "end": buffer_end if buffer_end > 0 else VIDEO_DURATION,
        "text": buffer_text,
    })

print(f"  Merged into {len(merged_cues)} sentences (from {len(raw_cues)} raw segments)")
for i, c in enumerate(merged_cues[:5]):
    dur = c["end"] - c["start"]
    print(f"    [{i}] ({dur:.1f}s) {c['text'][:90]}...")

# ── 5. Load F5-TTS ──────────────────────────────────────────────────────────
print("\n[5/7] Loading F5-TTS model...")
from f5_tts.api import F5TTS
f5 = F5TTS(device="cpu")
print("  F5-TTS model loaded")

# ── 6. Synthesize each sentence ────────────────────────────────────────────
print(f"\n[6/7] Synthesizing {len(merged_cues)} sentences with F5-TTS...")
TTS_DIR = os.path.join(OUT_DIR, "tts")
os.makedirs(TTS_DIR, exist_ok=True)

import soundfile as sf
import numpy as np
import librosa

sample_rate = 24000
CROSSFADE = 0.08  # 80ms crossfade between cues

for i, cue in enumerate(merged_cues):
    text = cue["text"]
    if not text:
        continue
    out_wav = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    slot_dur = cue["end"] - cue["start"]
    print(f"  [{i+1}/{len(merged_cues)}] ({slot_dur:.1f}s slot) {text[:60]}... ", end="", flush=True)

    try:
        wav, sr, _ = f5.infer(
            ref_file=REF_VOICE,
            ref_text=ref_text,
            gen_text=text,
            file_wave=out_wav,
            speed=1.0,
            nfe_step=16,
            cfg_strength=2.0,
            sway_sampling_coef=-1,
        )
        tts_dur = len(wav) / sr
        print(f"OK (tts={tts_dur:.1f}s)")
    except Exception as e:
        print(f"FAILED: {e}")
        continue

# ── 7. Stitch with time-stretch + crossfade ────────────────────────────────
print(f"\n[7/7] Stitching audio with time-stretch + crossfade...")

FINAL_WAV = os.path.join(OUT_DIR, "dubbed_audio.wav")
final_samples = int(VIDEO_DURATION * sample_rate) + sample_rate
final_audio = np.zeros(final_samples, dtype=np.float32)

for i, cue in enumerate(merged_cues):
    wav_path = os.path.join(TTS_DIR, f"cue_{i:04d}.wav")
    if not os.path.exists(wav_path):
        continue

    audio_data, sr = sf.read(wav_path)
    if sr != sample_rate:
        audio_data = librosa.resample(audio_data, orig_sr=sr, target_sr=sample_rate)

    # Ensure mono
    if audio_data.ndim > 1:
        audio_data = audio_data[:, 0]

    slot_dur = cue["end"] - cue["start"]
    slot_samples = int(slot_dur * sample_rate)
    tts_samples = len(audio_data)

    start_sample = int(cue["start"] * sample_rate)

    # Time-stretch to fit the slot exactly (preserving pitch)
    if tts_samples > 0 and slot_samples > 0:
        ratio = tts_samples / slot_samples
        if ratio > 1.05:
            # TTS is too long — speed it up (compress)
            print(f"    cue {i}: compressing {tts_samples/sr:.1f}s → {slot_dur:.1f}s (ratio={ratio:.2f})")
            audio_data = librosa.effects.time_stretch(audio_data, rate=ratio)
        elif ratio < 0.90:
            # TTS is too short — slow it down (expand) to fill the gap
            print(f"    cue {i}: expanding {tts_samples/sr:.1f}s → {slot_dur:.1f}s (ratio={ratio:.2f})")
            audio_data = librosa.effects.time_stretch(audio_data, rate=ratio)
        # else: close enough, use as-is

    # Place audio with crossfade into the final track
    end_sample = start_sample + len(audio_data)
    if end_sample > len(final_audio):
        end_sample = len(final_audio)
        audio_data = audio_data[:end_sample - start_sample]

    # Apply fade-in/fade-out for smooth transitions
    fade_samples = int(CROSSFADE * sample_rate)
    if len(audio_data) > 2 * fade_samples:
        # Fade in
        fade_in = np.linspace(0, 1, fade_samples)
        audio_data[:fade_samples] *= fade_in
        # Fade out
        fade_out = np.linspace(1, 0, fade_samples)
        audio_data[-fade_samples:] *= fade_out

    # Mix with crossfade (overlap with existing audio at the start)
    overlap_start = max(0, start_sample - fade_samples)
    overlap_len = min(fade_samples, start_sample - overlap_start)
    if overlap_len > 0:
        # Crossfade: existing audio fades out, new audio fades in
        existing = final_audio[overlap_start:overlap_start + overlap_len]
        new_part = audio_data[:overlap_len]
        fade_existing = np.linspace(1, 0, overlap_len)
        fade_new = np.linspace(0, 1, overlap_len)
        final_audio[overlap_start:overlap_start + overlap_len] = existing * fade_existing + new_part * fade_new
        # Place the rest
        final_audio[start_sample + overlap_len:end_sample] = audio_data[overlap_len:]
    else:
        final_audio[start_sample:end_sample] = audio_data

# Normalize to prevent clipping
peak = np.max(np.abs(final_audio))
if peak > 0.95:
    final_audio = final_audio * (0.95 / peak)

sf.write(FINAL_WAV, final_audio, sample_rate)
print(f"  Final audio: {FINAL_WAV} ({len(final_audio)/sample_rate:.1f}s)")

# Mux video + new audio
FINAL_VIDEO = os.path.join(OUT_DIR, "kafka_f5_dubbed_v2.mp4")
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
