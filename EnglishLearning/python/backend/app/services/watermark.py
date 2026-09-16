"""Video watermark removal via image inpainting.

Two backends are supported:

  * ``opencv`` (default) — uses ``cv2.inpaint`` with the TELEA fast-
    marching algorithm. Pure CPU, no model download, fast. Good enough
    for static corner watermarks over slides/low-texture backgrounds.
  * ``lama`` (optional) — uses LaMa via IOPaint. Higher quality on busy
    backgrounds but pulls in PyTorch + ~237 MB model weights. Lazy-
    imported so the sandbox stays lightweight when not used.

Static watermarks are the same in every frame, so a spatial-only
per-frame inpaint is sufficient. Temporal methods (ProPainter, E2FGVI)
would give better results only for *moving* watermarks or busy moving
backgrounds — neither of which we have. See
``docs/VIDEO_WATERMARK_INPAINTING.md`` for the full design rationale.

Pipeline::

    video → decode frames (PyAV) → build mask → cv2.inpaint per frame
          → re-encode video segments (H.264, one MP4 per segment)
          → lossless concat (ffmpeg -c copy) + mux audio

The video is processed in **segments** (default 300 frames each, ~10s at
30fps). Each segment is encoded to its own complete MP4 file. This
enables **resume after failure**: if the process is interrupted,
completed segments are preserved and skipped on retry. The final output
is produced by losslessly concatenating all segments with ffmpeg's
``-f concat -c copy`` demuxer (no re-encode — zero quality loss from
concatenation).

Audio is muxed from the original source video during the concat step
(re-encoded to AAC at 128k if the source codec is different).
"""

from __future__ import annotations

import logging
import os
import shutil
import subprocess
from typing import List, Optional, Sequence, Tuple, Union

import av
import cv2
import numpy as np

logger = logging.getLogger(__name__)

# Frames per segment. ~10s at 30fps. Balance between resume granularity
# (smaller = less work lost on failure) and file management overhead
# (fewer files = faster concat). 300 is a good default.
DEFAULT_SEGMENT_FRAMES = 300

# Type alias for a watermark region: (x, y, w, h) in pixels, top-left origin.
Region = Tuple[int, int, int, int]
RegionSpec = Union[Sequence[Region], str]  # list of regions, or "auto"


def _build_mask_from_regions(
    regions: Sequence[Region],
    width: int,
    height: int,
    dilate_px: int = 8,
) -> np.ndarray:
    """Construct a binary mask (0/255, uint8) from rectangular regions.

    The mask is white (255) where the watermark lives, black elsewhere.
    Each region is dilated by ``dilate_px`` so antialiased edges of the
    watermark text/logo are also covered — without this, a 1-px gap
    between the user's rectangle and the actual text edge leaves a
    visible halo after inpainting.
    """
    mask = np.zeros((height, width), dtype=np.uint8)
    for (x, y, w, h) in regions:
        x = max(0, int(x))
        y = max(0, int(y))
        x2 = min(width, int(x + w))
        y2 = min(height, int(y + h))
        if x2 <= x or y2 <= y:
            continue
        mask[y:y2, x:x2] = 255
    if dilate_px > 0:
        kernel = cv2.getStructuringElement(
            cv2.MORPH_ELLIPSE, (dilate_px * 2 + 1, dilate_px * 2 + 1)
        )
        mask = cv2.dilate(mask, kernel, iterations=1)
    return mask


def _load_mask_image(mask_path: str, width: int, height: int,
                     dilate_px: int = 8) -> np.ndarray:
    """Load a binary mask PNG and resize it to the video frame size."""
    mask = cv2.imread(mask_path, cv2.IMREAD_GRAYSCALE)
    if mask is None:
        raise FileNotFoundError(f"mask image not found: {mask_path}")
    if mask.shape != (height, width):
        mask = cv2.resize(mask, (width, height), interpolation=cv2.INTER_NEAREST)
    # Any non-zero pixel becomes 255 (white = watermark).
    mask = (mask > 127).astype(np.uint8) * 255
    if dilate_px > 0:
        kernel = cv2.getStructuringElement(
            cv2.MORPH_ELLIPSE, (dilate_px * 2 + 1, dilate_px * 2 + 1)
        )
        mask = cv2.dilate(mask, kernel, iterations=1)
    return mask


def detect_watermark_regions(
    video_path: str,
    sample_frames: int = 16,
    std_threshold: float = 15.0,
    min_brightness: int = 30,
    min_area: int = 200,
    dilate_px: int = 8,
) -> List[Region]:
    """Auto-detect static watermark regions.

    Strategy: sample ``sample_frames`` frames evenly across the video,
    compute per-pixel std across frames. Watermark pixels are *static*
    (std < threshold) AND have non-trivial brightness (> min_brightness,
    so we exclude plain black background corners).

    Returns a list of (x, y, w, h) bounding boxes for each detected
    watermark, after morphological cleanup.
    """
    container = av.open(video_path)
    stream = container.streams.video[0]
    total = stream.frames or 0
    duration_sec = float(stream.duration * stream.time_base) if stream.duration else 0

    # Evenly-spaced frame indices
    if total > 0:
        target_pts = np.linspace(0, total - 1, sample_frames).astype(int)
    else:
        # Fallback: use time-based seeking
        target_pts = np.linspace(0, duration_sec, sample_frames, endpoint=False)

    frames = []
    frame_idx = 0
    for frame in container.decode(video=0):
        if frame_idx in target_pts:
            arr = frame.to_ndarray(format="bgr24")
            frames.append(arr)
            if len(frames) >= len(target_pts):
                break
        frame_idx += 1
    container.close()

    if len(frames) < 4:
        logger.warning("detect: only %d frames sampled, need ≥4", len(frames))
        return []

    stack = np.stack(frames).astype(np.float32)
    std_img = stack.std(axis=0).max(axis=2)  # max std across BGR
    mean_img = stack.mean(axis=0).max(axis=2)  # max brightness across BGR
    h, w = std_img.shape

    # Static + bright pixels are candidate watermark pixels
    static = std_img < std_threshold
    bright = mean_img > min_brightness
    candidate = (static & bright).astype(np.uint8) * 255

    # Morphological open to drop speckle, then close to fill text glyphs
    k_open = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (5, 5))
    k_close = cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (15, 15))
    candidate = cv2.morphologyEx(candidate, cv2.MORPH_OPEN, k_open)
    candidate = cv2.morphologyEx(candidate, cv2.MORPH_CLOSE, k_close)

    # Dilate to cover antialiased edges
    if dilate_px > 0:
        k_dil = cv2.getStructuringElement(
            cv2.MORPH_ELLIPSE, (dilate_px * 2 + 1, dilate_px * 2 + 1)
        )
        candidate = cv2.dilate(candidate, k_dil, iterations=1)

    # Connected components → bounding boxes
    n_labels, _labels, stats, _centroids = cv2.connectedComponentsWithStats(
        candidate, connectivity=8
    )
    regions: List[Region] = []
    for i in range(1, n_labels):  # 0 = background
        x, y, w_i, h_i, area = stats[i]
        if area < min_area:
            continue
        regions.append((int(x), int(y), int(w_i), int(h_i)))

    logger.info("detect: %d watermark regions found: %s", len(regions), regions)
    return regions


def _inpaint_frame_opencv(frame: np.ndarray, mask: np.ndarray,
                          radius: int) -> np.ndarray:
    """Inpaint one frame with cv2.inpaint Navier-Stokes.

    Uses INPAINT_NS (Navier-Stokes based) instead of INPAINT_TELEA
    because it is ~1.4x faster (measured 37.5 vs 26.0 fps on 1080p)
    and produces comparable quality for static watermarks. NS is
    particularly good at reconstructing thin lines and text.
    """
    return cv2.inpaint(frame, mask, inpaintRadius=radius,
                       flags=cv2.INPAINT_NS)


def _inpaint_frame_lama(frame: np.ndarray, mask: np.ndarray,
                         _radius: int) -> np.ndarray:
    """Inpaint one frame with LaMa via IOPaint.

    Lazy-imported so the opencv backend works without IOPaint installed.
    """
    from iopaint.model.lama import Lama  # type: ignore
    from iopaint.runtime import ModelManager  # type: ignore

    # The model manager is cached on the function object so the model is
    # loaded only once per process.
    if not hasattr(_inpaint_frame_lama, "_manager"):
        _inpaint_frame_lama._manager = ModelManager(name="lama", device="cpu")
    mgr = _inpaint_frame_lama._manager
    # IOPaint expects RGB.
    rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
    out_rgb = mgr(rgb, mask, hd_strategy="original")
    return cv2.cvtColor(out_rgb, cv2.COLOR_RGB2BGR)


_ENGINES = {
    "opencv": _inpaint_frame_opencv,
    "lama": _inpaint_frame_lama,
}


def remove_watermark(
    video_path: str,
    output_path: str,
    regions: RegionSpec = "auto",
    *,
    engine: str = "opencv",
    dilate_px: int = 8,
    inpaint_radius: int = 3,
    sample_frames: int = 16,
    mask_path: Optional[str] = None,
    workers: Optional[int] = None,
    max_frames: Optional[int] = None,
    overwrite: bool = False,
    progress_callback=None,
    segment_frames: int = DEFAULT_SEGMENT_FRAMES,
) -> dict:
    """Remove a static watermark from ``video_path``.

    Args:
        video_path: input video file (any container PyAV can decode).
        output_path: output ``.mp4`` path.
        regions: ``"auto"`` (detect via frame differencing) or a list of
            ``(x, y, w, h)`` rectangles in pixels. Ignored if ``mask_path``.
        engine: ``"opencv"`` (default, CPU, fast) or ``"lama"``.
        dilate_px: expand the mask by this many pixels.
        inpaint_radius: cv2.inpaint radius (opencv engine only).
        sample_frames: number of frames to sample for auto-detection.
        mask_path: optional binary mask PNG (white = remove).
        workers: (unused, kept for API compatibility).
        max_frames: process only the first N frames (for testing).
        overwrite: if True, overwrite an existing output file.
        progress_callback: optional callable(completed: int, total: int).
        segment_frames: number of frames per segment file. Smaller values
            give finer resume granularity (less work lost on failure) but
            more files to manage. Default 300 (~10s at 30fps).

    Returns:
        Dict with ``frames_processed``, ``resumed_from``, ``duration_s``,
        ``regions``, ``engine``, ``output_path``.

    Resume semantics:
        Processing is done in **segments** of ``segment_frames`` frames.
        Each segment is encoded to its own complete MP4 file in a
        ``.segments/`` directory. If processing is interrupted:

        1. Completed segments are preserved and **skipped** on retry.
        2. The last in-progress segment (if corrupt) is detected and
           re-encoded from scratch.
        3. Remaining segments are processed.
        4. All segments are losslessly concatenated using ffmpeg's
           concat demuxer (``-c copy``), and the original audio is muxed.

        Example: if watermark removal fails at 95% (segment 22 of 23
        complete), the retry resumes from segment 22 — only the last ~5%
        of frames are re-processed. The 22 completed segments are reused.

        This uses the industry-standard segment-encode + lossless-concat
        pattern (same as ffmpeg's ``-f segment`` muxer + ``-f concat``
        demuxer). PyAV handles per-frame inpainting + per-segment encoding;
        ffmpeg CLI handles the final lossless concatenation.
    """
    if engine not in _ENGINES:
        raise ValueError(f"unknown engine: {engine!r} "
                         f"(choose from {list(_ENGINES)})")
    inpaint_fn = _ENGINES[engine]

    # ── Resume check: is the final output already complete? ────────────
    if os.path.exists(output_path) and not overwrite:
        if _is_output_complete(video_path, output_path, max_frames):
            logger.info("skip: %s already complete", output_path)
            if progress_callback:
                progress_callback(1, 1)
            return {"output_path": output_path, "skipped": True}
        else:
            logger.warning("incomplete output detected, deleting: %s",
                           output_path)
            os.remove(output_path)

    if overwrite:
        # Clean up any old segments
        seg_dir = output_path + ".segments"
        if os.path.isdir(seg_dir):
            shutil.rmtree(seg_dir, ignore_errors=True)

    # ── Probe video properties ──────────────────────────────────────────
    probe = av.open(video_path)
    v_stream = probe.streams.video[0]
    width = v_stream.width
    height = v_stream.height
    total_frames = v_stream.frames or 0
    has_audio = len(probe.streams.audio) > 0
    in_rate = v_stream.codec_context.rate  # Fraction
    probe.close()
    logger.info("video: %dx%d, %d frames, audio=%s",
                width, height, total_frames, has_audio)

    # ── Build mask ──────────────────────────────────────────────────────
    if mask_path is not None:
        mask = _load_mask_image(mask_path, width, height, dilate_px)
        detected_regions: List[Region] = []
    elif regions == "auto":
        detected_regions = detect_watermark_regions(
            video_path, sample_frames=sample_frames, dilate_px=dilate_px,
        )
        if not detected_regions:
            logger.warning("no watermark detected; copying video unchanged")
            _copy_video_unchanged(video_path, output_path)
            return {"output_path": output_path, "regions": [],
                    "frames_processed": 0, "engine": engine,
                    "resumed_from": 0}
        mask = _build_mask_from_regions(detected_regions, width, height, 0)
    else:
        detected_regions = list(regions)  # type: ignore[arg-type]
        mask = _build_mask_from_regions(detected_regions, width, height, 0)

    mask_area = int((mask > 0).sum())
    logger.info("mask: %d pixels (%.2f%% of frame)",
                mask_area, 100 * mask_area / (width * height))

    # ── Determine frame count ───────────────────────────────────────────
    effective_total = (min(max_frames, total_frames)
                        if max_frames and total_frames > 0
                        else max_frames if max_frames else total_frames)
    if effective_total == 0:
        # Unknown frame count — can't segment. Fall back to single-pass.
        logger.warning("unknown frame count, using single-pass mode")
        return _remove_watermark_single_pass(
            video_path, output_path, inpaint_fn, mask, inpaint_radius,
            in_rate, width, height, has_audio, detected_regions, engine,
            max_frames, progress_callback,
        )

    # ── Segment-based processing ─────────────────────────────────────────
    n_segments = (effective_total + segment_frames - 1) // segment_frames
    seg_dir = output_path + ".segments"
    os.makedirs(seg_dir, exist_ok=True)

    # Scan for completed segments (resume)
    start_seg, resumed_frames = _scan_completed_segments(
        seg_dir, segment_frames, n_segments, effective_total, width, height
    )

    if start_seg >= n_segments:
        logger.info("all %d segments already complete, concatenating...",
                    n_segments)
    elif resumed_frames > 0:
        logger.info("resuming from segment %d/%d (frame %d/%d, %.0f%% done)",
                    start_seg + 1, n_segments, resumed_frames,
                    effective_total, 100 * resumed_frames / effective_total)
    else:
        logger.info("starting fresh: %d segments × %d frames = %d total",
                    n_segments, segment_frames, effective_total)

    # Open input container
    in_container = av.open(video_path)
    in_v_stream = in_container.streams.video[0]

    # If resuming, try to seek near the resume point for speed
    skip_pts_threshold = _seek_to_resume_point(
        in_container, in_v_stream, resumed_frames, in_rate
    )
    # Re-create the decode generator AFTER seeking
    frame_gen = in_container.decode(in_v_stream)

    import time
    t0 = time.perf_counter()
    frames_done = resumed_frames

    # Process segments
    for seg_idx in range(start_seg, n_segments):
        seg_start = seg_idx * segment_frames
        seg_end = min(seg_start + segment_frames, effective_total)
        seg_nframes = seg_end - seg_start

        seg_path = os.path.join(seg_dir, f"seg_{seg_idx:04d}.mp4")
        if os.path.exists(seg_path):
            os.remove(seg_path)  # stale/corrupt, start fresh

        out_container = av.open(seg_path, "w")
        out_v_stream = out_container.add_stream("h264", rate=in_rate)
        out_v_stream.width = width
        out_v_stream.height = height
        out_v_stream.pix_fmt = "yuv420p"
        out_v_stream.options = {"preset": "veryfast", "crf": "20"}

        seg_frame_count = 0
        while seg_frame_count < seg_nframes:
            try:
                frame = next(frame_gen)
            except StopIteration:
                logger.warning("stream ended at segment %d, frame %d/%d",
                               seg_idx, seg_frame_count, seg_nframes)
                break

            # Skip frames before resume point (after seek)
            if skip_pts_threshold >= 0:
                if frame.pts is not None and frame.pts < skip_pts_threshold:
                    continue
                skip_pts_threshold = -1
                logger.info("resume: reached target frame, starting inpaint")

            # Inpaint + encode
            arr = frame.to_ndarray(format="bgr24")
            cleaned = inpaint_fn(arr, mask, inpaint_radius)
            out_frame = av.VideoFrame.from_ndarray(cleaned, format="bgr24")
            for packet in out_v_stream.encode(out_frame):
                out_container.mux(packet)

            seg_frame_count += 1
            frames_done += 1

            if progress_callback and (frames_done % 50 == 0 or
                                       frames_done == effective_total):
                progress_callback(frames_done, effective_total)

        # Flush encoder
        for packet in out_v_stream.encode():
            out_container.mux(packet)
        out_container.close()

        logger.info("segment %d/%d done: %d frames -> %s",
                    seg_idx + 1, n_segments, seg_frame_count,
                    os.path.basename(seg_path))

        if seg_frame_count == 0:
            # No frames — delete empty segment and stop
            if os.path.exists(seg_path):
                os.remove(seg_path)
            break

    in_container.close()

    # ── Concatenate segments + mux audio (ffmpeg, lossless) ─────────────
    _concat_segments_ffmpeg(seg_dir, n_segments, output_path,
                            video_path, has_audio)

    # Cleanup segment directory
    shutil.rmtree(seg_dir, ignore_errors=True)

    elapsed = time.perf_counter() - t0
    logger.info("done: %d frames in %.1fs -> %s",
                frames_done, elapsed, output_path)
    if progress_callback:
        progress_callback(frames_done, effective_total)
    return {
        "output_path": output_path,
        "regions": detected_regions,
        "frames_processed": frames_done,
        "resumed_from": resumed_frames,
        "duration_s": elapsed,
        "engine": engine,
    }


def _seek_to_resume_point(container, v_stream, target_frame, in_rate):
    """Seek the container near ``target_frame`` for fast resume.

    Returns a PTS threshold: frames with ``pts < threshold`` should be
    skipped (they're before the resume point). Returns -1 if no
    skipping is needed (target_frame == 0 or seek failed).
    """
    if target_frame <= 0:
        return -1

    fps = float(in_rate)
    time_base = float(v_stream.time_base) if v_stream.time_base else 0.0
    if time_base <= 0 or fps <= 0:
        # Can't compute timestamps, will decode-skip
        logger.info("seek: can't compute timestamps (time_base=%s, fps=%s), "
                    "decode-skipping %d frames", v_stream.time_base, in_rate,
                    target_frame)
        _decode_skip(container.streams.video[0], container, target_frame)
        return -1

    target_time = target_frame / fps
    target_pts = int(target_time / time_base)
    # Seek 2 seconds before target for keyframe alignment
    seek_time = max(0.0, target_time - 2.0)
    seek_pts = int(seek_time / time_base)

    try:
        container.seek(seek_pts, stream=v_stream)
        logger.info("seeked to %.1fs (target: %.1fs, skipping ~%d frames)",
                    seek_time, target_time, target_frame)
        return target_pts
    except Exception as e:
        logger.warning("seek failed (%s), decode-skipping %d frames",
                       e, target_frame)
        _decode_skip(v_stream, container, target_frame)
        return -1


def _decode_skip(v_stream, container, n_frames):
    """Decode and discard ``n_frames`` frames (fallback when seek fails)."""
    import time
    t0 = time.perf_counter()
    gen = container.decode(v_stream)
    for i in range(n_frames):
        try:
            next(gen)
        except StopIteration:
            break
        if (i + 1) % 1000 == 0:
            elapsed = time.perf_counter() - t0
            logger.info("  skip: %d/%d (%.0f fps)",
                        i + 1, n_frames, (i + 1) / max(elapsed, 0.001))


def _scan_completed_segments(seg_dir: str, segment_frames: int,
                              n_segments: int, effective_total: int,
                              width: int, height: int) -> Tuple[int, int]:
    """Scan ``seg_dir`` for completed segments.

    Returns ``(start_seg, resumed_frames)`` where ``start_seg`` is the
    index of the first segment that needs processing, and
    ``resumed_frames`` is the number of frames already done.

    Corrupt segments are deleted. The scan finds the first gap in the
    sequence — all segments before the gap are assumed complete.
    """
    if not os.path.isdir(seg_dir):
        return 0, 0

    # Collect valid segment indices
    valid_segs = {}
    for fname in os.listdir(seg_dir):
        if not (fname.startswith("seg_") and fname.endswith(".mp4")):
            continue
        try:
            seg_idx = int(fname[4:8])
        except ValueError:
            continue
        seg_path = os.path.join(seg_dir, fname)
        if _is_segment_valid(seg_path, width, height):
            valid_segs[seg_idx] = seg_path
        else:
            logger.warning("segment %d is corrupt/incomplete, deleting: %s",
                           seg_idx, fname)
            os.remove(seg_path)

    if not valid_segs:
        return 0, 0

    # Find the first gap in the sequence [0, 1, 2, ...]
    max_valid = max(valid_segs.keys())
    start_seg = n_segments  # default: all done
    for i in range(max_valid + 2):
        if i not in valid_segs:
            start_seg = i
            break

    # Delete any segments after the first gap (they're orphaned)
    for idx in list(valid_segs.keys()):
        if idx > start_seg:
            os.remove(valid_segs[idx])

    resumed_frames = min(start_seg * segment_frames, effective_total)
    return start_seg, resumed_frames


def _is_segment_valid(seg_path: str, width: int, height: int) -> bool:
    """Quick check: can the segment be opened with the right dimensions?

    A corrupt/truncated MP4 (encoder didn't flush, moov atom missing)
    will fail to open. This catches the common failure mode.
    """
    if not os.path.exists(seg_path) or os.path.getsize(seg_path) == 0:
        return False
    try:
        c = av.open(seg_path)
        v = c.streams.video[0]
        ok = v.width == width and v.height == height and (v.frames or 0) > 0
        c.close()
        return ok
    except Exception:
        return False


def _concat_segments_ffmpeg(seg_dir: str, n_segments: int,
                             output_path: str, video_path: str,
                             has_audio: bool) -> None:
    """Concatenate segment files using ffmpeg's concat demuxer (lossless).

    Uses ``-c copy`` for video (no re-encode — lossless). Audio is muxed
    from the original source video (re-encoded to AAC if needed).
    """
    # Build concat list (absolute paths)
    list_path = os.path.join(seg_dir, "concat_list.txt")
    with open(list_path, "w") as f:
        for i in range(n_segments):
            seg_path = os.path.join(seg_dir, f"seg_{i:04d}.mp4")
            if os.path.exists(seg_path):
                f.write(f"file '{seg_path}'\n")

    # Build ffmpeg command
    if has_audio:
        # Concat video segments (copy) + mux audio from source (re-encode to AAC)
        cmd = [
            "ffmpeg", "-y",
            "-f", "concat", "-safe", "0", "-i", list_path,
            "-i", video_path,
            "-c:v", "copy",
            "-c:a", "aac", "-b:a", "128k",
            "-map", "0:v", "-map", "1:a?",
            "-shortest",
            output_path,
        ]
    else:
        # Video-only concat
        cmd = [
            "ffmpeg", "-y",
            "-f", "concat", "-safe", "0", "-i", list_path,
            "-c:v", "copy",
            "-map", "0:v",
            output_path,
        ]

    logger.info("concatenating %d segments with ffmpeg...", n_segments)
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        # Leave segments in place for debugging / retry
        raise RuntimeError(
            f"ffmpeg concat failed (exit {result.returncode}):\n"
            f"{result.stderr[-2000:]}"
        )
    logger.info("concat done: %s", output_path)


def _remove_watermark_single_pass(
    video_path, output_path, inpaint_fn, mask, inpaint_radius,
    in_rate, width, height, has_audio, detected_regions, engine,
    max_frames, progress_callback,
) -> dict:
    """Fallback: single-pass encoding (no segments) when frame count is unknown.

    Uses temp-file + atomic rename. No resume capability.
    """
    import time
    t0 = time.perf_counter()
    partial_path = output_path + ".partial.mp4"
    if os.path.exists(partial_path):
        os.remove(partial_path)

    in_container = av.open(video_path)
    audio_container = av.open(video_path) if has_audio else None
    in_v_stream = in_container.streams.video[0]

    out_container = av.open(partial_path, "w")
    out_v_stream = out_container.add_stream("h264", rate=in_rate)
    out_v_stream.width = width
    out_v_stream.height = height
    out_v_stream.pix_fmt = "yuv420p"
    out_v_stream.options = {"preset": "veryfast", "crf": "20"}

    in_a_stream = audio_container.streams.audio[0] if audio_container else None
    out_a_stream = None
    if in_a_stream is not None:
        in_cc = in_a_stream.codec_context
        out_a_stream = out_container.add_stream("aac", rate=in_cc.sample_rate)
        out_a_stream.layout = "mono"
        out_a_stream.bit_rate = 128_000

    frames_done = 0
    effective_total = max_frames or 0
    for frame in in_container.decode(in_v_stream):
        if max_frames is not None and frames_done >= max_frames:
            break
        arr = frame.to_ndarray(format="bgr24")
        cleaned = inpaint_fn(arr, mask, inpaint_radius)
        out_frame = av.VideoFrame.from_ndarray(cleaned, format="bgr24")
        for packet in out_v_stream.encode(out_frame):
            out_container.mux(packet)
        frames_done += 1
        if progress_callback and (frames_done % 50 == 0):
            progress_callback(frames_done, effective_total)

    for packet in out_v_stream.encode():
        out_container.mux(packet)

    if in_a_stream is not None and out_a_stream is not None:
        for frame in audio_container.decode(in_a_stream):
            for packet in out_a_stream.encode(frame):
                packet.stream = out_a_stream
                out_container.mux(packet)
        for packet in out_a_stream.encode():
            packet.stream = out_a_stream
            out_container.mux(packet)

    in_container.close()
    if audio_container is not None:
        audio_container.close()
    out_container.close()

    os.rename(partial_path, output_path)

    elapsed = time.perf_counter() - t0
    logger.info("done (single-pass): %d frames in %.1fs -> %s",
                frames_done, elapsed, output_path)
    return {
        "output_path": output_path,
        "regions": detected_regions,
        "frames_processed": frames_done,
        "resumed_from": 0,
        "duration_s": elapsed,
        "engine": engine,
    }


def _is_output_complete(video_path: str, output_path: str,
                        max_frames: Optional[int] = None) -> bool:
    """Check if a final output file is complete (not corrupt)."""
    if not os.path.exists(output_path) or os.path.getsize(output_path) == 0:
        return False
    try:
        src = av.open(video_path)
        src_frames = src.streams.video[0].frames or 0
        src_has_audio = len(src.streams.audio) > 0
        src.close()

        out = av.open(output_path)
        out_v = out.streams.video[0]
        out_frames = out_v.frames or 0
        out_has_audio = len(out.streams.audio) > 0
        out.close()
    except Exception:
        return False

    expected = min(max_frames, src_frames) if max_frames else src_frames
    if expected == 0:
        return True
    return out_frames >= expected and (out_has_audio or not src_has_audio)


def _copy_video_unchanged(video_path: str, output_path: str) -> None:
    """Copy a video file as-is (used when no watermark is detected)."""
    in_container = av.open(video_path)
    out_container = av.open(output_path, "w")
    for stream in in_container.streams:
        out_stream = out_container.add_stream(template=stream)
        for packet in in_container.demux(stream):
            if packet.dts is None:
                continue
            out_container.mux(packet)
    in_container.close()
    out_container.close()
