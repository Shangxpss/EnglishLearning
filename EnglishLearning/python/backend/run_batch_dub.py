#!/usr/bin/env python3 -u
"""Batch dubbing + subtitle generation with per-stage timing.

Processes every ``.mp4`` in a directory through the v4.1 pipeline (respeed
synthesis + PlaceStitcher) and generates SRT subtitles alongside the
dubbed audio.

Features:

  * **Batch mode** — processes every video in a directory, not just one.
  * **Subtitle generation** — writes sentence-level + word-level SRT files
    for each video, using the same word alignment the dubber already
    produces (no extra model pass needed).
  * **Per-stage timing** — records wall-clock time for every stage of
    every video, then prints a summary report at the end so you can see
    where the time goes across the whole batch.
  * **Aligner reuse** — loads the WordAligner (faster-whisper) ONCE and
    reuses it for all videos. Loading the Whisper model is ~5-10s; doing
    it per-video would waste N× that.
  * **Resume** — skips videos that already have a sibling ``.srt`` file
    (since the pipeline deletes the source video after processing, a
    video with an ``.srt`` next to it is a finished output), so
    interrupted batches pick up where they left off.

Pipeline (per video)::

    0. watermark removal    (OpenCV TELEA — optional, auto-detect)
    1. duration probe        (PyAV)
    2. word alignment        (WordAligner — reused across batch)
    3. cue building          (pure Python)
    4. subtitle generation   (SRT — sentence + word level)
    5. respeed synthesis     (RespeedSynthesizer — two-pass edge-tts)
    6. placement stitching   (PlaceStitcher — loudness norm + room tone)
    7. muxing                (PyAV remux + AAC encode)

Stage 0 is optional — set the ``DUB_REMOVE_WATERMARK=1`` environment
variable to enable it. When enabled, a ``_clean.mp4`` intermediate is
produced (watermark removed) and used as the source for all subsequent
stages. The intermediate is deleted after muxing.

Run::

    python3 run_batch_dub.py [video_dir]
    DUB_REMOVE_WATERMARK=1 python3 run_batch_dub.py [video_dir]

If ``video_dir`` is omitted, defaults to the app/subtitle directory.
"""

import gc
import os
import sys
import time

import numpy as np
import soundfile as sf

sys.stdout.reconfigure(line_buffering=True)
sys.stderr.reconfigure(line_buffering=True)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
# Disable HF xet protocol — it bypasses HF_ENDPOINT mirror and hits
# cas-server.xethub.hf.co directly (returns 401 in sandboxed environments).
# Forcing the regular HTTP download path uses the mirror correctly.
os.environ.setdefault("HF_HUB_DISABLE_XET", "1")

from app.services.sync import (  # noqa: E402
    WordAligner,
    build_aligned_cues,
    PlaceStitcher,
    RespeedSynthesizer,
    VoiceConfig,
    build_backend,
    get_media_duration,
    mux_video_audio,
    write_srt,
    write_word_srt,
    write_segments_srt,
)

# ── configuration ──────────────────────────────────────────────────────────
# Voice: en-GB-MaisieNeural (English - United Kingdom - Maisie)
VOICE = VoiceConfig(
    backend="edge_tts",
    gender="female",
    edge_voice="en-GB-MaisieNeural",
)
MAX_RETRIES = 2
MAX_CONCURRENCY = 8

# Subtitle segmentation: max words per SRT entry. Long sentences are split
# at punctuation marks (commas, semicolons, em-dashes) or conjunctions
# (and, but, so, because, ...) near the midpoint. If no natural break is
# found, splits at the hard word cap. 0 = no splitting (full sentences).
MAX_WORDS_PER_SEGMENT = 10

# Whisper model config. tiny = faster, base = more accurate.
WHISPER_MODEL = "base"
WHISPER_LANG = "en"
WHISPER_DEVICE = "cpu"
WHISPER_COMPUTE = "int8"

# ── paths ──────────────────────────────────────────────────────────────────
BACKEND = os.path.dirname(os.path.abspath(__file__))
SUBTITLE_ROOT = os.path.join(BACKEND, "app", "subtitle")
AUDIO_DIR = os.path.join(BACKEND, "audio")

DEFAULT_INPUT_DIR = AUDIO_DIR

# Manual watermark region: (x, y, width, height) in pixels.
# Found by frame-diffing the original source video vs the dubbed output
# for a previous batch. Watermark is a thin text strip at the very
# bottom-left corner.
WATERMARK_REGION = [(0, 1045, 270, 30)]

# Subdirectory names to skip when walking the input tree.
_SKIP_DIRS = {"sync_dub_batch", "sync_dub", "sync_dub_parallel", "__pycache__"}


def find_videos(video_dir: str, recursive: bool = True) -> list:
    """Return a sorted list of .mp4 files under ``video_dir``.

    When ``recursive`` is True (default), walks the whole subtree so all
    videos in nested chapter folders are picked up. Skips:
      * the batch output directory (avoids reprocessing prior outputs)
      * already-dubbed videos (filenames ending in ``_sync_dubbed.mp4``
        or legacy ``_dubbed.mp4`` — they already have a TTS track)

    Note: the pipeline now writes its output as ``{stem}.mp4`` (matching
    the subtitle name), with the original source video deleted after
    processing. So a video present in the directory is either a fresh
    source (to process) or a finished output (we can't tell from the
    name alone — the caller decides via the resume check in
    ``process_one_video``).
    """
    if not os.path.isdir(video_dir):
        raise SystemExit(f"input directory not found: {video_dir}")

    videos: list = []
    if recursive:
        for root, dirs, files in os.walk(video_dir):
            # prune skipped dirs in-place so os.walk doesn't descend into them
            dirs[:] = [d for d in dirs if d not in _SKIP_DIRS]
            for f in files:
                if not f.lower().endswith(".mp4"):
                    continue
                if f.lower().endswith(("_sync_dubbed.mp4", "_dubbed.mp4")):
                    # legacy dubbed output — skip to avoid redubbing
                    continue
                videos.append(os.path.join(root, f))
    else:
        for f in os.listdir(video_dir):
            if f.lower().endswith(".mp4") and not f.lower().endswith(
                ("_sync_dubbed.mp4", "_dubbed.mp4")
            ):
                videos.append(os.path.join(video_dir, f))
    videos.sort()
    if not videos:
        raise SystemExit(f"no .mp4 files found under {video_dir}")
    return videos


def process_one_video(video_path: str, aligner: WordAligner,
                      backend, sample_rate: int, out_dir: str,
                      remove_watermark: bool = False) -> dict:
    """Run the full v4 pipeline + subtitle generation on one video.

    Returns a dict of per-stage timings in seconds:
        ``{stage_name: seconds}``.
    """
    timings = {}

    def _stage(name):
        class _S:
            def __enter__(self):
                self.t0 = time.perf_counter()
                return self

            def __exit__(self, *exc):
                self.dt = time.perf_counter() - self.t0
                timings[name] = self.dt
        return _S()

    video_name = os.path.basename(video_path)
    video_stem = os.path.splitext(video_name)[0]
    tts_dir = os.path.join(out_dir, "tts")
    os.makedirs(tts_dir, exist_ok=True)

    print(f"\n{'='*70}")
    print(f"Processing: {video_name}")
    print(f"{'='*70}")

    # ── resume check ─────────────────────────────────────────────────────
    # Since the output video now uses the SAME filename as the source
    # (``{stem}.mp4`` — no ``_dubbed`` suffix), we can't tell a finished
    # output from a fresh source by name alone. The reliable signal that
    # a video was already processed is the presence of a sibling .srt
    # file: the pipeline writes the .srt only after alignment succeeds,
    # and the source video is deleted at the end of muxing. So if both
    # the .mp4 and .srt exist side-by-side, the video is a finished
    # output — skip it.
    srt_marker = os.path.join(out_dir, video_stem + ".srt")
    if os.path.exists(srt_marker):
        print(f"  [resume] .srt already exists alongside {video_name} — "
              f"treating as finished output, skipping")
        timings["_skipped"] = True
        timings["_out_video"] = video_path
        timings["_out_srt"] = srt_marker
        timings["_out_word_srt"] = os.path.join(out_dir, video_stem + ".words.srt")
        timings["_out_segments_srt"] = os.path.join(out_dir, video_stem + ".segments.srt")
        return timings

    # ── 0. watermark removal (optional) ──────────────────────────────────
    # Produces a _clean.mp4 intermediate that all subsequent stages use
    # as their video source. If no watermark is detected, the stage is a
    # fast remux (stream-copy, no re-encode).
    clean_path = None
    if remove_watermark:
        with _stage("0_watermark"):
            from app.services.watermark import remove_watermark as _rm_wm
            clean_path = os.path.join(out_dir, video_stem + "_clean.mp4")
            result_wm = _rm_wm(video_path, clean_path, regions=WATERMARK_REGION)
        if result_wm.get("skipped"):
            print(f"  [0] watermark: skipped (output exists)")
        elif result_wm.get("frames_processed", 0) == 0:
            print(f"  [0] watermark: none detected, remuxed "
                  f"({timings['0_watermark']:.1f}s)")
        else:
            n_frames = result_wm["frames_processed"]
            print(f"  [0] watermark: removed {n_frames} frames, "
                  f"regions={result_wm.get('regions', [])} "
                  f"({timings['0_watermark']:.1f}s)")
    # Source video for all downstream stages: cleaned if available, else
    # the original.
    src_video = clean_path if clean_path and os.path.exists(clean_path) else video_path

    # ── 1. duration probe ──────────────────────────────────────────────
    with _stage("1_duration"):
        duration = get_media_duration(src_video)
    print(f"  [1] duration probe: {duration:.1f}s ({timings['1_duration']:.2f}s)")

    # ── 2. word alignment (aligner reused) ─────────────────────────────
    with _stage("2_alignment"):
        words = aligner.align(src_video)
    n_low = sum(1 for w in words if w.score < 0.5)
    print(f"  [2] alignment: {len(words)} words "
          f"({timings['2_alignment']:.1f}s, low-conf={n_low})")

    # ── 3. cue building ──────────────────────────────────────────────────
    with _stage("3_cues"):
        cues = build_aligned_cues(words, duration)
    print(f"  [3] cues: {len(cues)} sentences ({timings['3_cues']:.3f}s)")

    # ── 4. subtitle generation (NEW) ────────────────────────────────────
    # SRT generation is essentially free — the cues already carry the text
    # and timestamps. We write sentence-level, word-level, and Whisper's
    # raw segment-level SRT.
    with _stage("4_subtitles"):
        srt_path = os.path.join(out_dir, video_stem + ".srt")
        word_srt_path = os.path.join(out_dir, video_stem + ".words.srt")
        segments_srt_path = os.path.join(out_dir, video_stem + ".segments.srt")
        n_sent = write_srt(cues, srt_path,
                           max_words_per_segment=MAX_WORDS_PER_SEGMENT)
        n_word = write_word_srt(words, word_srt_path, group_size=1)
        n_seg = write_segments_srt(aligner.last_segments, segments_srt_path)
    print(f"  [4] subtitles: {n_sent} sentences + {n_word} words + "
          f"{n_seg} segments ({timings['4_subtitles']:.3f}s)")

    # ── 5. respeed synthesis (two-pass: normal → respeed overflowing) ────
    # Instead of compressing TTS audio in post-processing (which causes
    # metallic artifacts), we detect overflow and re-synthesize at a faster
    # edge-tts rate. edge-tts handles speed natively — no artifacts.
    with _stage("5_synthesis"):
        synthesizer = RespeedSynthesizer(
            backend=backend,
            max_retries=MAX_RETRIES,
            max_concurrency=MAX_CONCURRENCY,
        )
        synth_paths = synthesizer.synthesize(cues, tts_dir)
    n_ok = len(synth_paths)
    total_tts_audio = 0.0
    for p in synth_paths.values():
        try:
            ad, _ = sf.read(p)
            total_tts_audio += len(ad) / sample_rate
        except Exception:
            pass
    print(f"  [5] synthesis: {n_ok}/{len(cues)} sentences "
          f"({timings['5_synthesis']:.1f}s, audio={total_tts_audio:.1f}s, "
          f"speed={total_tts_audio/max(timings['5_synthesis'], 0.01):.1f}x rt)")

    # ── 6. placement stitching (no compression, no truncation) ──────────
    # TTS audio has already been speed-adjusted to fit slots — just place it.
    with _stage("6_stitching"):
        sentence_audio = {}
        for ci in range(len(cues)):
            wav_path = os.path.join(tts_dir, f"sent_{ci:04d}.wav")
            if not os.path.exists(wav_path):
                continue
            ad, sr = sf.read(wav_path)
            if sr != sample_rate:
                import librosa  # lazy
                ad = librosa.resample(np.asarray(ad, dtype=np.float32),
                                      orig_sr=sr, target_sr=sample_rate)
            if ad.ndim > 1:
                ad = ad[:, 0]
            sentence_audio[ci] = ad
        stitcher = PlaceStitcher(sample_rate=sample_rate)
        final_audio = stitcher.stitch(sentence_audio, cues, duration)
    final_wav = os.path.join(out_dir, "dubbed_audio.wav")
    sf.write(final_wav, final_audio, sample_rate)
    print(f"  [6] stitching: {len(final_audio)/sample_rate:.1f}s "
          f"({timings['6_stitching']:.2f}s)")

    # ── 7. muxing ────────────────────────────────────────────────────────
    # Output video uses the SAME stem as the source video (no ``_dubbed``
    # suffix) so the mp4 filename matches its sibling .srt subtitles.
    # This way any video player auto-loads the subtitle when opening the
    # video.
    #
    # IMPORTANT: ``out_video`` may have the SAME path as ``video_path`` (the
    # source) when the watermark intermediate is in use — in that case we
    # must NOT delete ``out_video`` before muxing, because the muxer reads
    # from ``src_video`` (= ``_clean.mp4``), not from ``out_video``, so the
    # pre-delete is unnecessary and would just churn the inode. We only
    # pre-delete when the source we're about to read (``src_video``) is
    # genuinely a different file from the output we're about to write.
    with _stage("7_muxing"):
        out_video = os.path.join(out_dir, video_stem + ".mp4")
        if (os.path.abspath(src_video) != os.path.abspath(out_video)
                and os.path.exists(out_video)):
            os.remove(out_video)
        mux_video_audio(
            video_path=src_video,
            audio_path=final_wav,
            output_path=out_video,
            audio_bitrate=128_000,
            shortest=True,
        )
    out_size = os.path.getsize(out_video) / (1024 * 1024)
    print(f"  [7] muxing: {out_size:.1f} MB ({timings['7_muxing']:.1f}s)")

    # ── cleanup: delete intermediate files to save disk ──────────────────
    # The dubbed mp4 has the video stream (remuxed from source) + new audio.
    # NOTE: when ``out_video`` has the same path as the original source
    # (which is the default since the rename to ``{stem}.mp4``), we MUST NOT
    # delete ``video_path`` — that path now points to the freshly-muxed
    # dubbed output. Only delete the source video when it's a separate file
    # from the output (legacy ``_dubbed.mp4`` naming). The ``_clean.mp4``
    # watermark intermediate is always safe to delete.
    try:
        import shutil
        # Delete intermediate TTS wavs + dubbed_audio.wav.
        if os.path.isdir(tts_dir):
            shutil.rmtree(tts_dir)
        if os.path.exists(final_wav):
            os.remove(final_wav)
        # Delete the _clean.mp4 watermark-removal intermediate (if any).
        if clean_path and os.path.exists(clean_path):
            os.remove(clean_path)
        # Delete the original source video ONLY when it's a separate file
        # from the dubbed output. When they share the same path (current
        # naming), the source has been replaced in-place by the muxer.
        if os.path.abspath(video_path) != os.path.abspath(out_video):
            if os.path.exists(video_path):
                os.remove(video_path)
                print(f"  [cleanup] deleted original source + intermediates")
            else:
                print(f"  [cleanup] deleted intermediates")
        else:
            print(f"  [cleanup] deleted intermediates (output replaced source in-place)")
    except Exception as e:
        print(f"  [cleanup] warning: {e}")

    total = sum(timings.values())
    rt_factor = total / duration if duration > 0 else 0
    print(f"  TOTAL: {total:.1f}s ({rt_factor:.2f}x realtime)")
    timings["_total"] = total
    timings["_duration"] = duration
    timings["_out_video"] = out_video
    timings["_out_srt"] = srt_path
    timings["_out_word_srt"] = word_srt_path
    timings["_out_segments_srt"] = segments_srt_path
    timings["_n_words"] = len(words)
    timings["_n_cues"] = len(cues)
    timings["_n_synth_ok"] = n_ok
    return timings


def print_batch_report(all_timings: list, video_names: list):
    """Print a summary table of per-stage timings across the whole batch."""
    print("\n" + "=" * 90)
    print("BATCH TIMING REPORT")
    print("=" * 90)

    # Per-video summary
    stage_keys = ["0_watermark", "1_duration", "2_alignment", "3_cues",
                  "4_subtitles", "5_synthesis", "6_stitching", "7_muxing"]
    header = f"{'video':<48} " + " ".join(f"{k.split('_')[1]:>8}" for k in stage_keys) + f" {'total':>8}"
    print(header)
    print("-" * len(header))
    for name, t in zip(video_names, all_timings):
        row = f"{name[:48]:<48} "
        for k in stage_keys:
            row += f"{t.get(k, 0):>8.1f} "
        row += f"{t['_total']:>8.1f}"
        print(row)
    print("-" * len(header))

    # Totals per stage
    print(f"\n{'Stage totals':<48} " +
          " ".join(f"{k.split('_')[1]:>8}" for k in stage_keys) + f" {'TOTAL':>8}")
    totals_row = f"{'(' + str(len(video_names)) + ' videos)':<48} "
    stage_totals = []
    for k in stage_keys:
        s = sum(t.get(k, 0) for t in all_timings)
        stage_totals.append(s)
        totals_row += f"{s:>8.1f} "
    grand_total = sum(stage_totals)
    totals_row += f"{grand_total:>8.1f}"
    print(totals_row)

    # Percentages
    print(f"\n{'Stage % of total':<48} " +
          " ".join(f"{k.split('_')[1]:>8}" for k in stage_keys))
    pct_row = f"{'':<48} "
    for s in stage_totals:
        pct = 100 * s / grand_total if grand_total > 0 else 0
        pct_row += f"{pct:>7.1f}% "
    print(pct_row)

    # Aggregate stats
    total_dur = sum(t["_duration"] for t in all_timings)
    total_words = sum(t["_n_words"] for t in all_timings)
    total_cues = sum(t["_n_cues"] for t in all_timings)
    total_synth_ok = sum(t["_n_synth_ok"] for t in all_timings)
    print(f"\n## Aggregate")
    print(f"  videos processed:    {len(all_timings)}")
    print(f"  total video length:  {total_dur:.1f}s ({total_dur/60:.1f} min)")
    print(
        f"  total processing:    {grand_total:.1f}s ({grand_total/60:.1f} min)")
    print(f"  batch realtime:      {grand_total/total_dur:.2f}x "
          f"(lower = faster than realtime)")
    print(f"  total words aligned: {total_words}")
    print(f"  total sentences:     {total_cues}")
    print(f"  total TTS ok:        {total_synth_ok}/{total_cues}")

    # Output file locations
    print(f"\n## Output files")
    for name, t in zip(video_names, all_timings):
        print(f"  {name}:")
        print(f"    video:    {t['_out_video']}")
        print(f"    srt:      {t['_out_srt']}")
        print(f"    words:    {t['_out_word_srt']}")
        print(f"    segments: {t.get('_out_segments_srt', '')}")

    # Bottleneck analysis
    print(f"\n## Bottleneck analysis")
    slowest_stage = max(zip(stage_keys, stage_totals), key=lambda x: x[1])
    print(f"  slowest stage: {slowest_stage[0]} "
          f"({slowest_stage[1]:.1f}s, "
          f"{100*slowest_stage[1]/grand_total:.1f}% of total)")
    print(f"  - stage 2 (alignment) is CPU-bound (faster-whisper int8)")
    print(f"  - stage 5 (synthesis) is IO-bound (network → async, not Rust)")
    print(f"  - stages 1,3,4,6,7 are negligible")


def main():
    input_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_INPUT_DIR
    do_remove_watermark = os.environ.get("DUB_REMOVE_WATERMARK", "0") == "1"
    print(f"Input directory: {input_dir}")
    if do_remove_watermark:
        print(f"Watermark removal: ENABLED (stage 0)")

    videos = find_videos(input_dir, recursive=True)
    print(f"Found {len(videos)} video(s):")
    for v in videos:
        rel = os.path.relpath(v, input_dir)
        print(f"  - {rel}")

    print(f"Output: in-place (dubbed video replaces original in same dir)")

    # ── one-time setup: load aligner + backend ───────────────────────────
    print(f"\n[setup] Loading WordAligner (model={WHISPER_MODEL}, "
          f"compute={WHISPER_COMPUTE})...")
    t0 = time.perf_counter()
    aligner = WordAligner(model_size=WHISPER_MODEL, language=WHISPER_LANG,
                          device=WHISPER_DEVICE, compute_type=WHISPER_COMPUTE)
    print(f"  aligner loaded in {time.perf_counter()-t0:.1f}s "
          f"(backend={aligner.backend})")

    print(f"[setup] Building TTS backend ({VOICE.backend})...")
    t0 = time.perf_counter()
    backend = build_backend(VOICE)
    sample_rate = backend.sample_rate
    print(f"  {VOICE.describe()}")
    print(f"  backend ready in {time.perf_counter()-t0:.2f}s "
          f"(sample_rate={sample_rate})")

    # ── process each video ────────────────────────────────────────────────
    all_timings = []
    video_names = []
    for i, video_path in enumerate(videos, 1):
        video_name = os.path.basename(video_path)
        # Output in-place: dubbed video + SRT go in the SAME directory as
        # the original video. Intermediates (tts/, dubbed_audio.wav) are
        # cleaned up after muxing.
        video_out_dir = os.path.dirname(video_path)
        video_names.append(os.path.relpath(video_path, input_dir))

        print(f"\n{'#'*70}")
        print(f"# VIDEO {i}/{len(videos)}: {video_name}")
        print(f"{'#'*70}")

        try:
            t = process_one_video(video_path, aligner, backend, sample_rate,
                                  video_out_dir,
                                  remove_watermark=do_remove_watermark)
            all_timings.append(t)
        except Exception as e:
            import traceback
            print(f"  FAILED: {e}")
            traceback.print_exc()
            all_timings.append({"_total": 0, "_duration": 0,
                                "_n_words": 0, "_n_cues": 0, "_n_synth_ok": 0,
                                "_out_video": "", "_out_srt": "",
                                "_out_word_srt": "", "_out_segments_srt": ""})

    # ── cleanup ──────────────────────────────────────────────────────────
    del aligner
    gc.collect()

    # ── report ────────────────────────────────────────────────────────────
    print_batch_report(all_timings, video_names)

    print(f"\n{'='*70}")
    print(f"BATCH COMPLETE: {len(all_timings)} videos processed")
    print(f"Output root: {input_dir}")
    print(f"{'='*70}")


if __name__ == "__main__":
    main()
