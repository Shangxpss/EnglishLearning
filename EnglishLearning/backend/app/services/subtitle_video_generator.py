"""Subtitle-to-Video generator.

Implements the industry-standard "Generate & Time-Stretch" pipeline used by AI
video dubbing tools (Rask.ai, ElevenLabs Dubbing, HeyGen, etc.):

    1. Parse:    read an .srt / .vtt subtitle file to obtain the text and the
                 exact start/end times for each block.
    2. Generate: synthesise TTS audio for each subtitle block (Microsoft Edge
                 TTS via ``edge-tts`` -- no API key required).
    3. Stretch:  pitch-preserving time-stretch (``audiostretchy`` with a
                 ``pydub`` + ffmpeg ``atempo`` fallback for extreme ratios) so
                 each block fits the exact slot defined by the subtitle.
    4. Stitch:   concatenate the per-block audio together, inserting the exact
                 milliseconds of silence dictated by the subtitle file.
    5. Render:   use ffmpeg to assemble a final video that either:
                   * burns the subtitles into a plain coloured background
                     (no source video required), or
                   * muxes the dubbed audio over a user-supplied background
                     video.

The module is intentionally dependency-light. ``edge-tts`` and ``pydub`` are the
only hard requirements; ``audiostretchy`` is imported lazily and falls back to
``atempo`` when it is unavailable. ``ffmpeg`` must be installed on the host
system (already used elsewhere in this codebase).

References / inspiration:
  * "Edge TTS Subtitle Dubbing" (fr0stb1rd) -- sample-accurate time-slot
    filling with numpy/librosa. https://github.com/fr0stb1rd/Edge-TTS-Subtitle-Dubbing
  * ffmpeg atempo trick for pitch-preserving speed change.
"""

from __future__ import annotations

import asyncio
import os
import re
import subprocess
import tempfile
import uuid
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple

logger = __import__("logging").getLogger(__name__)

# Use system ffmpeg instead of Trae's restricted version
FFMPEG_PATH = "/usr/bin/ffmpeg"
FFPROBE_PATH = "/usr/bin/ffprobe"

# --- Defaults ---------------------------------------------------------------

DEFAULT_VOICE = "en-GB-SoniaNeural"  # British English (London accent)
DEFAULT_TTS_RATE = "+0%"
DEFAULT_TTS_VOLUME = "+0%"
DEFAULT_TTS_PITCH = "+0Hz"

# Cap the stretch ratio so speech still sounds natural. ``atempo`` accepts a
# single value in [0.5, 2.0]; we tighten that window to keep things natural.
# never slower than 0.75x (block too long -> speed up)
MIN_STRETCH_RATIO = 0.75
# never faster than 1.5x  (block too short -> slow down)
MAX_STRETCH_RATIO = 1.5

# Output video defaults (used when no background video is provided).
DEFAULT_VIDEO_WIDTH = 1280
DEFAULT_VIDEO_HEIGHT = 720
DEFAULT_VIDEO_FPS = 30
DEFAULT_BG_COLOR = "0x0F172A"  # slate-900


# --- Subtitle parsing -------------------------------------------------------

@dataclass
class SubtitleCue:
    """A single subtitle block (one SRT/VTT entry)."""
    index: int
    start: float          # seconds
    end: float            # seconds
    text: str

    @property
    def duration(self) -> float:
        return max(0.0, self.end - self.start)


def parse_srt(content: str) -> List[SubtitleCue]:
    """Parse SRT-formatted text into a list of :class:`SubtitleCue`."""
    cues: List[SubtitleCue] = []
    blocks = re.split(r"\r?\n\r?\n", content.strip())
    for block in blocks:
        lines = [ln for ln in block.splitlines() if ln.strip()]
        if len(lines) < 2:
            continue
        # If first line is a number, drop it.
        if lines[0].strip().isdigit():
            lines = lines[1:]
        if not lines:
            continue
        time_line = lines[0]
        text = " ".join(lines[1:]).strip()
        m = re.match(
            r"(\d{2}:\d{2}:\d{2}[,.]\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2}[,.]\d{3})",
            time_line,
        )
        if not m:
            continue
        cues.append(
            SubtitleCue(
                index=len(cues) + 1,
                start=_parse_timestamp(m.group(1)),
                end=_parse_timestamp(m.group(2)),
                text=text,
            )
        )
    return cues


def parse_vtt(content: str) -> List[SubtitleCue]:
    """Parse WebVTT-formatted text into a list of :class:`SubtitleCue`."""
    if "WEBVTT" in content[:20]:
        # Strip the header
        content = content.split("\n", 1)[1] if "\n" in content else ""
    cues: List[SubtitleCue] = []
    blocks = re.split(r"\r?\n\r?\n", content.strip())
    for block in blocks:
        lines = [ln for ln in block.splitlines() if ln.strip()]
        if not lines:
            continue
        # Drop optional cue identifier line (a line without a --> in it that is
        # not a number either)
        if "-->" not in lines[0]:
            lines = lines[1:]
        if not lines:
            continue
        m = re.match(
            r"(?:\d{2}:)?(\d{2}:\d{2}[,.]\d{3}|\d{2}:\d{2}:\d{2}[,.]\d{3})"
            r"\s*-->\s*(?:\d{2}:)?(\d{2}:\d{2}[,.]\d{3}|\d{2}:\d{2}:\d{2}[,.]\d{3})",
            lines[0],
        )
        if not m:
            continue
        text = " ".join(lines[1:]).strip()
        cues.append(
            SubtitleCue(
                index=len(cues) + 1,
                start=_parse_timestamp(m.group(1)),
                end=_parse_timestamp(m.group(2)),
                text=text,
            )
        )
    return cues


def parse_subtitle_file(path: str) -> List[SubtitleCue]:
    """Auto-detect format by extension and parse."""
    with open(path, "r", encoding="utf-8-sig") as fh:
        content = fh.read()
    ext = os.path.splitext(path)[1].lower()
    if ext == ".vtt":
        return parse_vtt(content)
    return parse_srt(content)


def _parse_timestamp(ts: str) -> float:
    """Parse ``HH:MM:SS,mmm`` / ``MM:SS.mmm`` timestamps to seconds."""
    ts = ts.strip().replace(",", ".")
    parts = ts.split(":")
    if len(parts) == 3:
        h, m, s = parts
        return int(h) * 3600 + int(m) * 60 + float(s)
    if len(parts) == 2:
        m, s = parts
        return int(m) * 60 + float(s)
    return float(ts)


# --- TTS synthesis ----------------------------------------------------------

async def _synthesise_one(
    text: str,
    out_path: str,
    voice: str,
    rate: str,
    volume: str,
    pitch: str,
) -> float:
    """Synthesise a single block of text with edge-tts.

    Returns the duration (in seconds) of the generated audio.
    """
    import edge_tts

    communicate = edge_tts.Communicate(
        text=text,
        voice=voice,
        rate=rate,
        volume=volume,
        pitch=pitch,
    )
    await communicate.save(out_path)

    # Probe duration with ffprobe (single-shot, fast).
    return _probe_duration(out_path)


async def _synthesise_batch(
    cues: List[SubtitleCue],
    work_dir: str,
    voice: str,
    rate: str,
    volume: str,
    pitch: str,
    concurrency: int = 8,
) -> List[Tuple[SubtitleCue, str, float]]:
    """Synthesise TTS for every cue in parallel.

    Returns a list of ``(cue, audio_path, duration_seconds)`` tuples in the
    original cue order.
    """
    semaphore = asyncio.Semaphore(max(1, concurrency))

    async def _one(cue: SubtitleCue) -> Tuple[SubtitleCue, str, float]:
        out_path = os.path.join(work_dir, f"tts_{cue.index:05d}.mp3")
        async with semaphore:
            try:
                dur = await _synthesise_one(
                    cue.text, out_path, voice, rate, volume, pitch
                )
            except Exception as exc:  # pragma: no cover - network errors
                logger.warning("TTS failed for cue %d: %s", cue.index, exc)
                # Fall back to silence of the target duration so the rest of
                # the pipeline can still complete.
                _silence(out_path, cue.duration)
                dur = cue.duration
        return cue, out_path, dur

    return await asyncio.gather(*[_one(c) for c in cues])


def _silence(path: str, duration: float) -> None:
    """Write a silent mp3 of ``duration`` seconds to ``path``."""
    subprocess.run(
        [
            FFMPEG_PATH, "-y", "-f", "lavfi",
            "-i", f"anullsrc=r=24000:cl=mono",
            "-t", f"{max(0.1, duration):.3f}",
            "-acodec", "libmp3lame",
            path,
        ],
        capture_output=True,
        check=True,
    )


def _probe_duration(path: str) -> float:
    """Return media duration in seconds using ffprobe."""
    try:
        out = subprocess.run(
            [
                FFPROBE_PATH, "-v", "error",
                "-show_entries", "format=duration",
                "-of", "default=noprint_wrappers=1:nokey=1",
                path,
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        return float(out.stdout.strip())
    except Exception:
        return 0.0


# --- Time-stretching --------------------------------------------------------

def _clamped_ratio(target: float, actual: float) -> float:
    """Compute the speed ratio needed to fit ``actual`` seconds into ``target``."""
    if actual <= 0:
        return 1.0
    ratio = actual / target  # >1 means we must speed up (play faster)
    return max(MIN_STRETCH_RATIO, min(MAX_STRETCH_RATIO, ratio))


def _stretch_audio(
    in_path: str,
    out_path: str,
    target_duration: float,
    actual_duration: float,
) -> Tuple[float, str]:
    """Pitch-preserving time-stretch.

    Returns ``(resulting_duration, method)``.

    Tries ``audiostretchy`` first (high quality, WSOLA) and falls back to
    ffmpeg ``atempo`` (good quality, very portable).
    """
    if actual_duration <= 0 or target_duration <= 0:
        return target_duration, "silence"

    ratio = _clamped_ratio(target_duration, actual_duration)

    # If we are within ~30ms of target, skip stretching entirely -- the gap
    # will be absorbed by the silence stitching step.
    if abs(actual_duration - target_duration) < 0.03 or abs(ratio - 1.0) < 0.01:
        # Just copy the file.
        _copy(in_path, out_path)
        return actual_duration, "copy"

    method = _try_audiostretchy(in_path, out_path, ratio)
    if method is None:
        method = _try_atempo(in_path, out_path, ratio)
    if method is None:
        # Last resort: copy as-is and let silence/padding handle the diff.
        _copy(in_path, out_path)
        return actual_duration, "copy-fallback"

    # atempo / audiostretchy produce slightly different durations; trim or pad
    # so the block is exactly the target length.
    _force_duration(out_path, target_duration)
    return target_duration, method


def _try_audiostretchy(in_path: str, out_path: str, ratio: float) -> Optional[str]:
    """Try the ``audiostretchy`` library. Returns ``"audiostretchy"`` on success."""
    try:
        from audiostretchy import AudioStretch  # type: ignore
    except Exception:
        return None
    try:
        AudioStretch().stretch(
            in_path,
            out_path,
            # >1 = faster (shorter), <1 = slower (longer)
            ratio=ratio,
        )
        return "audiostretchy"
    except Exception as exc:
        logger.warning(
            "audiostretchy failed (%s); falling back to atempo", exc)
        return None


def _try_atempo(in_path: str, out_path: str, ratio: float) -> Optional[str]:
    """Use ffmpeg ``atempo`` filter (supports 0.5-2.0 per stage).

    Returns ``"atempo"`` on success, ``None`` on failure.
    """
    # Chain atempo filters for ratios outside [0.5, 2.0]. In practice we have
    # already clamped to [0.75, 1.5], but keep the chain logic for safety.
    filters = _build_atempo_chain(ratio)
    try:
        subprocess.run(
            [
                FFMPEG_PATH, "-y", "-i", in_path,
                "-filter:a", ",".join(filters),
                "-acodec", "libmp3lame",
                out_path,
            ],
            capture_output=True,
            check=True,
        )
        return "atempo"
    except Exception as exc:
        logger.warning("atempo failed: %s", exc)
        return None


def _build_atempo_chain(ratio: float) -> List[str]:
    """Split a ratio into a chain of ``atempo`` filters in [0.5, 2.0]."""
    if 0.5 <= ratio <= 2.0:
        return [f"atempo={ratio:.6f}"]
    chain: List[str] = []
    remaining = ratio
    while remaining > 2.0:
        chain.append("atempo=2.0")
        remaining /= 2.0
    while remaining < 0.5:
        chain.append("atempo=0.5")
        remaining /= 0.5
    chain.append(f"atempo={remaining:.6f}")
    return chain


def _force_duration(path: str, target: float) -> None:
    """Ensure the audio in ``path`` is exactly ``target`` seconds (trim or pad)."""
    tmp = path + ".tmp.mp3"
    subprocess.run(
        [
            FFMPEG_PATH, "-y", "-i", path,
            "-t", f"{target:.3f}",
            "-af", f"apad=whole_dur={target:.3f}",
            "-acodec", "libmp3lame",
            tmp,
        ],
        capture_output=True,
        check=True,
    )
    os.replace(tmp, path)


def _copy(src: str, dst: str) -> None:
    subprocess.run([FFMPEG_PATH, "-y", "-i", src, "-acodec", "libmp3lame", dst],
                   capture_output=True, check=True)


# --- Stitching --------------------------------------------------------------

def _stitch_audio(
    stretched: List[Tuple[SubtitleCue, str, float]],
    out_path: str,
    total_duration: float,
) -> float:
    """Concatenate per-block audio with the exact silence dictated by the SRT.

    Uses ``pydub`` for sample-accurate placement. The output is padded to
    ``total_duration`` so it always matches the underlying video timeline.
    """
    from pydub import AudioSegment  # type: ignore

    # Configure pydub to use system ffmpeg instead of Trae's restricted version
    AudioSegment.converter = FFMPEG_PATH

    output = AudioSegment.silent(duration=int(
        total_duration * 1000), frame_rate=24000)

    for cue, audio_path, _ in stretched:
        if cue.start < 0 or cue.end <= cue.start:
            continue
        block = AudioSegment.from_file(audio_path)
        # Place block at cue.start (overlay).
        output = output.overlay(block, position=int(cue.start * 1000))

    output.export(out_path, format="mp3")
    return len(output) / 1000.0


# --- Video rendering --------------------------------------------------------

def _render_video(
    audio_path: str,
    subtitle_path: str,
    out_path: str,
    background_video: Optional[str] = None,
    width: int = DEFAULT_VIDEO_WIDTH,
    height: int = DEFAULT_VIDEO_HEIGHT,
    fps: int = DEFAULT_VIDEO_FPS,
    bg_color: str = DEFAULT_BG_COLOR,
) -> None:
    """Render the final video.

    If ``background_video`` is provided, the dubbed audio + burned-in
    subtitles are muxed over it (the video is re-encoded to match the audio
    duration). Otherwise a solid coloured background is generated on the fly.
    """
    sub_filter = (
        f"subtitles={_escape_filter_path(subtitle_path)}:force_style="
        "'Alignment=2,MarginV=60,FontSize=18,Outline=2,Shadow=1,"
        "PrimaryColour=&HFFFFFFFF,OutlineColour=&H00000000'"
    )

    if background_video and os.path.exists(background_video):
        # Mux audio over the background video, scaling it to fit and matching
        # the audio's duration.
        cmd = [
            FFMPEG_PATH, "-y",
            "-i", background_video,
            "-i", audio_path,
            "-filter_complex",
            f"[0:v]scale={width}:{height}:force_original_aspect_ratio=decrease,"
            f"pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:{bg_color},"
            f"{sub_filter}[v]",
            "-map", "[v]",
            "-map", "1:a",
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-r", str(fps),
            "-c:a", "aac",
            "-b:a", "192k",
            "-shortest",
            out_path,
        ]
    else:
        # Generate a solid colour background of the same length as the audio.
        cmd = [
            FFMPEG_PATH, "-y",
            "-f", "lavfi",
            "-i", f"color=c={bg_color}:s={width}x{height}:r={fps}",
            "-i", audio_path,
            "-filter_complex",
            f"[0:v]{sub_filter}[v]",
            "-map", "[v]",
            "-map", "1:a",
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac",
            "-b:a", "192k",
            "-shortest",
            out_path,
        ]

    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"ffmpeg render failed: {result.stderr[-2000:]}")


def _escape_filter_path(path: str) -> str:
    """Escape a filesystem path for use inside an ffmpeg filtergraph."""
    return path.replace("\\", "\\\\").replace(":", "\\:").replace("'", "\\'")


# --- Orchestration ----------------------------------------------------------

@dataclass
class GenerationResult:
    success: bool
    audio_path: str = ""
    video_path: str = ""
    subtitle_path: str = ""
    cues: List[Dict[str, Any]] = field(default_factory=list)
    stats: Dict[str, Any] = field(default_factory=dict)
    error: str = ""


class SubtitleVideoGenerator:
    """Orchestrates the full subtitle -> TTS -> video pipeline."""

    def generate_audio_only(
        self,
        subtitle_path: str,
        output_dir: Optional[str] = None,
        voice: str = DEFAULT_VOICE,
        rate: str = DEFAULT_TTS_RATE,
        volume: str = DEFAULT_TTS_VOLUME,
        pitch: str = DEFAULT_TTS_PITCH,
        concurrency: int = 8,
        target_total_duration: Optional[float] = None,
    ) -> GenerationResult:
        """Run steps 1-4 of the pipeline (parse, TTS, stretch, stitch).

        Produces a single ``dubbed_audio.mp3`` plus a clean ``subs.srt`` but
        does NOT render a video. This is reused by the video-dubber, which
        muxes the resulting audio back onto an existing video.

        If ``target_total_duration`` is given (e.g. the source video's
        duration), the stitched audio is padded to exactly that length so it
        lines up with the original video timeline.
        """
        cues = parse_subtitle_file(subtitle_path)
        if not cues:
            return GenerationResult(success=False, error="No subtitle cues found.")

        if output_dir is None:
            output_dir = os.path.dirname(subtitle_path) or "."
        os.makedirs(output_dir, exist_ok=True)

        run_id = uuid.uuid4().hex[:8]
        work_dir = os.path.join(output_dir, f"sv_{run_id}")
        os.makedirs(work_dir, exist_ok=True)

        audio_path = os.path.join(work_dir, "dubbed_audio.mp3")
        clean_srt = os.path.join(work_dir, "subs.srt")
        _write_clean_srt(cues, clean_srt)

        try:
            # 1. Synthesise TTS for every cue.
            synthesised = asyncio.run(_synthesise_batch(
                cues, work_dir, voice, rate, volume, pitch, concurrency
            ))

            # 2. Time-stretch each block to fit its slot.
            stretched: List[Tuple[SubtitleCue, str, float]] = []
            stretch_stats: Dict[str, int] = {}
            for cue, tts_path, actual in synthesised:
                target = cue.duration
                out_path = os.path.join(
                    work_dir, f"stretched_{cue.index:05d}.mp3")
                _, method = _stretch_audio(
                    tts_path, out_path, target, actual
                )
                stretched.append((cue, out_path, target))
                stretch_stats[method] = stretch_stats.get(method, 0) + 1

            # 3. Stitch everything together with sample-accurate silence.
            # Use the larger of (last cue end + 0.5s) and the requested target
            # duration so the audio always covers the full original video.
            natural_end = max(c.end for c in cues) + 0.5
            total_duration = (
                max(natural_end, target_total_duration)
                if target_total_duration
                else natural_end
            )
            _stitch_audio(stretched, audio_path, total_duration)

            return GenerationResult(
                success=True,
                audio_path=audio_path,
                subtitle_path=clean_srt,
                cues=[
                    {"index": c.index, "start": c.start,
                        "end": c.end, "text": c.text}
                    for c in cues
                ],
                stats={
                    "cues": len(cues),
                    "total_duration": total_duration,
                    "voice": voice,
                    "stretch_methods": stretch_stats,
                    "min_stretch_ratio": MIN_STRETCH_RATIO,
                    "max_stretch_ratio": MAX_STRETCH_RATIO,
                    "work_dir": work_dir,
                },
            )
        except Exception as exc:
            logger.exception("Audio-only generation failed")
            return GenerationResult(success=False, error=str(exc))

    def generate(
        self,
        subtitle_path: str,
        output_dir: Optional[str] = None,
        voice: str = DEFAULT_VOICE,
        rate: str = DEFAULT_TTS_RATE,
        volume: str = DEFAULT_TTS_VOLUME,
        pitch: str = DEFAULT_TTS_PITCH,
        background_video: Optional[str] = None,
        width: int = DEFAULT_VIDEO_WIDTH,
        height: int = DEFAULT_VIDEO_HEIGHT,
        fps: int = DEFAULT_VIDEO_FPS,
        bg_color: str = DEFAULT_BG_COLOR,
        concurrency: int = 8,
    ) -> GenerationResult:
        """Full pipeline: parse -> TTS -> stretch -> stitch -> render video."""
        audio_result = self.generate_audio_only(
            subtitle_path=subtitle_path,
            output_dir=output_dir,
            voice=voice,
            rate=rate,
            volume=volume,
            pitch=pitch,
            concurrency=concurrency,
        )
        if not audio_result.success:
            return audio_result

        work_dir = audio_result.stats.get(
            "work_dir", os.path.dirname(audio_result.audio_path))
        video_path = os.path.join(
            os.path.dirname(work_dir),
            f"{os.path.splitext(os.path.basename(subtitle_path))[0]}_dubbed.mp4",
        )

        try:
            _render_video(
                audio_result.audio_path, audio_result.subtitle_path, video_path,
                background_video=background_video,
                width=width, height=height, fps=fps, bg_color=bg_color,
            )
            return GenerationResult(
                success=True,
                audio_path=audio_result.audio_path,
                video_path=video_path,
                subtitle_path=audio_result.subtitle_path,
                cues=audio_result.cues,
                stats=audio_result.stats,
            )
        except Exception as exc:
            logger.exception("Video render failed")
            return GenerationResult(success=False, error=str(exc))


def _write_clean_srt(cues: List[SubtitleCue], path: str) -> None:
    """Write cues to a fresh UTF-8 SRT file (avoids BOM/quoting issues)."""
    with open(path, "w", encoding="utf-8") as fh:
        for cue in cues:
            fh.write(f"{cue.index}\n")
            fh.write(
                f"{_format_srt_time(cue.start)} --> {_format_srt_time(cue.end)}\n")
            fh.write(f"{cue.text}\n\n")


def _format_srt_time(seconds: float) -> str:
    h = int(seconds // 3600)
    m = int((seconds % 3600) // 60)
    s = int(seconds % 60)
    ms = int((seconds % 1) * 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


# Convenience singleton, mirroring the pattern used by other services
# (audio_processor.py / subtitle_processor.py).
subtitle_video_generator = SubtitleVideoGenerator()
