"""Shared data models for the sync-aware dubbing pipeline.

These dataclasses flow between :mod:`word_aligner` (which produces word-level
timestamps from the original audio) and :mod:`chunk_stitcher` (which fits TTS
audio into the original rhythm). Keeping them in one place avoids circular
imports between the two modules.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List


@dataclass
class Word:
    """A single word with its original-audio time span.

    ``start``/``end`` are seconds relative to the start of the source audio.
    ``score`` is the aligner confidence in ``[0, 1]`` (1.0 for the uniform
    interpolation fallback which has no real confidence).
    """

    text: str
    start: float
    end: float
    score: float = 1.0

    @property
    def duration(self) -> float:
        return max(0.0, self.end - self.start)


@dataclass
class Chunk:
    """A breath-group: a contiguous run of words spoken without a pause.

    Chunks are the unit of TTS synthesis and time-stretching. Splitting a
    sentence into chunks (rather than synthesising the whole sentence at
    once) lets us preserve the intra-sentence pauses that the original speaker
    used, which is what makes the dubbed audio feel in rhythm.
    """

    words: List[Word] = field(default_factory=list)

    @property
    def text(self) -> str:
        return " ".join(w.text for w in self.words).strip()

    @property
    def start(self) -> float:
        return self.words[0].start if self.words else 0.0

    @property
    def end(self) -> float:
        return self.words[-1].end if self.words else 0.0

    @property
    def duration(self) -> float:
        return max(0.0, self.end - self.start)


@dataclass
class AlignedCue:
    """A complete sentence: its word-level alignment plus the sentence span.

    ``start``/``end`` cover the whole sentence (including any leading/trailing
    silence captured by Whisper). ``chunks`` break the spoken part into
    breath-groups. ``pause_after`` is the silence between the end of this
    sentence's last word and the start of the next sentence's first word —
    used by the stitcher to reproduce natural inter-sentence gaps.
    """

    text: str
    start: float
    end: float
    words: List[Word] = field(default_factory=list)
    chunks: List[Chunk] = field(default_factory=list)
    pause_after: float = 0.0
