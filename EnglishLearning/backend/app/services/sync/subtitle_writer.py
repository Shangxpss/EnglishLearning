"""Subtitle (SRT) generation from aligned cues.

The dubbing pipeline already produces word-level alignment
(:class:`WordAligner`) and sentence-level cues (:func:`build_aligned_cues`).
Each :class:`AlignedCue` carries ``text``, ``start``, ``end`` — exactly the
three fields an SRT subtitle entry needs. So subtitle generation is
essentially free: we already have the data, we just format it.

This module writes **two** kinds of subtitle:

1. **Sentence-level SRT** (``write_srt``) — one entry per sentence.
   Standard subtitle format. Good for reading while watching.

2. **Word-level SRT** (``write_word_srt``) — one entry per word.
   Useful for karaoke-style highlighting in the English Learning app
   (each word lights up as it's spoken). Much denser than sentence-level.

Both are written from the **same alignment pass** — no extra model load,
no extra network round-trips. The subtitles match the original English
transcript; since the dub follows the original timing (that's the whole
point of the sync pipeline), the same SRT works for both original and
dubbed audio.

Format reference: https://en.wikipedia.org/wiki/SubRip
"""

from __future__ import annotations

import logging
import re
from typing import List, Optional

from .models import AlignedCue, Word

logger = logging.getLogger(__name__)


def _format_srt_timestamp(seconds: float) -> str:
    """Format seconds as ``HH:MM:SS,mmm`` (SRT timestamp format).

    SRT uses a comma (not period) before the milliseconds, and zero-pads
    each field. Negative or NaN values clamp to 0.
    """
    if seconds is None or seconds != seconds or seconds < 0:
        seconds = 0.0
    total_ms = int(seconds * 1000)
    hours, rem = divmod(total_ms, 3_600_000)
    minutes, rem = divmod(rem, 60_000)
    secs, ms = divmod(rem, 1000)
    return f"{hours:02d}:{minutes:02d}:{secs:02d},{ms:03d}"


# ── subtitle segmentation ─────────────────────────────────────────────────
#
# When a sentence is very long (e.g. a run-on with no punctuation), the
# resulting SRT entry covers too much screen time and visually overflows the
# video player. ``split_cues_for_subtitles`` breaks long cues into smaller
# subtitle segments at natural word boundaries, while keeping each segment's
# timestamps word-aligned. This only affects subtitle output — TTS synthesis
# still receives the original (unsplit) cues so audio quality is preserved.
#
# Splitting follows Netflix Timed Text Style Guide best practices:
#   - 42 chars per line, max 2 lines → ~84 chars per subtitle entry
#   - Split at punctuation marks (comma, semicolon, colon, dash)
#   - Break before conjunctions (and, but, or, so, because, ...)
#   - Never leave a fragment of 1-2 words alone on a line
#   - If no punctuation available, split as evenly as possible
#   - Don't split short sentences even if word count exceeds the limit


# Netflix standard: 42 chars/line × 2 lines = 84 chars max per subtitle entry.
# Below this, the text fits comfortably without splitting.
MAX_CHARS_PER_ENTRY = 84

# Soft break points: commas, semicolons, colons, em-dashes, hyphens,
# AND sentence-ending punctuation (period, question mark, exclamation,
# closing quotes/parens). Including periods is critical: without it the
# splitter can't see sentence boundaries and glues words like "Think of it"
# onto the wrong sentence (see docs/SUBTITLE_SPLIT_MECHANISM.md §6.5).
_SOFT_BREAK = re.compile(r"[,;:—\-.*?!\u201d\u2019\u201c\u2018)'\]]$")
# Sentence-ending punctuation. Used by ``_merge_tiny_fragments`` to detect
# when the previous segment already ends a complete sentence: in that case
# a tiny trailing fragment is the START of the next sentence and must NOT
# be glued back onto the previous one (the original "Think of it" bug).
_SENTENCE_END = re.compile(r"[.?!][\u201d\u2019\u201c\u2018)'\"]*$")
_CONJUNCTIONS = {"and", "but", "or", "so", "because", "when", "while",
                 "if", "though", "although", "whereas", "as"}


def _find_split_index(words: List[Word], max_words: int) -> int:
    """Find the best word index to split a long cue at.

    Prefers a soft break point (punctuation) near the midpoint of the
    segment; falls back to a conjunction; falls back to a hard midpoint.

    Returns the split index such that ``words[:split_at]`` is the first
    segment. When a word at index ``i`` has a soft break (e.g. ``"call,"``),
    we return ``i + 1`` so the punctuation word stays in the first segment.
    This avoids leaving fragments like ``"With"`` or ``"whole."`` alone.
    """
    mid = max_words // 2
    # Search window: look for a soft break within [1, max_words].
    # Prefer the one closest to the midpoint.
    best = -1
    best_dist = max_words
    upper = min(len(words), max_words)
    for i in range(1, upper):
        w = words[i].text
        if _SOFT_BREAK.search(w) or w.lower() in _CONJUNCTIONS:
            # +1: keep the punctuation word in the first segment
            split_at = i + 1 if _SOFT_BREAK.search(w) else i
            dist = abs(split_at - mid)
            if dist < best_dist:
                best = split_at
                best_dist = dist
    if best > 0:
        return best
    # No soft break found — split at the hard cap.
    return min(max_words, len(words))


def _count_chars(words: List[Word]) -> int:
    """Count total characters in a word list (including spaces)."""
    return len(" ".join(w.text for w in words))


def split_cues_for_subtitles(
    cues: List[AlignedCue],
    max_words_per_segment: int = 0,
    max_tail_extend: float = 2.5,
) -> List[AlignedCue]:
    """Split long cues into shorter subtitle segments.

    Uses a hybrid word-count + character-count approach following Netflix
    Timed Text Style Guide best practices:

    1. **Character limit (primary)**: If a cue's text fits within
       ``MAX_CHARS_PER_ENTRY`` (84 chars = 42 CPL × 2 lines), it is kept
       as-is — even if the word count exceeds ``max_words_per_segment``.
       This prevents splitting short sentences unnecessarily.

    2. **Punctuation-based splitting**: When a cue must be split, the
       algorithm searches for the best punctuation mark (comma, semicolon,
       colon, dash) or conjunction near the midpoint and splits there.
       The punctuation word stays in the first segment.

    3. **Even splitting**: If no punctuation is found, the text is split
       into the minimum number of segments needed, as evenly as possible
       at word boundaries.

    4. **No tiny fragments**: After splitting, any segment with fewer
       than 3 words or fewer than 15 characters is merged back into the
       previous segment.

    Args:
        cues: original sentence-level cues (from :func:`build_aligned_cues`).
        max_words_per_segment: maximum number of words per subtitle entry.
            0 or negative = no splitting (return cues unchanged). A
            reasonable value is 8–12 words per segment.
        max_tail_extend: maximum seconds to extend the *last* segment of
            each cue so it doesn't vanish the instant speech ends.

    Returns:
        A new list of cues. Short cues are kept as-is; longer cues are
        split into multiple segments.
    """
    if not max_words_per_segment or max_words_per_segment <= 0:
        return list(cues)

    out: List[AlignedCue] = []
    n = len(cues)
    for ci, cue in enumerate(cues):
        words = cue.words or []
        next_start = cues[ci + 1].start if ci + 1 < n else None
        tail_end = _compute_tail_end(cue.end, next_start, max_tail_extend)

        # Don't split if text fits within the character limit, even if
        # word count exceeds max_words_per_segment. A 10-word sentence
        # (~60 chars) fits easily in one subtitle entry.
        if len(words) <= max_words_per_segment or _count_chars(words) <= MAX_CHARS_PER_ENTRY:
            out.append(AlignedCue(
                text=cue.text,
                start=cue.start,
                end=tail_end,
                words=words,
                pause_after=cue.pause_after,
            ))
            continue

        # Split this cue's words into chunks, preferring natural break
        # points. Keep splitting until the remaining text fits.
        remaining = list(words)
        while len(remaining) > max_words_per_segment and _count_chars(remaining) > MAX_CHARS_PER_ENTRY:
            split_at = _find_split_index(remaining, max_words_per_segment)
            seg_words = remaining[:split_at]
            remaining = remaining[split_at:]

            text = " ".join(w.text for w in seg_words).strip()
            # Extend this split segment's end into the silence before the
            # next split segment starts (so subtitles don't vanish during
            # intra-cue pauses, e.g. between "represents," and "and most
            # importantly,"). Same logic as `_compute_tail_end` but uses
            # the next split segment's first word as the boundary. Capped
            # at max_tail_extend so we don't bridge very long pauses.
            seg_end = seg_words[-1].end
            if remaining:
                next_seg_start = remaining[0].start
                seg_end = _compute_tail_end(seg_end, next_seg_start,
                                            max_tail_extend)
            out.append(AlignedCue(
                text=text,
                start=seg_words[0].start,
                end=seg_end,
                words=seg_words,
            ))

        # Last segment (the remaining words) — extend its end into the
        # silence before the next cue so it doesn't disappear too fast.
        if remaining:
            text = " ".join(w.text for w in remaining).strip()
            out.append(AlignedCue(
                text=text,
                start=remaining[0].start,
                end=tail_end,
                words=remaining,
            ))

    # ── Merge tiny trailing fragments ────────────────────────────────
    # If the last segment of a split cue is very short (< 3 words or
    # < 15 chars), merge it into the previous segment. This prevents
    # fragments like "whole." or "of code." from being alone on screen.
    out = _merge_tiny_fragments(out)

    return out


def _merge_tiny_fragments(segments: List[AlignedCue]) -> List[AlignedCue]:
    """Merge tiny trailing segments (< 3 words or < 15 chars) into the previous.

    Walks the list and merges any segment that is too small into the one
    before it. This is applied after splitting to clean up fragments like
    "whole." or "of code." that would otherwise appear alone on screen.

    Sentence-boundary guard: when the previous segment already ends with
    sentence punctuation (``.?!``), a tiny fragment is the beginning of the
    next sentence — gluing it back would recreate the "Think of it" bug.
    In that case the tiny fragment is kept as its own entry so the sentence
    boundary stays intact.
    """
    if len(segments) < 2:
        return segments

    merged: List[AlignedCue] = []
    for seg in segments:
        is_tiny = len(seg.words or []) < 3 or len(seg.text or "") < 15
        if is_tiny and merged:
            prev = merged[-1]
            prev_text = (prev.text or "").rstrip()
            # If the previous segment ends a sentence, the tiny fragment is
            # the start of the NEXT sentence — don't glue them together.
            if _SENTENCE_END.search(prev_text):
                merged.append(seg)
                continue
            # Merge into previous segment
            combined_words = (prev.words or []) + (seg.words or [])
            combined_text = " ".join(w.text for w in combined_words).strip()
            merged[-1] = AlignedCue(
                text=combined_text,
                start=prev.start,
                end=seg.end,  # extend to include the fragment's end
                words=combined_words,
                pause_after=seg.pause_after,
            )
        else:
            merged.append(seg)

    return merged


def _compute_tail_end(cue_end: float, next_start: Optional[float],
                      max_extend: float = 2.5) -> float:
    """Return the end timestamp for the last segment of a cue.

    Extends ``cue_end`` forward to fill the silence before the next cue's
    first voice, but never more than ``max_extend`` seconds and never past
    the next cue's start. When subtitles are burned into the video frames
    (hard subtitles), there is no player-side cue-swap latency, so no
    overlap or gap adjustment is needed — we just fill natural silence so
    the subtitle doesn't vanish the instant speech stops.
    """
    if next_start is None:
        return cue_end
    gap = next_start - cue_end
    if gap <= 0:
        # Cues overlap or are back-to-back. Leave cue_end as-is so the
        # next cue starts exactly when scheduled.
        return cue_end
    extend = min(gap, max_extend)
    if extend <= 0:
        return cue_end
    return cue_end + extend


def write_srt(
    cues: List[AlignedCue],
    srt_path: str,
    max_words_per_segment: int = 0,
) -> int:
    """Write sentence-level SRT subtitles from aligned cues.

    Each cue becomes one SRT entry. The entry's text is the cue's sentence
    text; the timestamps are the cue's ``start``/``end`` (word-aligned).

    Args:
        cues: aligned sentence cues from :func:`build_aligned_cues`.
        srt_path: output ``.srt`` file path.
        max_words_per_segment: if > 0, long cues are split into shorter
            subtitle entries so they don't visually overflow the video
            player. A reasonable value is 8–12 words. 0 = no splitting.

    Returns:
        Number of subtitle entries written.
    """
    segments = split_cues_for_subtitles(cues, max_words_per_segment)

    entries = 0
    with open(srt_path, "w", encoding="utf-8") as f:
        for i, cue in enumerate(segments, start=1):
            text = (cue.text or "").strip()
            if not text:
                continue
            # Ensure end >= start + 0.1s (SRT players may drop zero-length
            # entries). Also clamp end to be at least start.
            start = cue.start
            end = max(cue.end, start + 0.1)
            f.write(f"{i}\n")
            f.write(f"{_format_srt_timestamp(start)} --> "
                    f"{_format_srt_timestamp(end)}\n")
            f.write(f"{text}\n\n")
            entries += 1
    logger.info("write_srt: %d entries (max_words=%d) -> %s",
                entries, max_words_per_segment, srt_path)
    return entries


def write_word_srt(words: List[Word], srt_path: str,
                   group_size: int = 1) -> int:
    """Write word-level SRT subtitles (for karaoke-style highlighting).

    Each word (or group of ``group_size`` words) becomes one SRT entry.
    This is denser than sentence-level — useful for the English Learning app
    where each word lights up as it's spoken.

    Args:
        words: word-level alignment from :class:`WordAligner`.
        srt_path: output ``.srt`` file path.
        group_size: number of words per subtitle entry. 1 = one word per
            entry (densest, best for highlighting). 3-5 reduces flicker for
            fast speech.

    Returns:
        Number of subtitle entries written.
    """
    if group_size < 1:
        group_size = 1
    entries = 0
    with open(srt_path, "w", encoding="utf-8") as f:
        idx = 0
        i = 0
        while i < len(words):
            group = words[i:i + group_size]
            text = " ".join(w.text for w in group).strip()
            if not text:
                i += group_size
                continue
            start = group[0].start
            end = group[-1].end
            if end <= start:
                end = start + 0.1
            idx += 1
            f.write(f"{idx}\n")
            f.write(f"{_format_srt_timestamp(start)} --> "
                    f"{_format_srt_timestamp(end)}\n")
            f.write(f"{text}\n\n")
            entries += 1
            i += group_size
    logger.info("write_word_srt: %d entries (group_size=%d) -> %s",
                entries, group_size, srt_path)
    return entries


def write_segments_srt(segments: List[AlignedCue], srt_path: str) -> int:
    """Write Whisper's **raw segment-level** SRT, one entry per segment.

    This preserves the segments exactly as Whisper produced them — no
    cue-rebuilding, no splitting, no fragment-merging. Useful for:

      * Diagnosing subtitle quality: compare the raw Whisper output (this
        file) against the rebuilt + split output (``<stem>.srt``) to see
        what our cue_builder and splitter actually changed.
      * Using Whisper's natural pause-aligned boundaries directly, when the
        rebuilt cues feel worse than the original.

    Segments come from ``WordAligner.last_segments`` (populated during
    ``align()``). Each segment's ``text`` is Whisper's verbatim decoded
    text; ``start``/``end`` are Whisper's segment-level timestamps.

    Args:
        segments: ``List[AlignedCue]`` from ``WordAligner.last_segments``.
            Only ``text``, ``start``, ``end`` are used; ``words`` may be
            empty (Whisper segments have no word-level detail in this
            representation).
        srt_path: output ``.srt`` file path.

    Returns:
        Number of subtitle entries written.
    """
    entries = 0
    with open(srt_path, "w", encoding="utf-8") as f:
        for i, seg in enumerate(segments, start=1):
            text = (seg.text or "").strip()
            if not text:
                continue
            start = seg.start
            # Clamp end to be at least start + 0.1s so players don't drop
            # very short segments. Whisper segments always have a non-zero
            # span, but the clamp is cheap insurance.
            end = max(seg.end, start + 0.1)
            f.write(f"{i}\n")
            f.write(f"{_format_srt_timestamp(start)} --> "
                    f"{_format_srt_timestamp(end)}\n")
            f.write(f"{text}\n\n")
            entries += 1
    logger.info("write_segments_srt: %d entries -> %s", entries, srt_path)
    return entries


def cues_to_srt_string(cues: List[AlignedCue]) -> str:
    """Return the SRT content as a string (instead of writing to a file).

    Useful for embedding in MP4 containers or sending over the web API.
    """
    import io
    buf = io.StringIO()
    entries = 0
    for i, cue in enumerate(cues, start=1):
        text = (cue.text or "").strip()
        if not text:
            continue
        start = cue.start
        end = max(cue.end, start + 0.1)
        buf.write(f"{i}\n")
        buf.write(f"{_format_srt_timestamp(start)} --> "
                  f"{_format_srt_timestamp(end)}\n")
        buf.write(f"{text}\n\n")
        entries += 1
    return buf.getvalue()
