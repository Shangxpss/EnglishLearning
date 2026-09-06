#!/usr/bin/env python3 -u
"""Regenerate sentence-level .srt from existing .words.srt + .segments.srt.

This is a maintenance utility. Instead of re-running Whisper (stage 2 of
the dubbing pipeline), it parses the word-level alignment that was
already written as ``.words.srt`` and feeds it back into
``build_aligned_cues`` + ``write_srt``. This lets subtitle-split fixes
(new punctuation rules, sentence-boundary guards in
``_merge_tiny_fragments``, etc.) take effect without re-running any
ML stage — the dubbed audio and ``_dubbed.mp4`` files are untouched.

Inputs (per video stem):
  * ``{stem}.words.srt``    — word-level alignment (one entry per word)
  * ``{stem}.segments.srt`` — Whisper's raw segments (preserved as-is)

Output:
  * ``{stem}.srt``         — regenerated sentence-level subtitles

The ``.words.srt`` and ``.segments.srt`` files themselves are NEVER
modified by this script — they are the source of truth for the
alignment, and the splitter doesn't affect them anyway.

Usage::

    # Regenerate every .srt that has a matching .words.srt.
    python3 regenerate_srt.py

    # Dry run: report what would change without writing.
    python3 regenerate_srt.py --dry-run

    # Regenerate only one video (substring match on the file stem).
    python3 regenerate_srt.py --only "What is KIP"

    # Custom audio directory.
    python3 regenerate_srt.py --audio-dir /path/to/audio

    # Override the per-entry word cap (default 10).
    python3 regenerate_srt.py --max-words 12

    # Scan existing .srt files for the "glued fragment" bug only.
    python3 regenerate_srt.py --scan-only
"""

from __future__ import annotations

import argparse
import glob
import os
import re
import sys

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from app.services.sync import (  # noqa: E402
    build_aligned_cues,
    split_cues_for_subtitles,
    write_srt,
)
from app.services.sync.models import AlignedCue, Word  # noqa: E402

BACKEND = os.path.dirname(os.path.abspath(__file__))
DEFAULT_AUDIO_DIR = os.path.join(BACKEND, "audio")
DEFAULT_MAX_WORDS = 10


# ── SRT parsing helpers ────────────────────────────────────────────────────

# Generic SRT entry: index, start, end, text (may span multiple lines).
_SRT_ENTRY_RE = re.compile(
    r"(\d+)\s*\n"
    r"(\d{2}:\d{2}:\d{2},\d{3})\s*-->\s*(\d{2}:\d{2}:\d{2},\d{3})\s*\n"
    r"([^\n]+(?:\n[^\n]+)?)\n",
    re.MULTILINE,
)


def _srt_ts_to_seconds(ts: str) -> float:
    """Convert ``HH:MM:SS,mmm`` to seconds (float)."""
    h, m, s = ts.split(":")
    sec, ms = s.split(",")
    return int(h) * 3600 + int(m) * 60 + int(sec) + int(ms) / 1000.0


def parse_srt_entries(srt_path: str):
    """Yield (index, start_seconds, end_seconds, text) tuples."""
    if not os.path.exists(srt_path):
        return
    with open(srt_path, "r", encoding="utf-8") as f:
        content = f.read()
    for m in _SRT_ENTRY_RE.finditer(content):
        idx = int(m.group(1))
        start = _srt_ts_to_seconds(m.group(2))
        end = _srt_ts_to_seconds(m.group(3))
        text = m.group(4)
        yield idx, start, end, text


def count_srt_entries(srt_path: str) -> int:
    """Return the number of subtitle entries in an SRT file."""
    if not os.path.exists(srt_path):
        return 0
    with open(srt_path, "r", encoding="utf-8") as f:
        content = f.read()
    return len(_SRT_ENTRY_RE.findall(content))


def parse_word_srt(words_srt_path: str) -> list:
    """Parse a ``.words.srt`` into a list of :class:`Word` objects.

    Each SRT entry's text is expected to be a single word (or a small
    group of words when ``group_size`` > 1 was used at write time). For
    multi-word entries, the timestamp span is split evenly across the
    words — same fallback the ``interpolation`` aligner backend uses.
    """
    words: list = []
    for idx, start, end, text in parse_srt_entries(words_srt_path):
        text = text.strip()
        if not text:
            continue
        # ``write_word_srt`` with group_size=1 writes one word per entry.
        # If a multi-word entry shows up (older batches used group_size=3),
        # split it and interpolate timestamps across the words.
        tokens = text.split()
        if len(tokens) == 1:
            words.append(Word(text=tokens[0], start=start, end=end, score=0.0))
            continue
        if end <= start:
            end = start + 0.1
        per = (end - start) / len(tokens)
        for i, tok in enumerate(tokens):
            words.append(Word(
                text=tok,
                start=start + i * per,
                end=start + (i + 1) * per,
                score=0.0,
            ))
    return words


# ── diagnostic: detect the "glued fragment" bug ─────────────────────────────

def detect_glued_fragments(srt_path: str, min_glue_words: int = 2,
                           max_glue_words: int = 4) -> list:
    """Find entries whose text shows the "glued fragment" bug.

    The bug pattern: a single SRT entry's text contains a sentence-ending
    mark (``.`` ``?`` ``!``) followed by additional short text on the
    *same* entry — e.g. ``"... to Kafka. Think of it"``. The short text
    after the sentence end is the start of the next sentence and should
    be its own entry.
    """
    hits = []
    for idx, start, end, text in parse_srt_entries(srt_path):
        for m in re.finditer(r"[.?!](?=\s+\S)", text):
            tail = text[m.end():].strip()
            n = len(tail.split())
            if min_glue_words <= n <= max_glue_words:
                hits.append({
                    "entry": idx,
                    "timestamp_start": start,
                    "glue_text": tail,
                    "full_text": text,
                })
                break  # one hit per entry is enough
    return hits


def _count_glued_in_segments(text_lines: list) -> int:
    """Count glued-fragment bug occurrences in a list of SRT text lines."""
    n = 0
    for text in text_lines:
        for m in re.finditer(r"[.?!](?=\s+\S)", text):
            tail = text[m.end():].strip()
            wc = len(tail.split())
            if 2 <= wc <= 4:
                n += 1
                break
    return n


# ── main regeneration routine ──────────────────────────────────────────────

def find_video_stems(audio_dir: str, only: str = None) -> list:
    """Return sorted video stems that have both ``.words.srt`` and ``.srt``.

    A "stem" is the video filename without extension — e.g.
    ``10 - What is KIP--[koudaizy.com]``. We look for stems where a
    ``.words.srt`` exists (the source of word alignment).
    """
    words_paths = sorted(glob.glob(os.path.join(audio_dir, "*.words.srt")))
    stems = []
    for p in words_paths:
        # Strip ".words.srt"
        stem = os.path.basename(p)[:-len(".words.srt")]
        if only and only.lower() not in stem.lower():
            continue
        stems.append(stem)
    return stems


def regenerate_one(stem: str, audio_dir: str, max_words: int,
                    dry_run: bool = False, verbose: bool = False) -> dict:
    """Regenerate a single ``.srt`` from its ``.words.srt``.

    Returns a dict with keys: ``stem``, ``before``, ``after``, ``delta``,
    ``bugs_before``, ``bugs_after``, ``written`` (bool), ``error`` (str|None).
    """
    words_srt = os.path.join(audio_dir, stem + ".words.srt")
    srt_path = os.path.join(audio_dir, stem + ".srt")

    result = {
        "stem": stem,
        "srt_path": srt_path,
        "before": count_srt_entries(srt_path),
        "after": 0,
        "delta": 0,
        "bugs_before": len(detect_glued_fragments(srt_path)),
        "bugs_after": 0,
        "written": False,
        "error": None,
    }

    if not os.path.exists(words_srt):
        result["error"] = f"missing source: {os.path.basename(words_srt)}"
        return result

    try:
        words = parse_word_srt(words_srt)
    except Exception as e:
        result["error"] = f"words.srt parse failed: {e}"
        return result
    if not words:
        result["error"] = "words.srt is empty"
        return result

    # Duration: clamp to last word end (only used to bound the final cue).
    duration = words[-1].end + 0.1
    cues = build_aligned_cues(words, duration)
    segments = split_cues_for_subtitles(cues, max_words)

    if dry_run:
        result["after"] = len(segments)
        text_lines = [(seg.text or "").strip() for seg in segments if (seg.text or "").strip()]
        result["bugs_after"] = _count_glued_in_segments(text_lines)
    else:
        n = write_srt(cues, srt_path, max_words_per_segment=max_words)
        result["after"] = n
        result["bugs_after"] = len(detect_glued_fragments(srt_path))
        result["written"] = True

    result["delta"] = result["after"] - result["before"]
    if verbose:
        _print_segment_preview(stem, segments, words)
    return result


def _print_segment_preview(stem: str, segments: list, words: list) -> None:
    """Print the first 3 and last 2 segments + word count for debugging."""
    print(f"    --- preview for {stem} "
          f"({len(words)} words -> {len(segments)} segments) ---")
    sample = segments[:3] + (segments[-2:] if len(segments) > 3 else [])
    seen = set()
    for seg in sample:
        sid = id(seg)
        if sid in seen:
            continue
        seen.add(sid)
        text = (seg.text or "").strip()
        text_preview = text if len(text) <= 80 else text[:77] + "..."
        print(f"      [{seg.start:7.2f}-{seg.end:7.2f}] {text_preview}")


# ── CLI ─────────────────────────────────────────────────────────────────────

def parse_args(argv=None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Regenerate .srt subtitles from existing .words.srt.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Run with --dry-run first to preview the effect.",
    )
    p.add_argument(
        "--audio-dir", default=DEFAULT_AUDIO_DIR,
        help=f"Directory containing .words.srt + .srt files "
             f"(default: {DEFAULT_AUDIO_DIR}).",
    )
    p.add_argument(
        "--only", default=None,
        help="Only process videos whose stem contains this substring "
             "(case-insensitive). Useful for testing on one video.",
    )
    p.add_argument(
        "--max-words", type=int, default=DEFAULT_MAX_WORDS,
        help=f"Max words per SRT entry before splitting (default: {DEFAULT_MAX_WORDS}).",
    )
    p.add_argument(
        "--dry-run", action="store_true",
        help="Don't write any files — just report what would change.",
    )
    p.add_argument(
        "--scan-only", action="store_true",
        help="Only scan existing .srt files for the glued-fragment bug; "
             "don't regenerate anything. Exits 1 if any bugs found.",
    )
    p.add_argument(
        "--verbose", "-v", action="store_true",
        help="Show a per-video segment preview.",
    )
    return p.parse_args(argv)


def cmd_scan_only(audio_dir: str, only: str = None) -> int:
    """Scan existing .srt files for the glued-fragment bug."""
    srt_paths = sorted(glob.glob(os.path.join(audio_dir, "*.srt")))
    # Exclude .words.srt and .segments.srt — those aren't subject to the splitter.
    srt_paths = [p for p in srt_paths
                 if not p.endswith((".words.srt", ".segments.srt"))]
    if only:
        only_lower = only.lower()
        srt_paths = [p for p in srt_paths if only_lower in os.path.basename(p).lower()]

    if not srt_paths:
        print(f"no .srt files found under {audio_dir}", file=sys.stderr)
        return 1

    total_bugs = 0
    print(f"Scanning {len(srt_paths)} .srt file(s) for glued-fragment bugs...")
    for srt_path in srt_paths:
        hits = detect_glued_fragments(srt_path)
        stem = os.path.basename(srt_path)[:-4]
        if not hits:
            print(f"  [ok]   {stem}: no bugs")
            continue
        print(f"  [BUG]  {stem}: {len(hits)} glued fragment(s)")
        for h in hits[:5]:
            preview = h["glue_text"]
            if len(preview) > 60:
                preview = preview[:57] + "..."
            ts_str = _seconds_to_srt_ts(h["timestamp_start"])
            print(f"           entry #{h['entry']} ({ts_str}): +{preview!r}")
        if len(hits) > 5:
            print(f"           ... and {len(hits) - 5} more")
        total_bugs += len(hits)

    print(f"\nTotal bugs: {total_bugs}")
    return 1 if total_bugs else 0


def _seconds_to_srt_ts(seconds: float) -> str:
    """Format seconds as ``HH:MM:SS,mmm`` for diagnostic output."""
    if seconds is None or seconds < 0:
        seconds = 0.0
    total_ms = int(seconds * 1000)
    hours, rem = divmod(total_ms, 3_600_000)
    minutes, rem = divmod(rem, 60_000)
    secs, ms = divmod(rem, 1000)
    return f"{hours:02d}:{minutes:02d}:{secs:02d},{ms:03d}"


def main(argv=None) -> int:
    args = parse_args(argv)

    if args.scan_only:
        return cmd_scan_only(args.audio_dir, args.only)

    if not os.path.isdir(args.audio_dir):
        print(f"audio dir not found: {args.audio_dir}", file=sys.stderr)
        return 2

    stems = find_video_stems(args.audio_dir, args.only)
    if not stems:
        msg = f"no .words.srt files found under {args.audio_dir}"
        if args.only:
            msg += f" matching {args.only!r}"
        print(msg, file=sys.stderr)
        return 1

    action = "Would regenerate" if args.dry_run else "Regenerating"
    print(f"{action} .srt for {len(stems)} video(s) "
          f"(max_words={args.max_words})...")

    totals = {"before": 0, "after": 0, "bugs_before": 0, "bugs_after": 0}
    for stem in stems:
        r = regenerate_one(stem, args.audio_dir, args.max_words,
                            dry_run=args.dry_run, verbose=args.verbose)

        if r["error"]:
            print(f"  [skip] {stem}: {r['error']}")
            continue

        sign = "+" if r["delta"] >= 0 else ""
        bug_sign = "-" if r["bugs_before"] - r["bugs_after"] > 0 else ""
        bug_delta = r["bugs_before"] - r["bugs_after"]
        bug_str = (f", bugs {r['bugs_before']}->{r['bugs_after']} "
                   f"({bug_sign}{bug_delta})") if r["bugs_before"] else ""
        action_tag = "would write" if args.dry_run else "ok"
        print(f"  [{action_tag}] {stem}: {r['before']} -> {r['after']} entries "
              f"({sign}{r['delta']}){bug_str}")

        totals["before"] += r["before"]
        totals["after"] += r["after"]
        totals["bugs_before"] += r["bugs_before"]
        totals["bugs_after"] += r["bugs_after"]

    sign = "+" if totals["after"] - totals["before"] >= 0 else ""
    print(f"\nTotal: {totals['before']} -> {totals['after']} entries "
          f"({sign}{totals['after'] - totals['before']}), "
          f"bugs {totals['bugs_before']} -> {totals['bugs_after']}")
    if totals["bugs_after"] > 0:
        print("\nSome glued-fragment bugs remain. Re-run with --verbose to "
              "inspect, or check the cue_builder.py merging logic.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
