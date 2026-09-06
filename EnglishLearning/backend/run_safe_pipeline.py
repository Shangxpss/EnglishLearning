#!/usr/bin/env python3
"""Safe pipeline: process videos one at a time, preserve originals until dubbed
output is fully generated, commit AND PUSH after each video.

Safety guarantees:
  * Original video is NEVER deleted/overwritten during processing.
  * Dubbed output is written to ``{stem}_dubbed_new.mp4`` (temp name).
  * SRTs are written to ``.tmp`` files during processing.
  * Only after successful muxing, atomically replace original with dubbed
    version (``os.replace``) and finalize SRTs.
  * If any stage fails, temp files are cleaned up and original is preserved.
  * After each successful video: ``git add`` + ``git commit`` + ``git push``
    so finished work is safe on the remote (workspace is ephemeral).

Usage::

    .venv/bin/python run_safe_pipeline.py
"""

import gc
import os
import shutil
import subprocess
import sys
import time

import numpy as np
import soundfile as sf

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

from app.services.sync import (  # noqa: E402
    WordAligner,
    build_aligned_cues,
    PlaceStitcher,
    RespeedSynthesizer,
    VoiceConfig,
    build_backend,
    get_media_duration,
    mux_video_audio,
    write_srt,
    write_word_srt,
    write_segments_srt,
)

# ── configuration ──────────────────────────────────────────────────────────
VOICE = VoiceConfig(
    backend="edge_tts",
    gender="female",
    edge_voice="en-GB-MaisieNeural",
)
MAX_RETRIES = 2
MAX_CONCURRENCY = 8
MAX_WORDS_PER_SEGMENT = 10

WHISPER_MODEL = "base"
WHISPER_LANG = "en"
WHISPER_DEVICE = "cpu"
WHISPER_COMPUTE = "int8"

BACKEND = os.path.dirname(os.path.abspath(__file__))
VIDEO_DIR = os.path.join(BACKEND, "video")
REPO_ROOT = "/workspace"

# Watermark region (x, y, w, h) in pixels for 1920x1080 source videos.
WATERMARK_REGION = [(0, 1045, 270, 30)]
REMOVE_WATERMARK = os.environ.get("DUB_REMOVE_WATERMARK", "1") == "1"


def find_videos(video_dir: str) -> list:
    """Return sorted list of .mp4 files that still need processing.

    Skips:
      * temp files from a failed run (``*_dubbed_new.mp4``)
      * watermark intermediates (``*_clean.mp4``)
      * already-processed videos (have a sibling ``.srt`` — resume marker)
    """
    if not os.path.isdir(video_dir):
        raise SystemExit(f"video dir not found: {video_dir}")
    videos = []
    for f in sorted(os.listdir(video_dir)):
        if not f.lower().endswith(".mp4"):
            continue
        if f.endswith("_dubbed_new.mp4"):
            continue  # stale temp from a failed run
        if f.endswith("_clean.mp4"):
            continue  # stale watermark-removal intermediate
        stem = os.path.splitext(f)[0]
        srt = os.path.join(video_dir, stem + ".srt")
        if os.path.exists(srt):
            print(f"[resume] {f} already has .srt — skipping")
            continue
        videos.append(os.path.join(video_dir, f))
    return videos


def _reencode_for_size(output_path: str, target_mb: float = 90.0) -> None:
    """Re-encode a muxed video to fit under a size limit (ffmpeg transcode).

    Computes the target bitrate from the video duration and target size,
    then transcodes with libx264 + downscale to 1280x720. Used when the
    muxed output exceeds GitHub's 100 MB file size limit.
    """
    import subprocess
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration",
         "-of", "default=noprint_wrappers=1:nokey=1", output_path],
        capture_output=True, text=True)
    duration = float(probe.stdout.strip())
    if duration <= 0:
        raise RuntimeError(f"cannot probe duration of {output_path}")
    total_bps = int(target_mb * 8 * 1024 * 1024 / duration)
    audio_bps = 96_000
    video_bps = max(80_000, total_bps - audio_bps)
    tmp_out = output_path + ".small.mp4"
    cmd = [
        "ffmpeg", "-y", "-i", output_path,
        "-c:v", "libx264",
        "-b:v", str(video_bps),
        "-maxrate", str(int(video_bps * 1.3)),
        "-bufsize", str(int(video_bps * 2)),
        "-preset", "veryfast",
        "-vf", "scale=1280:720",
        "-c:a", "aac", "-b:a", "96k",
        "-movflags", "+faststart",
        tmp_out,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=3600)
    if result.returncode != 0 or not os.path.exists(tmp_out):
        if os.path.exists(tmp_out):
            os.remove(tmp_out)
        raise RuntimeError(f"re-encode failed: {result.stderr[-500:]}")
    os.replace(tmp_out, output_path)


def process_video_safe(video_path: str, aligner: WordAligner,
                       backend, sample_rate: int, video_dir: str) -> bool:
    """Run the full pipeline on one video with safe output naming.

    Original is preserved until dubbed output is complete.
    Returns True on success, False on failure.
    """
    video_name = os.path.basename(video_path)
    stem = os.path.splitext(video_name)[0]

    # Temp output names — original stays untouched until success
    temp_video = os.path.join(video_dir, stem + "_dubbed_new.mp4")
    temp_srt = os.path.join(video_dir, stem + ".srt.tmp")
    temp_word_srt = os.path.join(video_dir, stem + ".words.srt.tmp")
    temp_segments_srt = os.path.join(video_dir, stem + ".segments.srt.tmp")
    tts_dir = os.path.join(video_dir, "tts")
    final_wav = os.path.join(video_dir, "dubbed_audio.wav")
    clean_path = os.path.join(video_dir, stem + "_clean.mp4")

    os.makedirs(tts_dir, exist_ok=True)

    try:
        print(f"\n{'='*70}")
        print(f"Processing: {video_name}")
        print(f"{'='*70}")

        # ── 0. watermark removal (optional) ─────────────────────────────
        src_video = video_path
        if REMOVE_WATERMARK and WATERMARK_REGION:
            t0 = time.perf_counter()
            from app.services.watermark import remove_watermark as _rm_wm
            result_wm = _rm_wm(video_path, clean_path,
                               regions=WATERMARK_REGION)
            if result_wm.get("skipped"):
                print(f"  [0] watermark: skipped ({time.perf_counter()-t0:.2f}s)")
            elif result_wm.get("frames_processed", 0) == 0:
                print(f"  [0] watermark: none detected, remuxed "
                      f"({time.perf_counter()-t0:.2f}s)")
            else:
                n_frames = result_wm["frames_processed"]
                print(f"  [0] watermark: removed {n_frames} frames "
                      f"({time.perf_counter()-t0:.2f}s)")
            if os.path.exists(clean_path):
                src_video = clean_path

        # ── 1. duration probe ───────────────────────────────────────────
        t0 = time.perf_counter()
        duration = get_media_duration(src_video)
        print(f"  [1] duration: {duration:.1f}s ({time.perf_counter()-t0:.2f}s)")

        # ── 2. word alignment (aligner reused) ──────────────────────────
        t0 = time.perf_counter()
        words = aligner.align(src_video)
        n_low = sum(1 for w in words if w.score < 0.5)
        print(f"  [2] alignment: {len(words)} words "
              f"({time.perf_counter()-t0:.1f}s, low-conf={n_low})")

        # ── 3. cue building ──────────────────────────────────────────────
        t0 = time.perf_counter()
        cues = build_aligned_cues(words, duration)
        print(f"  [3] cues: {len(cues)} sentences ({time.perf_counter()-t0:.3f}s)")

        # ── 4. subtitles (to temp .tmp files) ───────────────────────────
        t0 = time.perf_counter()
        n_sent = write_srt(cues, temp_srt, max_words_per_segment=MAX_WORDS_PER_SEGMENT)
        n_word = write_word_srt(words, temp_word_srt, group_size=1)
        n_seg = write_segments_srt(aligner.last_segments, temp_segments_srt)
        print(f"  [4] subtitles: {n_sent} sent + {n_word} words + {n_seg} seg "
              f"({time.perf_counter()-t0:.3f}s)")

        # ── 5. respeed synthesis ────────────────────────────────────────
        t0 = time.perf_counter()
        synthesizer = RespeedSynthesizer(
            backend=backend,
            max_retries=MAX_RETRIES,
            max_concurrency=MAX_CONCURRENCY,
        )
        synth_paths = synthesizer.synthesize(cues, tts_dir)
        print(f"  [5] synthesis: {len(synth_paths)}/{len(cues)} "
              f"({time.perf_counter()-t0:.1f}s)")

        # ── 6. placement stitching ──────────────────────────────────────
        t0 = time.perf_counter()
        sentence_audio = {}
        for ci in range(len(cues)):
            wav_path = os.path.join(tts_dir, f"sent_{ci:04d}.wav")
            if not os.path.exists(wav_path):
                continue
            ad, sr = sf.read(wav_path)
            if sr != sample_rate:
                import librosa  # lazy
                ad = librosa.resample(np.asarray(ad, dtype=np.float32),
                                      orig_sr=sr, target_sr=sample_rate)
            if ad.ndim > 1:
                ad = ad[:, 0]
            sentence_audio[ci] = ad
        stitcher = PlaceStitcher(sample_rate=sample_rate)
        final_audio = stitcher.stitch(sentence_audio, cues, duration)
        sf.write(final_wav, final_audio, sample_rate)
        print(f"  [6] stitching: {len(final_audio)/sample_rate:.1f}s "
              f"({time.perf_counter()-t0:.2f}s)")

        # ── 7. muxing (output to TEMP name — original is safe) ─────────
        t0 = time.perf_counter()
        mux_video_audio(
            video_path=src_video,
            audio_path=final_wav,
            output_path=temp_video,
            audio_bitrate=128_000,
            shortest=True,
        )
        out_size = os.path.getsize(temp_video) / (1024 * 1024)
        print(f"  [7] muxing: {out_size:.1f} MB ({time.perf_counter()-t0:.1f}s)")

        # ── 7b. size guard: GitHub rejects files > 100 MB ───────────────
        if out_size > 95.0:
            print(f"  [7b] {out_size:.1f} MB > 95 MB limit — re-encoding...")
            t0b = time.perf_counter()
            _reencode_for_size(temp_video, target_mb=90.0)
            out_size = os.path.getsize(temp_video) / (1024 * 1024)
            print(f"  [7b] re-encoded: {out_size:.1f} MB "
                  f"({time.perf_counter()-t0b:.1f}s)")

        # ── SUCCESS: atomically replace original with dubbed version ────
        final_video = os.path.join(video_dir, stem + ".mp4")
        os.replace(temp_video, final_video)
        os.replace(temp_srt, os.path.join(video_dir, stem + ".srt"))
        os.replace(temp_word_srt, os.path.join(video_dir, stem + ".words.srt"))
        os.replace(temp_segments_srt, os.path.join(video_dir, stem + ".segments.srt"))

        print(f"  [ok] replaced original with dubbed version")
        return True

    except Exception as e:
        import traceback
        print(f"  [FAIL] {e}")
        traceback.print_exc()
        for p in [temp_video, temp_srt, temp_word_srt, temp_segments_srt]:
            if os.path.exists(p):
                os.remove(p)
        return False
    finally:
        if os.path.isdir(tts_dir):
            shutil.rmtree(tts_dir, ignore_errors=True)
        if os.path.exists(final_wav):
            os.remove(final_wav)
        if os.path.exists(clean_path):
            try:
                os.remove(clean_path)
            except OSError:
                pass


def git_commit_local(stem: str, video_dir: str) -> bool:
    """Commit dubbed video + SRTs for one video locally (no push)."""
    rel_dir = os.path.relpath(video_dir, REPO_ROOT)
    files = [
        os.path.join(rel_dir, stem + ".mp4"),
        os.path.join(rel_dir, stem + ".srt"),
        os.path.join(rel_dir, stem + ".words.srt"),
        os.path.join(rel_dir, stem + ".segments.srt"),
    ]

    subprocess.run(["git", "reset"], cwd=REPO_ROOT, capture_output=True)

    for f in files:
        result = subprocess.run(
            ["git", "add", "--", f],
            cwd=REPO_ROOT, capture_output=True, text=True)
        if result.returncode != 0:
            print(f"  [git] add failed for {f}: {result.stderr.strip()}")
            return False

    msg = f"feat: dub {stem[:70]}"
    result = subprocess.run(
        ["git", "commit", "-m", msg],
        cwd=REPO_ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        if "nothing to commit" in result.stdout.lower():
            print(f"  [git] nothing to commit (already up to date)")
            return True
        print(f"  [git] commit failed: {result.stderr.strip()}")
        return False

    print(f"  [git] committed locally")
    return True


def git_push_remote() -> bool:
    """Push commits to remote. Called after EACH video."""
    result = subprocess.run(
        ["git", "push", "origin", "HEAD:master"],
        cwd=REPO_ROOT, capture_output=True, text=True, timeout=300)
    if result.returncode != 0:
        stderr = result.stderr.strip()
        stdout = result.stdout.strip()
        print(f"  [git] push FAILED: {stderr or stdout}")
        return False
    print(f"  [git] pushed to remote")
    return True


def main():
    print(f"Video dir: {VIDEO_DIR}")
    print(f"Repo root: {REPO_ROOT}")
    print(f"Watermark removal: {'ENABLED' if REMOVE_WATERMARK else 'DISABLED'}"
          f" (regions={WATERMARK_REGION})")

    videos = find_videos(VIDEO_DIR)
    print(f"\nFound {len(videos)} video(s) to process:")
    for v in videos:
        print(f"  - {os.path.basename(v)}")

    if not videos:
        print("\nNothing to do — all videos already have .srt files.")
        return

    # ── one-time setup: load aligner + backend ───────────────────────────
    print(f"\n[setup] Loading WordAligner (model={WHISPER_MODEL}, "
          f"compute={WHISPER_COMPUTE})...")
    t0 = time.perf_counter()
    aligner = WordAligner(model_size=WHISPER_MODEL, language=WHISPER_LANG,
                          device=WHISPER_DEVICE, compute_type=WHISPER_COMPUTE)
    print(f"  aligner loaded in {time.perf_counter()-t0:.1f}s "
          f"(backend={aligner.backend})")

    print(f"[setup] Building TTS backend ({VOICE.backend})...")
    t0 = time.perf_counter()
    backend = build_backend(VOICE)
    sample_rate = backend.sample_rate
    print(f"  {VOICE.describe()}")
    print(f"  backend ready in {time.perf_counter()-t0:.2f}s "
          f"(sample_rate={sample_rate})")

    # ── process each video ────────────────────────────────────────────────
    results = []
    for i, video_path in enumerate(videos, 1):
        video_name = os.path.basename(video_path)
        stem = os.path.splitext(video_name)[0]

        print(f"\n{'#'*70}")
        print(f"# VIDEO {i}/{len(videos)}: {video_name}")
        print(f"{'#'*70}")

        t0 = time.perf_counter()
        success = process_video_safe(video_path, aligner, backend,
                                      sample_rate, VIDEO_DIR)
        elapsed = time.perf_counter() - t0

        if success:
            print(f"\n  [done] {elapsed:.1f}s — committing + pushing...")
            git_ok = git_commit_local(stem, VIDEO_DIR)
            push_ok = False
            if git_ok:
                push_ok = git_push_remote()
            results.append((video_name, success, elapsed, git_ok, push_ok))
        else:
            results.append((video_name, False, elapsed, False, False))
            print(f"\n  [fail] {elapsed:.1f}s — continuing to next video")

    # ── cleanup ──────────────────────────────────────────────────────────
    del aligner
    gc.collect()

    # ── final report ──────────────────────────────────────────────────────
    print(f"\n{'='*70}")
    print("FINAL REPORT")
    print(f"{'='*70}")
    for name, ok, t, git_ok, push_ok in results:
        status = "OK " if ok else "FAIL"
        if ok and push_ok:
            git_status = "committed + pushed"
        elif ok and git_ok:
            git_status = "committed (push FAILED)"
        else:
            git_status = "NOT committed"
        print(f"  [{status}] {name[:55]:<55} {t:>7.1f}s  {git_status}")
    n_ok = sum(1 for _, ok, _, _, _ in results if ok)
    n_pushed = sum(1 for _, _, _, _, p in results if p)
    print(f"\n  {n_ok}/{len(results)} videos succeeded")
    print(f"  {n_pushed}/{len(results)} videos pushed to remote")
    print(f"{'='*70}")


if __name__ == "__main__":
    main()
