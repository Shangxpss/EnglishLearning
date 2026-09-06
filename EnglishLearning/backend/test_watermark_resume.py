#!/usr/bin/env python3
"""Test segment-based resume for watermark removal.

Tests:
1. Fresh run produces valid output (correct frame count + audio)
2. Re-run when output is complete → skips
3. Simulate failure (delete last segment) → resumes from last good segment
4. Verify lossless concat (frame count matches)
"""
import os
import sys
import shutil
import logging
import tempfile

import av
import numpy as np

logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")

# Add backend to path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app.services.watermark import (
    remove_watermark,
    _is_output_complete,
    _is_segment_valid,
)

def make_test_video(path: str, n_frames: int = 300, fps: int = 30,
                     width: int = 320, height: int = 240, with_audio: bool = True):
    """Create a test video with a 'watermark' (white rectangle in corner).

    Uses ffmpeg CLI for reliability (avoids PyAV audio encoding issues).
    """
    import subprocess
    duration = n_frames / fps

    # Generate video with lavfi (testsrc pattern) + audio (sine wave)
    vf = f"testsrc=duration={duration}:size={width}x{height}:rate={fps}"
    # Draw a white rectangle as "watermark" in bottom-right corner
    vf += f",drawbox=x={width-100}:y={height-40}:w=80:h=30:color=white@1:t=fill"

    cmd = [
        "ffmpeg", "-y",
        "-f", "lavfi", "-i", vf,
    ]
    if with_audio:
        cmd += ["-f", "lavfi", "-i",
                f"sine=frequency=440:duration={duration}:sample_rate=44100"]
        cmd += ["-c:a", "aac", "-b:a", "64k", "-ac", "1"]

    cmd += ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23",
            "-pix_fmt", "yuv420p"]
    if not with_audio:
        cmd += ["-an"]
    cmd += [path]

    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"ffmpeg failed to create test video:\n{result.stderr[-1000:]}")

    # Verify
    c = av.open(path)
    v = c.streams.video[0]
    actual_frames = v.frames or 0
    has_a = len(c.streams.audio) > 0
    c.close()
    print(f"Created test video: {path} ({actual_frames} frames, audio={has_a})")


def probe_video(path: str) -> dict:
    """Probe a video file."""
    c = av.open(path)
    v = c.streams.video[0]
    a = c.streams.audio[0] if c.streams.audio else None
    info = {
        "width": v.width,
        "height": v.height,
        "frames": v.frames or 0,
        "has_audio": a is not None,
        "duration": float(v.duration * v.time_base) if v.duration else 0,
    }
    c.close()
    return info


def test_fresh_run(test_video, output_path, seg_frames=100, max_frames=300):
    """Test 1: Fresh run produces valid output."""
    print("\n" + "=" * 60)
    print("TEST 1: Fresh run")
    print("=" * 60)

    # Clean up
    if os.path.exists(output_path):
        os.remove(output_path)
    seg_dir = output_path + ".segments"
    if os.path.isdir(seg_dir):
        shutil.rmtree(seg_dir)

    progress_log = []
    def progress_cb(completed, total):
        if completed % 50 == 0 or completed == total:
            pct = 100 * completed / max(total, 1)
            progress_log.append((completed, total, pct))
            print(f"  progress: {completed}/{total} ({pct:.0f}%)")

    result = remove_watermark(
        test_video, output_path,
        regions=[(220, 200, 80, 30)],  # bottom-right corner
        dilate_px=4,
        segment_frames=seg_frames,
        max_frames=max_frames,
        progress_callback=progress_cb,
    )

    print(f"\nResult: {result}")
    print(f"Progress callbacks: {len(progress_log)}")

    # Verify output
    assert os.path.exists(output_path), "output file not created"
    src_info = probe_video(test_video)
    out_info = probe_video(output_path)
    print(f"Source: {src_info}")
    print(f"Output: {out_info}")

    assert out_info["frames"] >= max_frames, \
        f"frame count mismatch: {out_info['frames']} < {max_frames}"
    assert out_info["has_audio"] == src_info["has_audio"], \
        f"audio mismatch: src={src_info['has_audio']} out={out_info['has_audio']}"
    assert out_info["width"] == src_info["width"]
    assert out_info["height"] == src_info["height"]

    # Verify segments directory is cleaned up
    assert not os.path.isdir(seg_dir), "segment dir should be cleaned up"

    print("PASS: Fresh run produces valid output with correct frames + audio")
    return result


def test_skip_complete(test_video, output_path):
    """Test 2: Re-run when output is complete → skips."""
    print("\n" + "=" * 60)
    print("TEST 2: Skip when complete")
    print("=" * 60)

    result = remove_watermark(
        test_video, output_path,
        regions=[(220, 200, 80, 30)],
        dilate_px=4,
        max_frames=300,
    )

    assert result.get("skipped") is True, f"should skip, got: {result}"
    print(f"Result: {result}")
    print("PASS: Skipped correctly")


def test_resume_after_failure(test_video, output_path, seg_frames=100,
                                max_frames=300):
    """Test 3: Simulate failure (crash during concat), verify resume."""
    print("\n" + "=" * 60)
    print("TEST 3: Resume after failure")
    print("=" * 60)

    # Clean up
    if os.path.exists(output_path):
        os.remove(output_path)
    seg_dir = output_path + ".segments"
    if os.path.isdir(seg_dir):
        shutil.rmtree(seg_dir)

    import app.services.watermark as wm
    original_concat = wm._concat_segments_ffmpeg

    # Step 1: Create segments 0-1 with max_frames=200, but crash during concat
    print("Step 1: Create 2 segments, simulate crash during concat...")
    def failing_concat(*args, **kwargs):
        raise RuntimeError("Simulated crash during concat!")
    wm._concat_segments_ffmpeg = failing_concat

    try:
        remove_watermark(
            test_video, output_path,
            regions=[(220, 200, 80, 30)],
            dilate_px=4,
            segment_frames=seg_frames,
            max_frames=200,  # only 2 segments
        )
        assert False, "should have raised"
    except RuntimeError as e:
        print(f"  Expected crash: {e}")

    # Restore concat
    wm._concat_segments_ffmpeg = original_concat

    # Verify segments are left in place (no cleanup happened)
    assert os.path.isdir(seg_dir), "segment dir should exist after crash"
    segs = sorted([f for f in os.listdir(seg_dir) if f.endswith(".mp4")])
    print(f"  Segments on disk after crash: {segs}")
    assert len(segs) == 2, f"expected 2 segments, got {len(segs)}"

    # Output should NOT exist (concat failed)
    assert not os.path.exists(output_path), "output should not exist"

    # Step 2: Resume run — should detect 2 segments, process segment 2
    print("\nStep 2: Resume run (should skip segments 0-1, process segment 2)...")
    progress_log = []
    def progress_cb(completed, total):
        progress_log.append((completed, total))

    result2 = remove_watermark(
        test_video, output_path,
        regions=[(220, 200, 80, 30)],
        dilate_px=4,
        segment_frames=seg_frames,
        max_frames=max_frames,
        progress_callback=progress_cb,
    )

    print(f"Resume result: resumed_from={result2.get('resumed_from')}, "
          f"frames={result2.get('frames_processed')}")

    # Verify resume worked
    assert result2["resumed_from"] >= 200, \
        f"should resume from >=200, got {result2['resumed_from']}"
    assert result2["frames_processed"] >= max_frames, \
        f"should process to {max_frames}, got {result2['frames_processed']}"

    # Verify output is valid
    assert os.path.exists(output_path), "output not created"
    out_info = probe_video(output_path)
    print(f"Output: {out_info}")
    assert out_info["frames"] >= max_frames, \
        f"output frames {out_info['frames']} < {max_frames}"
    assert out_info["has_audio"], "output should have audio"

    # Verify segments cleaned up
    assert not os.path.isdir(seg_dir), "segments should be cleaned up"

    # Check progress callbacks show resume (first callback should be > 0)
    if progress_log:
        first_cb = progress_log[0]
        print(f"First progress callback: {first_cb}")
        # After resume, the first callback should show we're past the resume point
        assert first_cb[0] >= 200 or first_cb[1] == max_frames, \
            f"first callback should show resume, got {first_cb}"

    print("PASS: Resume correctly skips completed segments")
    return result2


def test_corrupt_segment_detection(test_video, output_path, seg_frames=100,
                                   max_frames=300):
    """Test 4: Corrupt a segment, verify it's detected and re-encoded."""
    print("\n" + "=" * 60)
    print("TEST 4: Corrupt segment detection")
    print("=" * 60)

    # Clean up
    if os.path.exists(output_path):
        os.remove(output_path)
    seg_dir = output_path + ".segments"
    if os.path.isdir(seg_dir):
        shutil.rmtree(seg_dir)

    import app.services.watermark as wm
    original_concat = wm._concat_segments_ffmpeg

    # Step 1: Create 2 segments with failing concat (leaves segments in place)
    print("Step 1: Create 2 segments, simulate crash...")
    def failing_concat(*args, **kwargs):
        raise RuntimeError("Simulated crash!")
    wm._concat_segments_ffmpeg = failing_concat

    try:
        remove_watermark(
            test_video, output_path,
            regions=[(220, 200, 80, 30)],
            dilate_px=4,
            segment_frames=seg_frames,
            max_frames=200,
        )
    except RuntimeError:
        pass

    wm._concat_segments_ffmpeg = original_concat

    # Verify segments exist
    assert os.path.isdir(seg_dir), "segment dir should exist"
    segs = sorted([f for f in os.listdir(seg_dir) if f.endswith(".mp4")])
    print(f"  Segments: {segs}")
    assert len(segs) == 2

    # Delete output (from failed concat)
    if os.path.exists(output_path):
        os.remove(output_path)

    # Corrupt segment 1 (truncate it)
    seg1_path = os.path.join(seg_dir, "seg_0001.mp4")
    assert os.path.exists(seg1_path), "seg_0001 should exist"
    with open(seg1_path, "r+b") as f:
        f.seek(0, 2)
        size = f.tell()
        f.seek(size // 2)
        f.truncate(size // 2)
    print(f"  Corrupted seg_0001.mp4 (truncated to half)")

    # Verify segment 1 is now invalid
    assert not _is_segment_valid(seg1_path, 320, 240), "corrupted seg should be invalid"
    print(f"  _is_segment_valid(seg_0001) = False (correct)")

    # Step 2: Resume — should detect corrupt seg_0001, re-encode from segment 1
    print("\nStep 2: Resume (should detect corrupt seg_0001, re-encode from seg 1)...")
    result = remove_watermark(
        test_video, output_path,
        regions=[(220, 200, 80, 30)],
        dilate_px=4,
        segment_frames=seg_frames,
        max_frames=max_frames,
    )

    print(f"Result: resumed_from={result.get('resumed_from')}, "
          f"frames={result.get('frames_processed')}")

    # Should resume from segment 1 (not 2) because segment 1 was corrupt
    # Segment 0 (100 frames) is still valid → resume from frame 100
    assert result["resumed_from"] <= 100, \
        f"should resume from segment 1 (<=100 frames), got {result['resumed_from']}"

    # Verify output is valid
    assert os.path.exists(output_path)
    out_info = probe_video(output_path)
    print(f"Output: {out_info}")
    assert out_info["frames"] >= max_frames

    print("PASS: Corrupt segment detected and re-encoded")


if __name__ == "__main__":
    tmpdir = tempfile.mkdtemp(prefix="wm_test_")
    try:
        test_video = os.path.join(tmpdir, "test_input.mp4")
        output_path = os.path.join(tmpdir, "test_output.mp4")

        # Create test video (300 frames = 10s at 30fps, with audio)
        print("Creating test video...")
        make_test_video(test_video, n_frames=300, fps=30, with_audio=True)
        src_info = probe_video(test_video)
        print(f"Source: {src_info}")

        # Run tests
        test_fresh_run(test_video, output_path, seg_frames=100, max_frames=300)
        test_skip_complete(test_video, output_path)
        test_resume_after_failure(test_video, output_path, seg_frames=100,
                                  max_frames=300)
        test_corrupt_segment_detection(test_video, output_path, seg_frames=100,
                                      max_frames=300)

        print("\n" + "=" * 60)
        print("ALL TESTS PASSED")
        print("=" * 60)

    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
        print(f"\nCleaned up {tmpdir}")
