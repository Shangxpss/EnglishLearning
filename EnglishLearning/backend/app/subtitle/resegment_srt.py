#!/usr/bin/env python3
"""Re-segment SRT subtitles using word-level timestamps.

Reads .words.srt files for word-level timing, then groups words into
segments of at most MAX_WORDS_PER_SEGMENT words, writing new .srt files.

This fixes the problem where sentence-level SRT segments are too long
(e.g. 40+ words covering 25+ seconds), making subtitles unreadable.

Usage:
    python resegment_srt.py                     # process all .words.srt in kafka_dubbed/
    python resegment_srt.py --max-words 10      # custom max words per segment
    python resegment_srt.py --max-words 8 --dir /path/to/subtitles
"""

import argparse
import os
import re
import sys
from dataclasses import dataclass


# ── configuration ──────────────────────────────────────────────────────────
DEFAULT_MAX_WORDS = 12
DEFAULT_DIR = os.path.dirname(os.path.abspath(__file__))


# ── data structures ────────────────────────────────────────────────────────
@dataclass
class Word:
    text: str
    start_ms: int   # milliseconds
    end_ms: int


@dataclass
class Segment:
    index: int
    start_ms: int
    end_ms: int
    text: str


# ── SRT timestamp helpers ──────────────────────────────────────────────────
def ms_to_srt(ms: int) -> str:
    """Convert milliseconds → SRT timestamp 'HH:MM:SS,mmm'."""
    h = ms // 3_600_000
    ms %= 3_600_000
    m = ms // 60_000
    ms %= 60_000
    s = ms // 1_000
    ms %= 1_000
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def srt_to_ms(ts: str) -> int:
    """Convert SRT timestamp 'HH:MM:SS,mmm' → milliseconds."""
    ts = ts.strip().replace(",", ".")
    parts = re.split(r"[:.]", ts)
    h, m, s, ms = int(parts[0]), int(parts[1]), int(parts[2]), int(parts[3])
    return h * 3_600_000 + m * 60_000 + s * 1_000 + ms


# ── parse .words.srt ──────────────────────────────────────────────────────
def parse_words_srt(path: str) -> list[Word]:
    """Parse a .words.srt file into a list of Word with timestamps."""
    words = []
    with open(path, "r", encoding="utf-8") as f:
        content = f.read()

    blocks = content.strip().split("\n\n")
    for block in blocks:
        lines = block.strip().split("\n")
        if len(lines) < 3:
            continue
        # line 0 = index, line 1 = timestamp, line 2+ = text
        ts_line = lines[1]
        match = re.match(r"(\S+)\s*-->\s*(\S+)", ts_line)
        if not match:
            continue
        start_ms = srt_to_ms(match.group(1))
        end_ms = srt_to_ms(match.group(2))
        text = " ".join(lines[2:]).strip()
        if text:
            words.append(Word(text=text, start_ms=start_ms, end_ms=end_ms))
    return words


# ── sentence boundary detection ────────────────────────────────────────────
_SENT_END = re.compile(r'[.!?]["\')\]]*$')
_CLAUSE_BREAK = re.compile(r'[,;:—–-]$')


def _is_sentence_end(word: Word) -> bool:
    """True if this word ends a sentence (ends with . ! ? possibly + quote/paren)."""
    return bool(_SENT_END.search(word.text.strip()))


def _is_clause_break(word: Word) -> bool:
    """True if this word ends a clause (comma, semicolon, colon, dash)."""
    return bool(_CLAUSE_BREAK.search(word.text.strip()))


# ── re-segment ────────────────────────────────────────────────────────────
def resegment(words: list[Word], max_words: int) -> list[Segment]:
    """Group words into segments that respect sentence boundaries.

    Rules:
      1. A sentence-ending word (. ! ?) is always a hard break — the next
         segment starts fresh with the following word.
      2. Within a sentence, if we hit max_words, split at the best break
         point (prefer the last clause break within the window; otherwise
         just split at max_words).
      3. This ensures a segment never shows words from the next sentence
         before the audio has reached them.
    """
    segments = []
    idx = 1
    i = 0

    while i < len(words):
        # Collect words until we hit max_words or a sentence end
        chunk = [words[i]]
        j = i + 1

        while j < len(words) and len(chunk) < max_words:
            chunk.append(words[j])
            # If the word we just added ends a sentence, break here
            if _is_sentence_end(words[j]):
                j += 1
                break
            j += 1

        # If we stopped because of max_words (not sentence end),
        # try to split at the last clause break to keep phrasing natural.
        if len(chunk) == max_words and j <= len(words):
            # Only re-split if the last word is NOT already a sentence end
            if not _is_sentence_end(chunk[-1]):
                # Find the last clause break within the chunk (but not the
                # very last word — that would make a 1-word tail segment)
                best_split = -1
                for k in range(len(chunk) - 2, 0, -1):
                    if _is_clause_break(chunk[k]):
                        best_split = k + 1  # split AFTER this word
                        break
                if best_split > 0:
                    # Emit the part before the clause break
                    front = chunk[:best_split]
                    text = " ".join(w.text for w in front)
                    segments.append(Segment(
                        index=idx, start_ms=front[0].start_ms,
                        end_ms=front[-1].end_ms, text=text))
                    idx += 1
                    # Continue with the remainder
                    i += best_split
                    continue

        text = " ".join(w.text for w in chunk)
        start_ms = chunk[0].start_ms
        end_ms = chunk[-1].end_ms
        segments.append(Segment(index=idx, start_ms=start_ms,
                        end_ms=end_ms, text=text))
        idx += 1
        i = j

    return segments


def _make_continuous(segments: list[Segment], words: list[Word]) -> list[Segment]:
    """Ensure zero-gap timing between segments within a sentence.

    When a sentence is split across segments, the continuation segment
    starts at the **exact end time of the previous segment's last word**,
    not at the next word's start.  This eliminates any silent gap between
    speech and subtitle, removing perceived latency.
    """
    if len(segments) <= 1:
        return segments

    # Identify which segments are continuations of the same sentence
    is_continuation = [False] * len(segments)
    for i in range(1, len(segments)):
        prev_seg = segments[i - 1]
        last_word = None
        for w in words:
            if w.end_ms == prev_seg.end_ms:
                last_word = w
                break
        if last_word and not _is_sentence_end(last_word):
            is_continuation[i] = True

    adjusted = []
    for i, seg in enumerate(segments):
        new_start = seg.start_ms
        new_end = seg.end_ms

        # Continuation segment: start at previous segment's last word end
        if is_continuation[i] and adjusted:
            prev = adjusted[-1]
            # Start this segment at the previous segment's end (zero gap)
            new_start = prev.end_ms

        # If this segment doesn't end a sentence and next is a continuation,
        # extend its end to the next segment's first word start
        if i < len(segments) - 1 and is_continuation[i + 1]:
            new_end = segments[i + 1].start_ms

        adjusted.append(Segment(
            index=seg.index, start_ms=new_start,
            end_ms=new_end, text=seg.text))

    return adjusted


# ── write .srt ─────────────────────────────────────────────────────────────
def write_srt(segments: list[Segment], path: str) -> None:
    """Write segments as an SRT file."""
    with open(path, "w", encoding="utf-8") as f:
        for seg in segments:
            f.write(f"{seg.index}\n")
            f.write(f"{ms_to_srt(seg.start_ms)} --> {ms_to_srt(seg.end_ms)}\n")
            f.write(f"{seg.text}\n\n")


# ── process one .words.srt file ────────────────────────────────────────────
def process_file(words_srt_path: str, max_words: int) -> bool:
    """Re-segment a single .words.srt → .srt. Returns True if changed."""
    words = parse_words_srt(words_srt_path)
    if not words:
        print(f"  SKIP (no words): {words_srt_path}")
        return False

    segments = resegment(words, max_words)
    segments = _make_continuous(segments, words)

    # Write the new .srt next to the .words.srt
    srt_path = words_srt_path.replace(".words.srt", ".srt")
    write_srt(segments, srt_path)

    # Stats
    old_word_count = len(words)
    new_seg_count = len(segments)
    max_seg_words = max(
        len(s.text.split()) for s in segments
    )
    max_seg_dur = max(
        (s.end_ms - s.start_ms) / 1000.0 for s in segments
    )
    print(f"  {os.path.basename(srt_path)}: "
          f"{old_word_count} words → {new_seg_count} segments "
          f"(max {max_seg_words} words, max {max_seg_dur:.1f}s)")
    return True


# ── main ───────────────────────────────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(
        description="Re-segment SRT subtitles using word-level timestamps"
    )
    parser.add_argument(
        "--max-words", type=int, default=DEFAULT_MAX_WORDS,
        help=f"Max words per segment (default: {DEFAULT_MAX_WORDS})",
    )
    parser.add_argument(
        "--dir", type=str, default=DEFAULT_DIR,
        help=f"Root directory to process (default: kafka_dubbed/)",
    )
    args = parser.parse_args()

    root_dir = args.dir
    max_words = args.max_words

    print(f"Re-segmenting subtitles in: {root_dir}")
    print(f"Max words per segment: {max_words}")
    print()

    # Find all .words.srt files
    words_srt_files = []
    for dirpath, dirnames, filenames in os.walk(root_dir):
        for fn in filenames:
            if fn.endswith(".words.srt"):
                words_srt_files.append(os.path.join(dirpath, fn))
    words_srt_files.sort()

    if not words_srt_files:
        print("No .words.srt files found.")
        sys.exit(1)

    print(f"Found {len(words_srt_files)} .words.srt file(s)\n")

    changed = 0
    for path in words_srt_files:
        if process_file(path, max_words):
            changed += 1

    print(f"\nDone: {changed}/{len(words_srt_files)} files re-segmented")


if __name__ == "__main__":
    main()
