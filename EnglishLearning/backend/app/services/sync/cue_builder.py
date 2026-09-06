"""Builds :class:`AlignedCue` objects from raw word alignments.

Bridge between :mod:`word_aligner` (per-word timestamps) and
:mod:`chunk_stitcher` (per-chunk TTS fitting). Groups the flat word stream
into complete sentences, attaching each sentence's word span and the
inter-sentence pause, exactly as described in
``docs/TTS_SYNC_MECHANISM.md`` §4.4.

Post-processing: tiny trailing cues (fewer than 3 words, like "of code.")
are merged into the previous cue. This handles cases where Whisper
incorrectly inserts a sentence-final period mid-sentence (e.g. "block."
before "of code"), which would otherwise create a 2-word cue fragment.
"""

from __future__ import annotations

import re
from typing import List

from .models import AlignedCue, Word

# A word ending with sentence-final punctuation closes the current sentence.
_SENTENCE_END = re.compile(r"[.!?]\s*$")

# Minimum number of words for a standalone cue. Shorter cues are merged
# into the previous one to avoid fragments like "of code." on screen.
_MIN_CUE_WORDS = 3


def build_aligned_cues(words: List[Word], total_duration: float) -> List[AlignedCue]:
    """Group words into sentences, preserving word-level alignment.

    Args:
        words: flat word stream from :class:`WordAligner`, ordered by time.
        total_duration: total source audio length (seconds); the last cue's
            ``end`` is clamped to this if it ran past.

    Returns:
        List of :class:`AlignedCue`, each carrying the sentence text, its
        ``[start, end]`` span, its words, and the silence before the next
        sentence (``pause_after``).
    """
    cues: List[AlignedCue] = []
    current_words: List[Word] = []

    def flush():
        if not current_words:
            return
        start = current_words[0].start
        end = current_words[-1].end
        text = " ".join(w.text for w in current_words).strip()
        cues.append(AlignedCue(
            text=text,
            start=start,
            end=end,
            words=list(current_words),
        ))
        current_words.clear()

    for w in words:
        current_words.append(w)
        if _SENTENCE_END.search(w.text):
            flush()
    flush()  # trailing words with no terminal punctuation

    if not cues:
        return []

    # ── Merge tiny trailing cues ──────────────────────────────────────
    # If a cue has fewer than _MIN_CUE_WORDS words, it's likely a fragment
    # caused by incorrect sentence-boundary detection (e.g. Whisper placing
    # a period mid-sentence like "block." before "of code"). Merge it into
    # the previous cue to avoid tiny on-screen fragments.
    merged: List[AlignedCue] = []
    for cue in cues:
        if len(cue.words or []) < _MIN_CUE_WORDS and merged:
            prev = merged[-1]
            combined_words = list(prev.words or [])

            # Strip sentence-final punctuation from the previous cue's
            # last word — the period was likely a Whisper hallucination
            # that incorrectly triggered sentence-end. The merged cue's
            # final word retains its own punctuation.
            if combined_words:
                last = combined_words[-1]
                cleaned_text = re.sub(r"[.!?]+$", "", last.text)
                combined_words[-1] = Word(
                    text=cleaned_text,
                    start=last.start,
                    end=last.end,
                    score=last.score,
                )

            combined_words += (cue.words or [])
            combined_text = " ".join(w.text for w in combined_words).strip()
            merged[-1] = AlignedCue(
                text=combined_text,
                start=prev.start,
                end=cue.end,
                words=combined_words,
            )
        else:
            merged.append(cue)
    cues = merged

    # Clamp the final cue's end to the total duration if it ran over.
    if cues[-1].end > total_duration:
        cues[-1].end = total_duration

    # Compute inter-sentence pause: silence between the end of one sentence's
    # last word and the start of the next sentence's first word.
    for i in range(len(cues) - 1):
        cues[i].pause_after = max(0.0, cues[i + 1].start - cues[i].end)

    return cues
