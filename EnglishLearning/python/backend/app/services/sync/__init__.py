"""Sync-aware dubbing pipeline.

A drop-in timing layer that fixes the audio-video drift in
``run_chattts_dub.py``. The original scripts are kept intact; this package
adds the missing pieces:

    word_aligner   — word-level forced alignment (WhisperX / stable-ts / fallback)
    cue_builder    — groups aligned words into sentences with pauses
    chunk_stitcher — sentence-level synthesis + compress-only stretch (v3)
    voice_config   — VoiceConfig: backend, gender, accent, decoding params
    tts_backends   — pluggable TTS (ChatTTS local + edge-tts native accent)
    parallel_synth — parallel synthesis + gentle stitcher (no phase vocoder)
    subtitle_writer — SRT subtitle generation from aligned cues (sentence + word level)
    models         — shared dataclasses (Word / Chunk / AlignedCue)

See ``docs/TTS_SYNC_MECHANISM.md`` for the full design rationale and
``run_sync_dub.py`` for an end-to-end script that wires these together.
"""

from .models import AlignedCue, Chunk, Word
from .word_aligner import WordAligner
from .cue_builder import build_aligned_cues
from .chunk_stitcher import (
    ChunkStitcher,
    SentenceStitcher,
    split_into_chunks,
    fit_chunk,
)
from .voice_config import VoiceConfig
from .tts_backends import (
    TTSBackend,
    ChatTTSBackend,
    EdgeTTSBackend,
    build_backend,
)
from .parallel_synth import (
    ParallelSynthesizer,
    GentleStitcher,
    RespeedSynthesizer,
    PlaceStitcher,
    normalize_text,
    validate_audio,
)
from .subtitle_writer import (
    write_srt,
    write_word_srt,
    write_segments_srt,
    cues_to_srt_string,
    split_cues_for_subtitles,
)
from .av_utils import (
    get_media_duration,
    decode_audio,
    mp3_to_wav,
    mux_video_audio,
    extract_room_tone,
)

__all__ = [
    "AlignedCue",
    "Chunk",
    "Word",
    "WordAligner",
    "build_aligned_cues",
    "ChunkStitcher",
    "SentenceStitcher",
    "split_into_chunks",
    "fit_chunk",
    "VoiceConfig",
    "TTSBackend",
    "ChatTTSBackend",
    "EdgeTTSBackend",
    "build_backend",
    "ParallelSynthesizer",
    "GentleStitcher",
    "RespeedSynthesizer",
    "PlaceStitcher",
    "normalize_text",
    "validate_audio",
    "write_srt",
    "write_word_srt",
    "write_segments_srt",
    "cues_to_srt_string",
    "split_cues_for_subtitles",
    "get_media_duration",
    "decode_audio",
    "mp3_to_wav",
    "mux_video_audio",
    "extract_room_tone",
]
