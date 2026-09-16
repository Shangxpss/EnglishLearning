#!/usr/bin/env python3
"""Batch watermark removal for all _dubbed.mp4 videos in the subtitle folder.

Processes each video with segment-based resume (skips completed segments
on retry). Output: <base>_nowm.mp4 (no watermark).

After processing, verifies the output is correct (frame count matches,
audio preserved, watermark removed). If verification passes, the source
_dubbed.mp4 is DELETED to free storage. If verification fails, the source
is kept so the user can retry.

Uses a MANUAL watermark region (bottom-left, 243x18px text strip)
because auto-detection misses this thin watermark. The region was found
by comparing a known-clean frame with the watermarked frame:
  detected: (12, 1051) 243x18
  with padding: (0, 1045) 270x30
"""
import os, sys, time, glob, logging

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))
logging.basicConfig(level=logging.INFO,
                    format='%(asctime)s %(levelname)s: %(message)s',
                    datefmt='%H:%M:%S')
logger = logging.getLogger(__name__)

import av
import cv2
import numpy as np

from app.services.watermark import remove_watermark

SUBTITLE_DIR = os.path.join(os.path.dirname(__file__), 'app', 'subtitle')

# Manual watermark region: (x, y, width, height) in pixels.
# Found by comparing _clean.mp4 (watermark removed) with _dubbed.mp4:
#   watermark is a thin text strip at the very bottom-left corner.
#   detected bbox: (12, 1051) 243x18
#   padded: (0, 1045) 270x30 (5px padding + dilate_px=8 for edges)
WATERMARK_REGION = [(0, 1045, 270, 30)]

# After successful processing + verification, delete the source _dubbed.mp4
# to free storage. Set to False to keep sources (for debugging).
DELETE_SOURCE_AFTER_VERIFY = True


def verify_output(source_path: str, output_path: str,
                  watermark_region) -> tuple:
    """Verify the output video is correct before deleting the source.

    Checks three things:
      1. Frame count: output must have the same number of frames as source
      2. Audio: output must have an audio stream if source does
      3. Watermark removed: the watermark region must be clean (no text)

    Args:
        source_path: path to the _dubbed.mp4 (with watermark).
        output_path: path to the _nowm.mp4 (watermark removed).
        watermark_region: (x, y, w, h) of the watermark area to check.

    Returns:
        (ok: bool, reason: str) — ok=True if all checks pass.
    """
    if not os.path.exists(output_path):
        return False, "output file does not exist"
    if os.path.getsize(output_path) == 0:
        return False, "output file is empty"

    # ── Check 1: frame count matches ──────────────────────────────
    try:
        src = av.open(source_path)
        out = av.open(output_path)
        src_frames = src.streams.video[0].frames or 0
        out_frames = out.streams.video[0].frames or 0
        src_has_audio = len(src.streams.audio) > 0
        out_has_audio = len(out.streams.audio) > 0
        src_w = src.streams.video[0].width
        src_h = src.streams.video[0].height
        out_w = out.streams.video[0].width
        out_h = out.streams.video[0].height
        src.close()
        out.close()
    except Exception as e:
        return False, f"failed to probe: {e}"

    if src_frames == 0:
        return False, f"source has 0 frames (probe failed)"
    if out_frames == 0:
        return False, f"output has 0 frames (probe failed)"
    if out_frames < src_frames:
        return False, (f"frame count mismatch: source={src_frames}, "
                       f"output={out_frames} (output is shorter)")
    # Allow output to have slightly more frames (encoder may add 1-2)
    if out_frames > src_frames + 2:
        return False, (f"frame count mismatch: source={src_frames}, "
                       f"output={out_frames} (output is longer)")

    if out_w != src_w or out_h != src_h:
        return False, (f"resolution mismatch: source={src_w}x{src_h}, "
                       f"output={out_w}x{out_h}")

    # ── Check 2: audio preserved ──────────────────────────────────
    if src_has_audio and not out_has_audio:
        return False, "source has audio but output does not"

    # ── Check 3: watermark area is clean ──────────────────────────
    # Decode the first frame of both source and output.
    # In the source, the watermark region should have visible text
    # (non-black pixels). In the output, it should be cleaned (inpaint
    # filled the area with surrounding background colors).
    try:
        src_c = av.open(source_path)
        out_c = av.open(output_path)
        src_frame = next(src_c.decode(src_c.streams.video[0])).to_ndarray(format='bgr24')
        out_frame = next(out_c.decode(out_c.streams.video[0])).to_ndarray(format='bgr24')
        src_c.close()
        out_c.close()
    except Exception as e:
        return False, f"failed to decode frames: {e}"

    x, y, w, h = watermark_region[0]
    # Extract the watermark region from both frames
    src_wm = cv2.cvtColor(src_frame[y:y+h, x:x+w], cv2.COLOR_BGR2GRAY)
    out_wm = cv2.cvtColor(out_frame[y:y+h, x:x+w], cv2.COLOR_BGR2GRAY)

    # Count non-black pixels (watermark text) in source vs output
    src_nonblack = np.count_nonzero(src_wm > 30)
    out_nonblack = np.count_nonzero(out_wm > 30)

    if src_nonblack < 10:
        # Source doesn't have a visible watermark here — region may be wrong
        logger.warning("  verify: source watermark area has %d non-black pixels "
                       "(expected > 10) — region may be incorrect", src_nonblack)
    elif out_nonblack > src_nonblack * 0.5:
        return False, (f"watermark not removed: source={src_nonblack} "
                       f"non-black px, output={out_nonblack} "
                       f"(output still has >50% of watermark pixels)")

    return True, (f"frames={out_frames}/{src_frames}, audio={out_has_audio}, "
                  f"watermark px {src_nonblack}→{out_nonblack}")


def main():
    videos = sorted(glob.glob(os.path.join(SUBTITLE_DIR, '*_dubbed.mp4')))
    if not videos:
        logger.error("No _dubbed.mp4 files found in %s", SUBTITLE_DIR)
        return 1

    logger.info("Found %d videos to process", len(videos))
    for i, v in enumerate(videos):
        logger.info("  %d. %s", i + 1, os.path.basename(v))

    results = []
    total_t0 = time.perf_counter()

    for idx, video_path in enumerate(videos):
        base = os.path.splitext(video_path)[0]  # strip .mp4
        # Remove _dubbed suffix, add _nowm
        if base.endswith('_dubbed'):
            base = base[:-7]
        output_path = base + '_nowm.mp4'

        logger.info("=" * 60)
        logger.info("[%d/%d] %s", idx + 1, len(videos), os.path.basename(video_path))
        logger.info("  output: %s", os.path.basename(output_path))
        logger.info("=" * 60)

        last_pct = [0]
        def progress_cb(completed, total):
            if total > 0:
                pct = int(100 * completed / total)
                if pct >= last_pct[0] + 10 or completed == total:
                    last_pct[0] = pct
                    logger.info("  progress: %d/%d (%d%%)",
                                completed, total, pct)

        t0 = time.perf_counter()
        try:
            result = remove_watermark(
                video_path, output_path,
                regions=WATERMARK_REGION,
                segment_frames=300,
                progress_callback=progress_cb,
            )
            elapsed = time.perf_counter() - t0
            fps = result.get('frames_processed', 0) / max(elapsed, 0.001)
            logger.info("  DONE: %d frames in %.1fs (%.1f fps), resumed from %d",
                        result.get('frames_processed', 0), elapsed, fps,
                        result.get('resumed_from', 0))

            # ── Verify output before deleting source ──────────────
            if result.get('skipped'):
                logger.info("  output was already complete (skipped)")
                verify_ok = True
                verify_reason = "skipped (already existed)"
            else:
                logger.info("  verifying output...")
                verify_ok, verify_reason = verify_output(
                    video_path, output_path, WATERMARK_REGION)

            if verify_ok:
                logger.info("  VERIFY: PASS — %s", verify_reason)

                if DELETE_SOURCE_AFTER_VERIFY and not result.get('skipped'):
                    src_size = os.path.getsize(video_path) / (1024 * 1024)
                    os.remove(video_path)
                    logger.info("  DELETED source: %s (%.1f MB freed)",
                                os.path.basename(video_path), src_size)
                results.append((os.path.basename(video_path),
                                'OK', elapsed, fps))
            else:
                logger.error("  VERIFY: FAIL — %s", verify_reason)
                logger.error("  source KEPT for retry: %s",
                             os.path.basename(video_path))
                results.append((os.path.basename(video_path),
                                f'VERIFY FAIL: {verify_reason}', elapsed, 0))

        except Exception as e:
            elapsed = time.perf_counter() - t0
            logger.error("  FAILED: %s", e)
            logger.error("  source KEPT for retry: %s",
                         os.path.basename(video_path))
            results.append((os.path.basename(video_path), f'FAIL: {e}',
                            elapsed, 0))

    total_elapsed = time.perf_counter() - total_t0
    logger.info("=" * 60)
    logger.info("BATCH COMPLETE: %d videos in %.1f min", len(videos),
                total_elapsed / 60)
    logger.info("=" * 60)
    for name, status, t, fps in results:
        logger.info("  %-55s %s  %.1fs  %.1f fps", name, status, t, fps)

if __name__ == '__main__':
    sys.exit(main() or 0)
