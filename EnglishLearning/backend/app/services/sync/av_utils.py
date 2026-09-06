"""PyAV-based media helpers — replaces system ffmpeg for audio decoding.

faster-whisper's README notes:

    "Unlike openai-whisper, FFmpeg does not need to be installed on the
    system. The audio is decoded with the Python library PyAV which bundles
    the FFmpeg libraries in its package."

This module exposes that same PyAV decoding for the rest of the dubbing
pipeline, so we no longer shell out to a system ``ffmpeg`` binary for:

  * reading media duration (replaces ``ffprobe``)
  * converting edge-tts mp3 output to wav (replaces ``ffmpeg -i mp3 ...``)
  * resampling audio to a target sample rate / mono

System ``ffmpeg`` is still needed for stage 7 (muxing video + audio into
the final .mp4) — PyAV can mux but the API is far more verbose than a
single ``ffmpeg -c:v copy -c:a aac`` call. That one usage is documented
in the orchestrator.
"""

from __future__ import annotations

import logging
from typing import Optional

import numpy as np

logger = logging.getLogger(__name__)


def get_media_duration(path: str) -> float:
    """Return the duration of a media file in seconds (video or audio).

    Uses PyAV — no system ``ffprobe`` required. Reads the container-level
    duration, which for MP4/MKV/MP3 is reliable.
    """
    import av

    container = av.open(path)
    try:
        # Container duration is in microseconds (AV_TIME_BASE = 1_000_000).
        if container.duration is not None and container.duration > 0:
            return float(container.duration) / 1_000_000.0
        # Fallback: derive from the longest stream's duration.
        best = 0.0
        for stream in container.streams:
            if stream.duration is None:
                continue
            # Stream duration is in stream's time_base units; for audio/video
            # streams av.streams expose it as a float already in seconds via
            # the .duration property when accessed through the container.
            try:
                d = float(stream.duration * stream.time_base)
            except (TypeError, ValueError):
                continue
            best = max(best, d)
        return best
    finally:
        container.close()


def decode_audio(
    path: str,
    target_sr: int = 24000,
    mono: bool = True,
    max_seconds: Optional[float] = None,
) -> np.ndarray:
    """Decode any audio/video file to a 1-D float32 numpy array.

    Uses PyAV's ``AudioResampler`` to convert to the target sample rate
    and channel layout in one pass — no system ``ffmpeg`` subprocess and
    no intermediate WAV file.

    Args:
        path: input file (any container/codec PyAV supports — mp3, mp4,
            m4a, wav, ogg, flac, …).
        target_sr: target sample rate in Hz.
        mono: if True, downmix to mono.
        max_seconds: if set, stop decoding after this many seconds of
            output audio (useful for probes / tests).

    Returns:
        ``np.ndarray`` of shape ``(N,)`` float32 in ``[-1, 1]``.
    """
    import av

    container = av.open(path)
    try:
        layout = "mono" if mono else "stereo"
        resampler = av.AudioResampler(
            format="fltp",   # 32-bit float planar — to_ndarray gives float32
            layout=layout,
            rate=target_sr,
        )
        chunks: list[np.ndarray] = []
        total_samples = 0
        max_samples = int(max_seconds * target_sr) if max_seconds else None

        for frame in container.decode(audio=0):
            resampled = resampler.resample(frame)
            for rf in resampled:
                # to_ndarray() on a planar-float frame returns shape
                # (channels, samples); for mono that's (1, N).
                arr = rf.to_ndarray()
                if mono and arr.ndim > 1:
                    arr = arr.mean(axis=0)
                else:
                    arr = arr.reshape(-1)
                chunks.append(arr.astype(np.float32, copy=False))
                total_samples += arr.shape[-1]
                if max_samples is not None and total_samples >= max_samples:
                    # Truncate to the requested max length.
                    out = np.concatenate(chunks)[:max_samples]
                    return out
        if not chunks:
            return np.zeros(0, dtype=np.float32)
        return np.concatenate(chunks)
    finally:
        container.close()


def mp3_to_wav(
    mp3_path: str,
    wav_path: str,
    target_sr: int = 24000,
    mono: bool = True,
) -> None:
    """Convert an mp3 file to a wav file via PyAV (no system ffmpeg).

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
    except Exception as e:
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

    Uses **PyAV only** — no system ``ffmpeg`` subprocess. The video stream
    is **remuxed** (codec setup copied via ``add_stream(template=...)``,
    packets passed through without decode/encode, so no generational loss
    — exactly what ``-c:v copy`` does). The audio WAV is **encoded to AAC**
    via PyAV's ``add_stream('aac', ...)`` + ``stream.encode(frame)``.

    PyAV's :meth:`Container.mux` calls ``av_interleaved_write_frame``
    internally, which buffers packets and writes them in DTS order — so
    feeding all video packets first, then all audio packets, produces a
    correctly interleaved MP4. This is the recommended remuxing pattern
    from the PyAV cookbook (see https://pyav.org/docs/stable/cookbook/basics.html#remuxing).

    Args:
        video_path: input video file (any container PyAV supports).
        audio_path: input WAV file (the dubbed audio track).
        output_path: output .mp4 path.
        audio_bitrate: AAC bitrate in bits/sec (default 128000 = 128 kbps).
        shortest: if True, stop encoding audio once its timestamp exceeds
            the video duration (emulates ``-shortest``).
    """
    import av
    import numpy as np
    import soundfile as sf
    from fractions import Fraction

    # ── open inputs ───────────────────────────────────────────────────
    in_container = av.open(video_path)
    if not in_container.streams.video:
        in_container.close()
        raise ValueError(f"no video stream in {video_path}")
    in_video = in_container.streams.video[0]

    audio_data, sr = sf.read(audio_path)
    if audio_data.ndim > 1:
        audio_data = audio_data[:, 0]            # → mono
    audio_data = audio_data.astype(np.float32)

    # Emulate -shortest: clip audio to video duration.
    if shortest:
        video_duration = float(in_container.duration) / 1_000_000.0
        max_samples = int(video_duration * sr)
        if len(audio_data) > max_samples:
            audio_data = audio_data[:max_samples]

    # ── open output container + streams ───────────────────────────────
    out_container = av.open(output_path, mode="w")
    # Remux: copy the video stream's codec config verbatim (no transcode).
    # In PyAV 17.x the cookbook's `add_stream(template=...)` kwarg was
    # promoted to a dedicated method `add_stream_from_template()`. It creates
    # a stream that carries packets through without invoking an encoder —
    # exactly what `ffmpeg -c:v copy` does.
    out_video = out_container.add_stream_from_template(in_video)
    # Audio: encode WAV → AAC.
    out_audio = out_container.add_stream("aac", rate=sr)
    out_audio.layout = "mono"
    out_audio.bit_rate = audio_bitrate

    # ── 1. remux video packets (copy, no decode/encode) ────────────────
    # PyAV's mux() uses av_interleaved_write_frame which buffers and
    # reorders by DTS, so feeding all video packets first then all audio
    # packets still yields a correctly interleaved MP4.
    for packet in in_container.demux(in_video):
        if packet.dts is None:
            continue                              # skip flushing packets
        packet.stream = out_video
        out_container.mux(packet)

    # ── 2. encode audio frames → AAC packets ──────────────────────────
    # AAC needs frames of a fixed size (frame_size, typically 1024 samples).
    # We chunk the WAV into frame_size blocks, set pts in audio time_base,
    # and encode. The encoder may buffer frames and emit packets in batches.
    frame_size = out_audio.frame_size or 1024
    # Triangular dither before int16 quantization — eliminates correlated
    # quantization noise (sounds like a low-level buzz on quiet passages).
    # Adds ~1 bit of noise at -96 dB, inaudible but decorrelates the error.
    noise = (np.random.rand(len(audio_data) + frame_size) - 0.5) * 2.0
    noise += (np.random.rand(len(audio_data) + frame_size) - 0.5) * 2.0
    audio_f64 = audio_data + noise[:len(audio_data)] * (1.0 / 32767.0)
    audio_int16 = np.clip(audio_f64 * 32767.0, -32768, 32767).astype(np.int16)
    audio_time_base = Fraction(1, sr)

    for offset in range(0, len(audio_int16), frame_size):
        chunk = audio_int16[offset:offset + frame_size]
        if len(chunk) < frame_size:
            # pad final partial frame with zeros so the encoder flushes
            chunk = np.pad(chunk, (0, frame_size - len(chunk)))
        # AudioFrame.from_ndarray on an s16 planar frame expects shape
        # (channels, samples); for mono that's (1, N).
        frame = av.AudioFrame.from_ndarray(
            chunk.reshape(1, -1), format="s16", layout="mono"
        )
        frame.sample_rate = sr
        frame.time_base = audio_time_base
        frame.pts = offset                        # sample-index as pts
        for packet in out_audio.encode(frame):
            packet.stream = out_audio
            out_container.mux(packet)

    # Flush the audio encoder (emits any buffered packets with dts=None
    # guard handled by encode() returning [] when truly done).
    for packet in out_audio.encode():
        packet.stream = out_audio
        out_container.mux(packet)

    # ── finalize ──────────────────────────────────────────────────────
    out_container.close()
    in_container.close()
