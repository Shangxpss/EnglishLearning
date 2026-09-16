"""Symmetric per-chunk time-stretching with pause preservation.

This is the fix for the sync drift in ``run_chattts_dub.py``.

The original stitcher had two bugs (see ``docs/TTS_SYNC_MECHANISM.md`` §2.3):

  1. **Asymmetric stretch**: it only *compressed* TTS that overflowed its slot
     (capped at 1.3x) and never *expanded* TTS that was shorter — so the
     dubbed speaking rate was systematically slower than the original, and
     any overflow beyond 1.3x bled into the next cue causing cumulative drift.

  2. **Erased intra-sentence pauses**: sentence merging widened each slot to
     span several Whisper segments, and the TTS for the whole merged sentence
     played as one continuous utterance — losing the natural breath pauses the
     original speaker used inside the sentence.

This module fixes both:

  * ``split_into_chunks`` breaks a sentence's words into **breath-group
    chunks** at punctuation and at silence gaps, so each chunk is short enough
    that a 0.85-1.25x stretch keeps it inside the perceptually-safe window.

  * ``fit_chunk`` stretches the TTS audio **symmetrically** to exactly fill
    the original chunk's duration — so the dubbed speech runs at the same
    speed as the original speaker.

  * ``stitch`` places each chunk at its original start time and leaves the
    original inter-chunk pauses as silence — reproducing the original rhythm.

Only needs numpy + librosa + soundfile (no heavy ML deps).
"""

from __future__ import annotations

import logging
import re
from typing import List, Optional

import numpy as np

from .models import AlignedCue, Chunk, Word

logger = logging.getLogger(__name__)

# Perceptually safe time-stretch window for librosa's phase vocoder.
# Below 0.8 the voice starts to sound "chipmunk"; above 1.25 it sounds slurred.
MIN_STRETCH_RATIO = 0.85
MAX_STRETCH_RATIO = 1.25

# A pause longer than this (seconds) between two words splits a new chunk —
# the original speaker took a breath / hesitated here.
DEFAULT_PAUSE_SPLIT = 0.25

# Split chunks at these punctuation marks too (end of a clause).
_CLAUSE_END = re.compile(r"[.,;:!?]$")

# Tiny fade to avoid clicks at chunk boundaries.
_FADE_S = 0.015


def split_into_chunks(words: List[Word],
                      pause_split: float = DEFAULT_PAUSE_SPLIT) -> List[Chunk]:
    """Break a sentence's words into breath-group chunks.

    A new chunk starts when:
      * the silence between two consecutive words exceeds ``pause_split``, or
      * the previous word ended with clause punctuation (`,`, `;`, etc.).
    """
    chunks: List[Chunk] = []
    current: List[Word] = []

    def flush():
        if current:
            chunks.append(Chunk(words=list(current)))
            current.clear()

    for i, w in enumerate(words):
        if current:
            prev = current[-1]
            gap = w.start - prev.end
            clause_break = bool(_CLAUSE_END.search(prev.text))
            if gap > pause_split or clause_break:
                flush()
        current.append(w)
    flush()
    return chunks


def fit_chunk(audio: np.ndarray, target_samples: int,
              min_ratio: float = MIN_STRETCH_RATIO,
              max_ratio: float = MAX_STRETCH_RATIO) -> np.ndarray:
    """Stretch/truncate ``audio`` to exactly ``target_samples`` length.

    The stretch ratio is clamped to ``[min_ratio, max_ratio]`` so the voice
    stays natural. If even at the clamp the audio is still the wrong length we
    pad with silence (when too short after max stretch) or truncate (when too
    long after min stretch) rather than degrading the voice further.
    """
    if target_samples <= 0:
        return np.zeros(0, dtype=np.float32)
    audio = np.asarray(audio, dtype=np.float32).squeeze()
    if audio.size == 0:
        return np.zeros(target_samples, dtype=np.float32)

    if audio.size == target_samples:
        return audio

    ratio = audio.size / target_samples
    clamped = max(min_ratio, min(ratio, max_ratio))

    if abs(clamped - 1.0) > 1e-3:
        # lazy import librosa (it is a heavy import)
        import librosa  # type: ignore
        audio = librosa.effects.time_stretch(audio, rate=clamped)

    # After stretch, exact-fit: pad or truncate.
    if audio.size >= target_samples:
        return audio[:target_samples].astype(np.float32)
    padded = np.zeros(target_samples, dtype=np.float32)
    padded[:audio.size] = audio
    return padded


def _apply_fade(audio: np.ndarray, sample_rate: int, fade_s: float = _FADE_S) -> np.ndarray:
    n = int(fade_s * sample_rate)
    if audio.size <= 2 * n or n <= 0:
        return audio
    out = audio.copy()
    out[:n] *= np.linspace(0.0, 1.0, n, dtype=np.float32)
    out[-n:] *= np.linspace(1.0, 0.0, n, dtype=np.float32)
    return out


class ChunkStitcher:
    """Builds a rhythm-faithful final waveform from per-chunk TTS audio.

    Usage::

        stitcher = ChunkStitcher(sample_rate=24000)
        final = stitcher.stitch(cue_to_chunk_audio, cues, total_duration)
        soundfile.write("dubbed.wav", final, 24000)
    """

    def __init__(self, sample_rate: int,
                 min_ratio: float = MIN_STRETCH_RATIO,
                 max_ratio: float = MAX_STRETCH_RATIO,
                 pause_split: float = DEFAULT_PAUSE_SPLIT):
        self.sample_rate = sample_rate
        self.min_ratio = min_ratio
        self.max_ratio = max_ratio
        self.pause_split = pause_split

    # -- chunking ----------------------------------------------------------

    def build_chunks(self, cue: AlignedCue) -> List[Chunk]:
        """Derive breath-group chunks for a cue from its aligned words."""
        if not cue.words:
            return []
        return split_into_chunks(cue.words, pause_split=self.pause_split)

    # -- stitching --------------------------------------------------------

    def stitch(self,
               chunk_audio: dict,
               cues: List[AlignedCue],
               total_duration: float,
               normalize: bool = True) -> np.ndarray:
        """Assemble the final audio track.

        Args:
            chunk_audio: mapping ``(cue_index, chunk_index) -> np.ndarray`` of
                TTS audio for each chunk. Missing entries become silence.
            cues: the aligned cues (each must carry its ``chunks``).
            total_duration: total output length in seconds.
            normalize: peak-normalize to 0.95 if clipping.
        """
        sr = self.sample_rate
        total_samples = int(total_duration * sr) + sr  # +1s tail buffer
        final = np.zeros(total_samples, dtype=np.float32)

        n_compressed = n_expanded = n_missing = 0

        for ci, cue in enumerate(cues):
            if not cue.chunks:
                continue
            for chi, chunk in enumerate(cue.chunks):
                key = (ci, chi)
                audio = chunk_audio.get(key)
                if audio is None:
                    n_missing += 1
                    continue

                audio = np.asarray(audio, dtype=np.float32).squeeze()
                if audio.ndim > 1:
                    audio = audio[:, 0]

                target_samples = int(chunk.duration * sr)
                ratio = audio.size / target_samples if target_samples > 0 else 1.0

                fitted = fit_chunk(audio, target_samples,
                                   min_ratio=self.min_ratio,
                                   max_ratio=self.max_ratio)
                fitted = _apply_fade(fitted, sr)

                # diagnostics: which direction did we stretch?
                eff = fitted.size / max(audio.size, 1)
                if ratio > 1.05:
                    n_compressed += 1
                elif ratio < 0.95:
                    n_expanded += 1

                # Place at the chunk's original start time. Gaps between
                # chunks stay as zeros = natural silence (pause preserved).
                start_sample = int(chunk.start * sr)
                end_sample = min(start_sample + fitted.size, total_samples)
                if start_sample >= total_samples:
                    continue
                final[start_sample:end_sample] = fitted[:end_sample - start_sample]

        if normalize:
            peak = float(np.max(np.abs(final))) if final.size else 0.0
            if peak > 0.95:
                final *= 0.95 / peak

        logger.info(
            "ChunkStitcher: chunks stretched=%d compressed=%d expanded=%d "
            "missing=%d, final=%.2fs",
            n_compressed + n_expanded, n_compressed, n_expanded, n_missing,
            len(final) / sr,
        )
        return final


class SentenceStitcher:
    """Natural-prosody stitcher: synthesise per-sentence, stretch gently.

    This is the **production** stitcher. It trades perfect word-level sync
    for natural speech quality, which empirical testing showed is what users
    actually want:

      * ChatTTS receives the **whole sentence** so it can produce coherent
        intonation across the sentence (no fragmented prosody).
      * The sentence's TTS audio is placed at the sentence's accurate
        word-aligned ``start`` time (from WordAligner), and **only compressed
        gently** when it would overflow the sentence slot — never expanded.
        When TTS is shorter than the slot, the remaining time stays as natural
        silence (the pause the original speaker took).
      * Inter-sentence pauses are preserved exactly.

    Why not the per-chunk :class:`ChunkStitcher`? It split sentences into
    breath-group fragments (74 chunks for 23 sentences). Two failure modes
    appeared:

      1. ~46% of fragments developed phase-vocoder click artifacts because
         ``librosa.effects.time_stretch`` on sub-second audio is unreliable.
      2. ChatTTS produces garbled / near-silent output on sub-second
         fragments (no prosodic context), so words were effectively omitted.

    The :class:`SentenceStitcher` avoids both by keeping synthesis at the
    sentence level — the same granularity that made ``run_chattts_dub.py``
    sound natural — while still using the word-level alignment for accurate
    sentence placement and inter-sentence pause preservation.
    """

    def __init__(self, sample_rate: int,
                 max_compress: float = 1.3,
                 fade_s: float = 0.005):
        self.sample_rate = sample_rate
        self.max_compress = max_compress
        # 5 ms is enough to suppress end-clicks from direct-assignment
        # discontinuities; the previous 20 ms default was ramping the final
        # syllable of any sentence still phonating at the boundary to zero,
        # producing the "last syllable suddenly stops" effect. See
        # docs/TTS_SYNC_MECHANISM.md §10 for the empirical diagnosis.
        self.fade_s = fade_s

    def stitch(self,
               sentence_audio: dict,
               cues: List[AlignedCue],
               total_duration: float,
               normalize: bool = True) -> np.ndarray:
        """Assemble final audio from per-sentence TTS.

        Args:
            sentence_audio: mapping ``cue_index -> np.ndarray`` of TTS audio
                for the whole sentence.
            cues: aligned cues (word-level start/end used for placement).
            total_duration: total output length in seconds.
            normalize: peak-normalize to 0.95 if clipping.
        """
        sr = self.sample_rate
        total_samples = int(total_duration * sr) + sr
        final = np.zeros(total_samples, dtype=np.float32)

        n_compressed = n_natural_pause = n_missing = 0
        fade_n = max(1, int(self.fade_s * sr))

        for ci, cue in enumerate(cues):
            audio = sentence_audio.get(ci)
            if audio is None:
                n_missing += 1
                continue
            audio = np.asarray(audio, dtype=np.float32).squeeze()
            if audio.ndim > 1:
                audio = audio[:, 0]

            slot_dur = cue.end - cue.start
            slot_samples = int(slot_dur * sr)
            tts_dur = audio.size / sr
            start_sample = int(cue.start * sr)

            # Pause-aware compression. The previous logic compressed
            # *whenever* TTS overflowed the slot, regardless of whether the
            # inter-sentence pause could absorb the overflow. That meant 5/7
            # compressed sentences on the edge-tts run were being phase-vocoder
            # stretched for no reason — they fit naturally in the pause, but
            # the stitcher was still applying up to 1.30x compression, which
            # sounded metallic on edge-tts's clean neural audio.
            #
            # New rule: only compress when the overflow would actually
            # collide with the NEXT sentence's audio region. The available
            # absorption room is slot_dur + pause_after. The last cue has no
            # next sentence, so it never needs compression — its overflow
            # just extends past the end and is cut by ffmpeg `-shortest`.
            # See docs/TTS_SYNC_MECHANISM.md §10.4.
            is_last_cue = (ci == len(cues) - 1)
            available_room = slot_dur + (0.0 if is_last_cue else cue.pause_after)
            available_samples = int(available_room * sr)

            if audio.size > available_samples and available_samples > 0 and not is_last_cue:
                # Genuine collision — compress, but only by the amount needed
                # to fit the actual overflow, capped at max_compress.
                ratio_needed = audio.size / available_samples
                rate = min(ratio_needed, self.max_compress)
                if rate > 1.01:
                    import librosa  # lazy
                    audio = librosa.effects.time_stretch(audio, rate=rate)
                    n_compressed += 1
            else:
                # Either fits in slot, or overflows only into the
                # inter-sentence pause (no collision). Leave natural.
                gap = slot_dur - tts_dur
                if gap > 0.15:
                    n_natural_pause += 1
                elif audio.size > slot_samples:
                    # Overflows into pause but not past next sentence —
                    # count separately for diagnostics.
                    n_natural_pause += 1

            # Gentle fade-in to avoid a click at sentence start (always safe —
            # there's always silence or the previous sentence's tail before it).
            # Fade-out is conditional: only when the placed audio reaches all
            # the way to the NEXT sentence's start (i.e. fills or overflows
            # slot + pause_after), because only then can a non-zero sample be
            # directly overwritten by the next sentence's audio (a click). When
            # the TTS fits in the slot, or overflows only into the
            # inter-sentence pause without reaching the next cue, the next
            # sample is already zero so no fade-out is needed — and applying
            # one would attenuate the final syllable.
            # (See docs/TTS_SYNC_MECHANISM.md §10 — this was the root cause of
            # the "last syllable suddenly stops" bug.)
            if audio.size > 2 * fade_n:
                audio[:fade_n] *= np.linspace(0.0, 1.0, fade_n, dtype=np.float32)
                if audio.size >= available_samples:
                    audio[-fade_n:] *= np.linspace(1.0, 0.0, fade_n, dtype=np.float32)

            end_sample = min(start_sample + audio.size, total_samples)
            if start_sample >= total_samples:
                continue
            final[start_sample:end_sample] = audio[:end_sample - start_sample]

        if normalize:
            peak = float(np.max(np.abs(final))) if final.size else 0.0
            if peak > 0.95:
                final *= 0.95 / peak

        logger.info(
            "SentenceStitcher: sentences=%d compressed=%d natural_pause=%d "
            "missing=%d, final=%.2fs",
            len(cues), n_compressed, n_natural_pause, n_missing, len(final) / sr,
        )
        return final
