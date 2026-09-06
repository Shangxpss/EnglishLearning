#!/usr/bin/env python3
"""Re-run alignment + subtitle generation with correct word timestamps.

Replaces the existing ``.srt``, ``.words.srt``, ``.segments.srt`` files
in the audio directory with versions built from REAL word-level
timestamps (faster-whisper's cross-attention weights, ~100 ms accuracy)
instead of the previous uniform-interpolation fake timestamps.

Skips:
  * TTS synthesis (the dub audio is unaffected by word timing)
  * Video muxing (the ``_dubbed.mp4`` files are unchanged)
  * Watermark removal (the ``_clean.mp4`` intermediates are reused)

Per-video stages:
    1. Probe duration of ``*_dubbed.mp4``
    2. Run WordAligner with the new ``word_timestamps=True`` code path
    3. build_aligned_cues (sentence-level cues)
    4. write_srt (sentence-level, with split logic)
       write_word_srt (word-level)
       write_segments_srt (Whisper raw segments)

The script deletes any stale ``.align.json`` checkpoint before aligning
so the new code path actually runs (checkpoints embed the old uniform
timestamps and would short-circuit the alignment).
"""

from __future__ import annotations

import argparse
import gc
import glob
import os
import sys
import time

# Match the batch script's path setup so the same imports work.
BACKEND = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, BACKEND)
os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

from app.services.sync import (  # noqa: E402
    WordAligner,
    build_aligned_cues,
    get_media_duration,
    write_srt,
    write_word_srt,
    write_segments_srt,
)

# Match run_batch_dub.py configuration exactly so regenerated SRTs are
# directly comparable to what the full pipeline would produce.
WHISPER_MODEL = "base"
WHISPER_LANG = "en"
WHISPER_DEVICE = "cpu"
WHISPER_COMPUTE = "int8"
MAX_WORDS_PER_SEGMENT = 10


def find_videos(audio_dir: str, only: str = "") -> list:
    """Return sorted list of video paths to process.

    Matches ``*.mp4`` files that have a corresponding ``.srt`` subtitle
    file (so we only re-align videos we have subtitles for). Skips legacy
    ``_sync_dubbed.mp4`` outputs.
    """
    out = []
    for video in sorted(glob.glob(os.path.join(audio_dir, "*.mp4"))):
        # Skip the legacy _sync outputs — they don't have matching SRTs.
        if video.endswith("_sync.mp4") or video.endswith("_sync_dubbed.mp4"):
            continue
        stem = os.path.splitext(os.path.basename(video))[0]
        srt = os.path.join(audio_dir, stem + ".srt")
        if not os.path.exists(srt):
            continue
        if only and only.lower() not in os.path.basename(video).lower():
            continue
        out.append(video)
    return out


def clear_checkpoints(audio_dir: str, only: str = "") -> int:
    """Delete stale ``.align.json`` checkpoints so alignment re-runs."""
    n = 0
    for ckpt in glob.glob(os.path.join(audio_dir, ".*.align.json")):
        if only and only.lower() not in os.path.basename(ckpt).lower():
            continue
        os.remove(ckpt)
        n += 1
    return n


def regenerate_one(video_path: str, aligner: WordAligner,
                   audio_dir: str) -> dict:
    """Re-run alignment + subtitle stages for one video."""
    t0 = time.perf_counter()
    video_name = os.path.basename(video_path)
    video_stem = os.path.splitext(video_name)[0]
    # Use the video stem directly — the renamed videos no longer have a
    # ``_dubbed`` suffix, so their stem matches the SRT stem already.
    stem = video_stem

    # 1. duration probe
    duration = get_media_duration(video_path)

    # 2. alignment (re-runs with word_timestamps=True)
    words = aligner.align(video_path)

    # 3. cue building
    cues = build_aligned_cues(words, duration)

    # 4. write SRTs (overwrite existing files)
    srt_path = os.path.join(audio_dir, stem + ".srt")
    word_srt_path = os.path.join(audio_dir, stem + ".words.srt")
    segments_srt_path = os.path.join(audio_dir, stem + ".segments.srt")
    n_sent = write_srt(cues, srt_path, max_words_per_segment=MAX_WORDS_PER_SEGMENT)
    n_word = write_word_srt(words, word_srt_path, group_size=1)
    n_seg = write_segments_srt(aligner.last_segments, segments_srt_path)

    elapsed = time.perf_counter() - t0
    return {
        "video": video_name,
        "duration": duration,
        "n_words": len(words),
        "n_cues": len(cues),
        "n_sent_entries": n_sent,
        "n_word_entries": n_word,
        "n_seg_entries": n_seg,
        "elapsed_s": elapsed,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--audio-dir", default=os.path.join(
        BACKEND, "audio"))
    parser.add_argument("--only", default="",
                        help="substring filter on video filename")
    args = parser.parse_args()

    videos = find_videos(args.audio_dir, args.only)
    if not videos:
        print(f"No *_dubbed.mp4 files found under {args.audio_dir}")
        sys.exit(1)

    # Delete stale checkpoints so the new word_timestamps=True path runs.
    n_cleared = clear_checkpoints(args.audio_dir, args.only)
    print(f"Cleared {n_cleared} stale .align.json checkpoint(s)")

    print(f"\nRe-aligning {len(videos)} video(s) with word_timestamps=True "
          f"(model={WHISPER_MODEL}, backend interpolation+cross-attn)...\n")

    # Load aligner ONCE — Whisper model load is ~5-10 s.
    aligner = WordAligner(
        model_size=WHISPER_MODEL,
        language=WHISPER_LANG,
        device=WHISPER_DEVICE,
        compute_type=WHISPER_COMPUTE,
    )

    results = []
    for i, video in enumerate(videos, 1):
        print(f"[{i}/{len(videos)}] {os.path.basename(video)}")
        try:
            r = regenerate_one(video, aligner, args.audio_dir)
            print(f"   {r['n_words']:4d} words, {r['n_cues']:3d} cues, "
                  f"{r['n_sent_entries']:3d} sent / {r['n_word_entries']:4d} word / "
                  f"{r['n_seg_entries']:3d} seg entries  "
                  f"({r['elapsed_s']:.1f}s)")
            results.append(r)
        except Exception as e:
            print(f"   FAIL: {type(e).__name__}: {e}")
            import traceback
            traceback.print_exc()
            results.append({"video": os.path.basename(video), "error": str(e)})
        gc.collect()

    # Summary
    print(f"\n{'='*70}")
    print("Summary")
    print(f"{'='*70}")
    for r in results:
        if "error" in r:
            print(f"  FAIL  {r['video']}: {r['error']}")
        else:
            print(f"  ok    {r['video']}: {r['n_sent_entries']} sent / "
                  f"{r['n_word_entries']} word / {r['n_seg_entries']} seg entries")
    n_ok = sum(1 for r in results if "error" not in r)
    print(f"\nDone: {n_ok}/{len(results)} ok")


if __name__ == "__main__":
    main()
