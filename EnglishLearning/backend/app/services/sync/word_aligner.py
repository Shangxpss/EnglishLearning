"""Word-level forced alignment for dubbing.

The single most important piece of timing information the original
``run_chattts_dub.py`` lacked was *where each word starts and ends* in the
source audio. ``faster-whisper`` only emits segment-level timestamps that
drift by ~1 s. This module supplies word-level timestamps accurate to
~50-100 ms via a pluggable backend.

Backends:

    1. ``stable_ts``  — (DEFAULT) wraps faster-whisper/whisper, refines word
                        timestamps via alignment. No pyannote/HF-token needed.
    2. ``whisperx``   — faster-whisper + wav2vec2 forced alignment (<100 ms).
                        Used only when the caller explicitly requests it via
                        ``backend="whisperx"`` (e.g. user asks for it).

There is no longer an "interpolation" fallback — uniform word splits were
the original root cause of the subtitle delay bug (every word got the same
duration regardless of actual phoneme length), so the interpolation code
path has been removed entirely. If neither backend imports, alignment fails
loudly rather than silently producing wrong timestamps.

All backends return the same ``List[Word]`` so callers do not care which one
was selected. Selection is logged once.
"""

from __future__ import annotations

import json
import logging
import os
import tempfile
from typing import List, Optional

from .models import Word, AlignedCue

logger = logging.getLogger(__name__)

# Backends in priority order. Each entry is (name, import-probe, factory).
_AVAILABLE_BACKEND: Optional[str] = None


class WordAligner:
    """Produces word-level timestamps for an audio file.

    Example::

        aligner = WordAligner(model_size="base", language="en")
        words = aligner.align("source.wav")
        # words[0].start, words[0].end, words[0].text ...

    The first call lazy-loads the chosen backend's heavy ML deps; subsequent
    calls reuse the loaded model.
    """

    def __init__(self, model_size: str = "base", language: str = "en",
                 device: str = "cpu", compute_type: str = "int8",
                 backend: Optional[str] = None,
                 cpu_threads: Optional[int] = None):
        self.model_size = model_size
        self.language = language
        self.device = device
        self.compute_type = compute_type
        self.cpu_threads = cpu_threads
        self._requested_backend = backend
        self._model = None  # loaded lazily
        # Segments from the underlying Whisper transcription, preserved so
        # callers can write them out as a "raw Whisper" SRT alongside the
        # rebuilt sentence-level SRT. Populated by each backend during
        # ``align()``. Each entry is an ``AlignedCue`` with ``text``,
        # ``start``, ``end`` (no word-level detail).
        self.last_segments: List["AlignedCue"] = []  # type: ignore[name-defined]

    # -- backend selection -------------------------------------------------

    def _select_backend(self) -> str:
        """Pick the backend to use.

        Selection rules:
          * If the caller explicitly requested a backend (e.g. ``whisperx``),
            use it (must be importable, otherwise raise).
          * Otherwise, use ``stable_ts`` (the default). It is the only backend
            tried via auto-discovery. If it is not importable, raise — there
            is no silent interpolation fallback any more (see module docstring).
        """
        if self._requested_backend:
            if not _probe(self._requested_backend):
                raise ImportError(
                    f"Requested aligner backend {self._requested_backend!r} is "
                    f"not importable in this environment."
                )
            return self._requested_backend

        global _AVAILABLE_BACKEND
        if _AVAILABLE_BACKEND:
            return _AVAILABLE_BACKEND

        # Auto-discovery: stable_ts is the default. whisperx is NOT
        # auto-selected — it's only used when the caller explicitly asks
        # for it (per user requirement).
        if _probe("stable_ts"):
            _AVAILABLE_BACKEND = "stable_ts"
            logger.info("WordAligner: using backend=stable_ts (default)")
            return _AVAILABLE_BACKEND

        raise ImportError(
            "No aligner backend available. The default backend 'stable_ts' "
            "(stable-whisper) is not importable. Install it with "
            "`pip install stable-ts`, or explicitly request 'whisperx' via "
            "WordAligner(backend='whisperx')."
        )

    @property
    def backend(self) -> str:
        return self._select_backend()

    # -- public API --------------------------------------------------------

    def align(self, audio_path: str, transcript: str = None,
              progress_callback=None, checkpoint_dir: str = None) -> List[Word]:
        """Align the audio, returning word-level timestamps.

        Args:
            audio_path: path to audio/video file.
            transcript: optional known transcript (improves alignment of noisy audio).
            progress_callback: optional callable(completed: int, total: int) called
                after each chunk is aligned. Use for progress reporting.
            checkpoint_dir: directory for checkpoint files. If None, uses the
                audio file's directory. Checkpoints allow resuming interrupted
                alignment jobs.
        """
        # Try loading a complete checkpoint first
        cached_words, completed, total = self._load_checkpoint(audio_path, checkpoint_dir)
        if cached_words is not None and completed >= total and total > 0:
            logger.info("WordAligner: loaded complete checkpoint (%d words)", len(cached_words))
            if progress_callback:
                progress_callback(total, total)
            return cached_words

        # Get audio duration to decide on chunking
        from .av_utils import get_media_duration, decode_audio
        duration = get_media_duration(audio_path)

        # For short audio (<60s), align directly — no chunking needed
        if duration < 60:
            words = self._align_single(audio_path)
            if checkpoint_dir is not None or True:  # always save checkpoint
                self._save_checkpoint(audio_path, words, 1, 1, checkpoint_dir)
            if progress_callback:
                progress_callback(1, 1)
            return words

        # For long audio: chunked alignment with progress + checkpoint
        return self._align_chunked(audio_path, duration, progress_callback,
                                   checkpoint_dir, completed, total, cached_words)

    def _align_single(self, audio_path: str) -> List[Word]:
        """Align a single file using the selected backend (no chunking)."""
        backend = self._select_backend()
        if backend == "whisperx":
            return self._align_whisperx(audio_path)
        if backend == "stable_ts":
            return self._align_stable_ts(audio_path)
        raise RuntimeError(
            f"Unknown aligner backend {backend!r}. Only 'stable_ts' (default) "
            f"and 'whisperx' (explicit) are supported."
        )

    def _align_chunked(self, audio_path: str, duration: float,
                       progress_callback, checkpoint_dir: str,
                       resume_chunk: int, total_chunks: int,
                       cached_words: list) -> List[Word]:
        """Align long audio in chunks, with progress reporting + checkpointing.

        Splits the audio into ~100 chunks (min 30s each), aligns each chunk
        independently, and saves a checkpoint after each chunk so interrupted
        runs can resume.
        """
        import soundfile as sf
        from .av_utils import decode_audio

        # Determine chunk count
        n_chunks = max(1, min(100, int(duration / 60)))  # ~1 chunk per minute, max 100
        chunk_duration = duration / n_chunks

        # If resuming from a previous run with a different chunk count, restart
        if total_chunks > 0 and total_chunks != n_chunks:
            logger.info("WordAligner: chunk count changed (%d→%d), re-aligning from start",
                        total_chunks, n_chunks)
            resume_chunk = 0
            cached_words = None

        all_words = list(cached_words) if cached_words else []
        # Rebuild segment list to match cached words if resuming
        all_segments: List[AlignedCue] = []
        start_chunk = resume_chunk if resume_chunk < n_chunks else 0

        if start_chunk > 0:
            logger.info("WordAligner: resuming from chunk %d/%d", start_chunk, n_chunks)

        # Decode full audio at 16kHz (Whisper's expected rate)
        sr = 16000
        audio = decode_audio(audio_path, target_sr=sr, mono=True)
        chunk_samples = len(audio) // n_chunks

        for i in range(start_chunk, n_chunks):
            chunk_start_sample = i * chunk_samples
            chunk_end_sample = (i + 1) * chunk_samples if i < n_chunks - 1 else len(audio)
            chunk_audio = audio[chunk_start_sample:chunk_end_sample]
            offset = chunk_start_sample / sr

            # Write chunk to temp wav
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
                temp_path = f.name
            try:
                sf.write(temp_path, chunk_audio, sr)
                # Align this chunk (also populates self.last_segments)
                chunk_words = self._align_single(temp_path)
                # Adjust word timestamps by chunk offset
                for w in chunk_words:
                    w.start += offset
                    w.end += offset
                all_words.extend(chunk_words)
                # Adjust segment timestamps by chunk offset and aggregate
                for seg in self.last_segments:
                    all_segments.append(AlignedCue(
                        text=seg.text,
                        start=seg.start + offset,
                        end=seg.end + offset,
                    ))
            finally:
                if os.path.exists(temp_path):
                    os.unlink(temp_path)

            # Save partial checkpoint
            self._save_checkpoint(audio_path, all_words, i + 1, n_chunks, checkpoint_dir)

            # Report progress
            if progress_callback:
                progress_callback(i + 1, n_chunks)

        logger.info("WordAligner: aligned %d words in %d chunks (%.1fs audio)",
                    len(all_words), n_chunks, duration)
        # Expose aggregated segments to the caller (one entry per Whisper
        # segment across all chunks, timestamps already offset to the
        # original audio timeline).
        self.last_segments = all_segments
        return all_words

    def _checkpoint_path(self, audio_path: str, checkpoint_dir: str = None) -> str:
        """Return the checkpoint file path for a given audio file."""
        d = checkpoint_dir or os.path.dirname(os.path.abspath(audio_path))
        stem = os.path.splitext(os.path.basename(audio_path))[0]
        return os.path.join(d, f".{stem}.align.json")

    def _load_checkpoint(self, audio_path: str, checkpoint_dir: str = None):
        """Load a complete checkpoint if it exists and is valid.

        Returns (words, completed_chunks, total_chunks) or (None, 0, 0).
        """
        path = self._checkpoint_path(audio_path, checkpoint_dir)
        if not os.path.exists(path):
            return None, 0, 0
        try:
            with open(path, "r") as f:
                data = json.load(f)
            # Validate the video hasn't changed
            stat = os.stat(audio_path)
            if data.get("video_mtime") != stat.st_mtime or data.get("video_size") != stat.st_size:
                logger.info("checkpoint stale (video changed), re-aligning")
                return None, 0, 0
            if data.get("model") != self.model_size or data.get("language") != self.language:
                logger.info("checkpoint stale (model/language changed), re-aligning")
                return None, 0, 0
            words = [Word(text=w["text"], start=w["start"], end=w["end"], score=w["score"])
                     for w in data.get("words", [])]
            return words, data.get("completed_chunks", 0), data.get("total_chunks", 0)
        except Exception as e:
            logger.warning("checkpoint load failed: %s", e)
            return None, 0, 0

    def _save_checkpoint(self, audio_path: str, words, completed: int, total: int,
                         checkpoint_dir: str = None):
        """Save checkpoint (full or partial) to disk."""
        path = self._checkpoint_path(audio_path, checkpoint_dir)
        stat = os.stat(audio_path)
        data = {
            "video_mtime": stat.st_mtime,
            "video_size": stat.st_size,
            "model": self.model_size,
            "language": self.language,
            "backend": self.backend,
            "completed_chunks": completed,
            "total_chunks": total,
            "words": [{"text": w.text, "start": w.start, "end": w.end, "score": w.score}
                      for w in words],
        }
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(data, f)
        os.replace(tmp, path)

    # -- backend: whisperx -------------------------------------------------

    def _align_whisperx(self, audio_path: str) -> List[Word]:
        import whisperx  # type: ignore

        if self._model is None:
            self._model = whisperx.load_model(
                self.model_size, device=self.device, compute_type=self.compute_type
            )
        result = self._model.transcribe(audio_path, language=self.language,
                                        batch_size=8)
        # forced alignment pass
        align_model, align_meta = whisperx.load_align_model(
            self.language, device=self.device
        )
        aligned = whisperx.align(
            result["segments"], align_model, align_meta, audio_path,
            device=self.device,
        )
        words: List[Word] = []
        seg_cues: List[AlignedCue] = []
        for seg in aligned["segments"]:
            seg_text = str(seg.get("text", "")).strip()
            if seg_text:
                seg_cues.append(AlignedCue(
                    text=seg_text,
                    start=float(seg.get("start", 0.0)),
                    end=float(seg.get("end", 0.0)),
                ))
            for w in seg.get("words", []):
                # whisperx returns start/end as floats; some words carry a
                # score under "score" or "probability".
                score = float(w.get("score", w.get("probability", 1.0)))
                words.append(Word(
                    text=str(w.get("word", "")).strip(),
                    start=float(w.get("start", 0.0)),
                    end=float(w.get("end", 0.0)),
                    score=score,
                ))
        self.last_segments = seg_cues
        return [w for w in words if w.text]

    # -- backend: stable-ts ------------------------------------------------

    def _align_stable_ts(self, audio_path: str) -> List[Word]:
        import stable_whisper  # type: ignore

        if self._model is None:
            # stable-ts has a dedicated faster-whisper loader: it downloads
            # the model from HuggingFace (so HF_ENDPOINT mirror works) and is
            # much faster on CPU than the openai-whisper backend (which pulls
            # from OpenAI's S3 directly and ignores the mirror).
            # cpu_threads is forwarded to faster-whisper's WhisperModel.
            load_kwargs = {"device": self.device}
            if self.cpu_threads:
                load_kwargs["cpu_threads"] = self.cpu_threads
            self._model = stable_whisper.load_faster_whisper(
                self.model_size, **load_kwargs
            )
        result = self._model.transcribe(audio_path, language=self.language)
        # segment_to_word_level gives per-word start/end.
        words_data = result.segments_to_words() if hasattr(result, "segments_to_words") \
            else _stable_ts_extract_words(result)
        words: List[Word] = []
        for w in words_data:
            words.append(Word(
                text=str(w.get("word", "")).strip(),
                start=float(w.get("start", 0.0)),
                end=float(w.get("end", 0.0)),
                score=float(w.get("probability", 1.0)),
            ))
        # Preserve stable-ts segments (each segment has .text/.start/.end).
        seg_cues: List[AlignedCue] = []
        for seg in getattr(result, "segments", []) or []:
            seg_text = str(getattr(seg, "text", "")).strip()
            if seg_text:
                seg_cues.append(AlignedCue(
                    text=seg_text,
                    start=float(getattr(seg, "start", 0.0)),
                    end=float(getattr(seg, "end", 0.0)),
                ))
        self.last_segments = seg_cues
        return [w for w in words if w.text]


# -- helpers --------------------------------------------------------------

def _probe(name: str) -> bool:
    """Return True if a backend's deps are importable."""
    if name == "whisperx":
        try:
            import whisperx  # noqa: F401
            return True
        except Exception:
            return False
    if name == "stable_ts":
        try:
            import stable_whisper  # noqa: F401
            return True
        except Exception:
            return False
    return False


def _stable_ts_extract_words(result) -> List[dict]:
    """Best-effort extraction of word dicts from a stable-ts result object.

    stable-ts returns a result with ``segments`` whose ``.words`` are objects
    exposing ``.word/.start/.end/.probability``. Convert to plain dicts.
    """
    out: List[dict] = []
    for seg in getattr(result, "segments", []) or []:
        for w in getattr(seg, "words", []) or []:
            out.append({
                "word": getattr(w, "word", ""),
                "start": getattr(w, "start", 0.0),
                "end": getattr(w, "end", 0.0),
                "probability": getattr(w, "probability", 1.0),
            })
    return out
