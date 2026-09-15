"""Rust ffmpeg-next media helpers — replaces PyAV for audio/video processing.

The heavy lifting (FFmpeg via `ffmpeg-next`) is done in the compiled Rust
extension `english_media_native` (see ``native/rust``). This module is a thin,
compatibility-preserving wrapper over it so the rest of the dubbing pipeline
keeps its existing call sites and return types.

It replaces the system ``ffmpeg``/``ffprobe`` binaries and the PyAV bindings
for:

  * reading media duration (replaces ``ffprobe``)
  * converting edge-tts mp3 output to wav (replaces ``ffmpeg -i mp3 ...``)
  * resampling audio to a target sample rate / mono
  * muxing video + audio into the final .mp4 (``ffmpeg -c:v copy -c:a aac``)
"""

from __future__ import annotations

import logging
from typing import Optional

import numpy as np

logger = logging.getLogger(__name__)


def _get_native() -> "object":
    """Import the Rust extension lazily (kept importable without it)."""
    import english_media_native

    return english_media_native


def get_media_duration(path: str) -> float:
    """Return the duration of a media file in seconds (video or audio).

    Backed by the Rust ffmpeg-next extension (no system ``ffprobe``).
    Reads the container-level duration first, then falls back to the
    longest stream's duration.
    """
    return float(_get_native().duration(path))


def decode_audio(
    path: str,
    target_sr: int = 24000,
    mono: bool = True,
    max_seconds: Optional[float] = None,
) -> np.ndarray:
    """Decode any audio/video file to a 1-D float32 numpy array.

    Backed by the Rust ffmpeg-next extension which decodes + resamples to
    the target sample rate / channel layout in one pass (no system ``ffmpeg``
    subprocess, no intermediate WAV).

    Args:
        path: input file (any container/codec FFmpeg supports — mp3, mp4,
            m4a, wav, ogg, flac, …).
        target_sr: target sample rate in Hz.
        mono: if True, downmix to mono.
        max_seconds: if set, stop decoding after this many seconds of
            output audio (useful for probes / tests).

    Returns:
        ``np.ndarray`` of shape ``(N,)`` float32 in ``[-1, 1]``.
    """
    samples = _get_native().decode_audio(
        path,
        target_sr=int(target_sr),
        mono=bool(mono),
        max_seconds=float(max_seconds) if max_seconds is not None else None,
    )
    return np.asarray(samples, dtype=np.float32)


def mp3_to_wav(
    mp3_path: str,
    wav_path: str,
    target_sr: int = 24000,
    mono: bool = True,
) -> None:
    """Convert an mp3 file to a wav file via the Rust decoder (no system ffmpeg).

    Used by :class:`EdgeTTSBackend` to convert edge-tts's mp3 output to
    the wav format the rest of the pipeline expects.
    """
    import soundfile as sf

    audio = decode_audio(mp3_path, target_sr=target_sr, mono=mono)
    sf.write(wav_path, audio, target_sr)


def extract_room_tone(
    video_path: str,
    target_sr: int = 24000,
    segment_duration: float = 0.5,
    max_segments: int = 20,
) -> np.ndarray:
    """Extract ambient noise from a video's silent gaps.

    Scans the video's audio track for the quietest segments (likely pure
    ambient noise / room tone) and concatenates them. Used to fill gaps
    between TTS sentences so the dub doesn't have jarring dead-silence.

    Args:
        video_path: input video file.
        target_sr: output sample rate.
        segment_duration: length of each candidate segment in seconds.
        max_segments: max number of quiet segments to concatenate.

    Returns:
        1-D float32 array of concatenated room-tone segments. May be empty
        if the video has no quiet gaps (e.g. music throughout).
    """
    try:
        audio = decode_audio(video_path, target_sr=target_sr, mono=True)
    except Exception as e:  # noqa: BLE001
        logger.warning("extract_room_tone: decode failed: %s", e)
        return np.zeros(0, dtype=np.float32)

    if audio.size < target_sr:  # < 1s
        return np.zeros(0, dtype=np.float32)

    seg_n = int(segment_duration * target_sr)
    n_segs = len(audio) // seg_n
    if n_segs < 2:
        return np.zeros(0, dtype=np.float32)

    # Compute RMS for each segment, pick the quietest ones.
    segments = []
    for i in range(n_segs):
        seg = audio[i * seg_n:(i + 1) * seg_n]
        rms = float(np.sqrt(np.mean(seg ** 2)))
        segments.append((rms, seg))

    segments.sort(key=lambda x: x[0])
    # Take the quietest segments (but not the absolute quietest — that might
    # be digital silence; take segments 2..max_segments+2).
    quiet = segments[1:max_segments + 2]
    if not quiet:
        return np.zeros(0, dtype=np.float32)

    room_tone = np.concatenate([seg for _, seg in quiet])
    # Final safety: if the extracted room tone is near-silent, return empty
    # (the stitcher will skip room tone fill in that case).
    rt_rms = float(np.sqrt(np.mean(room_tone ** 2)))
    if rt_rms < 1e-5:
        return np.zeros(0, dtype=np.float32)

    logger.info("extract_room_tone: %d segments, %.1fs total, rms=%.4f",
                len(quiet), len(room_tone) / target_sr, rt_rms)
    return room_tone.astype(np.float32)


def mux_video_audio(
    video_path: str,
    audio_path: str,
    output_path: str,
    audio_bitrate: int = 128_000,
    shortest: bool = True,
) -> None:
    """Mux a video stream (copied) with an audio track (encoded to AAC) into an .mp4.

    Replaces the system-ffmpeg command::

        ffmpeg -i video.mp4 -i audio.wav \\
               -c:v copy -c:a aac -b:a 128k \\
               -map 0:v:0 -map 1:a:0 -shortest out.mp4

    Backed by the Rust ffmpeg-next extension: the video stream is remuxed
    (codec config copied, packets passed through without transcode — no
    generational loss, exactly what ``-c:v copy`` does) and the audio WAV is
    encoded to AAC. ``av_interleaved_write_frame`` renders packets in DTS
    order, so feeding video then audio produces a correctly interleaved MP4.

    Args:
        video_path: input video file (any container FFmpeg supports).
        audio_path: input WAV file (the dubbed audio track).
        output_path: output .mp4 path.
        audio_bitrate: AAC bitrate in bits/sec (default 128000 = 128 kbps).
        shortest: if True, stop encoding audio once its timestamp exceeds
            the video duration (emulates ``-shortest``).
    """
    _get_native().mux_video_audio(
        video_path,
        audio_path,
        output_path,
        audio_bitrate=int(audio_bitrate),
        shortest=bool(shortest),
    )