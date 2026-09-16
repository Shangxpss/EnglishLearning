"""Dubbing Studio API — async batch dubbing with progress tracking.

Exposes endpoints to start a dubbing job (file or folder), poll its progress,
and list past jobs. The heavy lifting reuses the sync dubbing pipeline
(``app.services.sync``) — this module just wraps it with:

  * **Job tracking** — each job gets a UUID; state lives in an in-memory dict.
  * **Background execution** — dubbing runs in a daemon thread so the HTTP
    request returns immediately with a ``job_id``.
  * **Progress polling** — ``GET /dubbing/progress/{job_id}`` returns the
    current video, stage, completed/total count, and per-video results.

Output layout (per the user's spec):
  * **Folder input** → a sibling ``<folder_name>_dubbed/`` directory is
    created alongside the original, containing dubbed ``.mp4`` + ``.srt`` files.
    Originals are kept untouched.
  * **File input** → a ``<name>_dubbed.mp4`` is written alongside the
    original file (originals kept untouched).
"""

from __future__ import annotations

import logging
import os
import threading
import time
import uuid
from dataclasses import asdict, dataclass, field
from typing import Dict, List, Optional

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/dubbing", tags=["dubbing"])


# ── job state ────────────────────────────────────────────────────────────


@dataclass
class VideoResult:
    name: str
    status: str  # "ok" | "failed"
    out_video: Optional[str] = None
    out_srt: Optional[str] = None
    duration: float = 0.0
    processing_time: float = 0.0
    error: Optional[str] = None


@dataclass
class JobState:
    job_id: str
    status: str  # "pending" | "running" | "completed" | "failed"
    input_path: str
    input_type: str  # "file" | "folder"
    output_path: str
    voice: str
    max_words_per_segment: int = 0
    room_tone: bool = True
    remove_watermark: bool = False
    total: int = 0
    completed: int = 0
    failed: int = 0
    current_video: Optional[str] = None
    current_stage: Optional[str] = None
    stage_progress: int = 0
    stage_total: int = 0
    started_at: float = 0.0
    finished_at: Optional[float] = None
    results: List[VideoResult] = field(default_factory=list)
    error: Optional[str] = None

    def to_dict(self) -> dict:
        d = asdict(self)
        now = time.time()
        d["elapsed"] = (self.finished_at or now) - self.started_at if self.started_at else 0.0
        d["progress_pct"] = round(self.completed / self.total * 100, 1) if self.total else 0.0
        d["stage_progress_pct"] = round(self.stage_progress / self.stage_total * 100, 1) if self.stage_total else 0.0
        return d


# In-memory job store (sufficient for a single-instance dev server).
_jobs: Dict[str, JobState] = {}
_jobs_lock = threading.Lock()

# Lazy-loaded pipeline singletons (aligner + backend are expensive to create).
_pipeline_cache: dict = {}
_pipeline_lock = threading.Lock()


# ── request / response models ─────────────────────────────────────────────


class DubRequest(BaseModel):
    path: str
    voice: str = "en-GB-MaisieNeural"
    whisper_model: str = "base"
    max_words_per_segment: int = 0  # 0 = no splitting (full sentences)
    room_tone: bool = True  # fill inter-sentence gaps with extracted ambient noise
    remove_watermark: bool = False  # run stage 0: auto-detect + inpaint watermark


class StartResponse(BaseModel):
    job_id: str
    status: str
    total: int
    input_type: str
    output_path: str


# ── pipeline loader ──────────────────────────────────────────────────────


def _get_pipeline(voice: str, whisper_model: str = "base"):
    """Load the sync dubbing pipeline (aligner + TTS backend) once, then cache.

    Loading faster-whisper takes ~5-10s, and building the edge-tts backend
    is cheap but stateful. We cache by voice so switching voices rebuilds
    only the backend, not the aligner.
    """
    with _pipeline_lock:
        aligner = _pipeline_cache.get("aligner")
        if aligner is None:
            # Import here so the API boots even if ML deps are missing.
            os.environ.setdefault("HF_ENDPOINT", "https://hf-mirror.com")
            os.environ.setdefault("CUDA_VISIBLE_DEVICES", "")
            os.environ.setdefault("HF_HUB_DISABLE_XET", "1")
            from app.services.sync import WordAligner

            aligner = WordAligner(
                model_size=whisper_model,
                language="en",
                device="cpu",
                compute_type="int8",
            )
            _pipeline_cache["aligner"] = aligner
            logger.info("WordAligner loaded (model=%s)", whisper_model)

        backend = _pipeline_cache.get(("backend", voice))
        if backend is None:
            from app.services.sync import VoiceConfig, build_backend

            cfg = VoiceConfig(
                backend="edge_tts",
                gender="female",
                edge_voice=voice,
            )
            backend = build_backend(cfg)
            _pipeline_cache[("backend", voice)] = backend
            logger.info("TTS backend built (voice=%s)", voice)

        return aligner, backend, backend.sample_rate


# ── path helpers ─────────────────────────────────────────────────────────


_SKIP_DIRS = {"sync_dub_batch", "sync_dub", "sync_dub_parallel", "__pycache__", "tts"}


def _find_videos(folder: str) -> List[str]:
    """Return sorted .mp4 files under ``folder`` (recursive)."""
    out: List[str] = []
    for root, dirs, files in os.walk(folder):
        dirs[:] = [d for d in dirs if d not in _SKIP_DIRS]
        for f in files:
            if f.lower().endswith(".mp4") and not f.lower().endswith(
                ("_dubbed.mp4", "_sync_dubbed.mp4")
            ):
                out.append(os.path.join(root, f))
    out.sort()
    return out


def _resolve_output(input_path: str) -> tuple[str, str]:
    """Determine output dir and input type from a path.

    Returns ``(output_path, input_type)``:
      * File  → output_path = dirname(file); input_type = "file"
      * Folder → output_path = sibling ``<name>_dubbed`` dir; input_type = "folder"

    For folder input, the ``_dubbed`` directory is created alongside the
    original folder, mirroring the internal subfolder structure.
    """
    if os.path.isfile(input_path):
        return os.path.dirname(os.path.abspath(input_path)), "file"

    input_path = os.path.abspath(input_path.rstrip("/"))
    parent = os.path.dirname(input_path)
    name = os.path.basename(input_path)
    out_dir = os.path.join(parent, f"{name}_dubbed")
    os.makedirs(out_dir, exist_ok=True)
    return out_dir, "folder"


# ── background job runner ────────────────────────────────────────────────


def _run_dub_job(job_id: str):
    """Background thread: process all videos for a job, updating state."""
    with _jobs_lock:
        job = _jobs.get(job_id)
    if job is None:
        return

    try:
        aligner, backend, sample_rate = _get_pipeline(job.voice)
    except Exception as e:
        logger.exception("pipeline load failed")
        with _jobs_lock:
            job.status = "failed"
            job.error = f"Pipeline load failed: {e}"
            job.finished_at = time.time()
        return

    # Resolve the video list.
    if job.input_type == "file":
        videos = [job.input_path]
    else:
        videos = _find_videos(job.input_path)

    with _jobs_lock:
        job.total = len(videos)
        if not videos:
            job.status = "failed"
            job.error = "No .mp4 files found in the given folder."
            job.finished_at = time.time()
            return

    # Import the per-video pipeline pieces (done here so the module import
    # at API startup doesn't require all ML deps).
    from app.services.sync import (
        PlaceStitcher,
        RespeedSynthesizer,
        build_aligned_cues,
        get_media_duration,
        mux_video_audio,
        write_srt,
        write_word_srt,
    )
    import numpy as np
    import soundfile as sf

    MAX_RETRIES = 2
    MAX_CONCURRENCY = 8

    for vi, video_path in enumerate(videos):
        video_name = os.path.basename(video_path)
        video_stem = os.path.splitext(video_name)[0]

        # Output dir: for folder input, mirror subfolder structure in _dubbed.
        if job.input_type == "folder":
            rel = os.path.relpath(os.path.dirname(video_path), job.input_path)
            out_dir = os.path.join(job.output_path, rel)
            os.makedirs(out_dir, exist_ok=True)
        else:
            out_dir = job.output_path

        # ── Resume check ──────────────────────────────────────────────
        # If the dubbed output already exists from a previous run, skip it.
        # This makes the job resumable: if it was interrupted (Ctrl+C,
        # network error, process kill), restarting with the same path picks
        # up exactly where it left off. The filesystem IS the checkpoint.
        out_video_check = os.path.join(out_dir, video_stem + "_dubbed.mp4")
        if os.path.exists(out_video_check) and os.path.getsize(out_video_check) > 0:
            with _jobs_lock:
                job.completed += 1
                job.results.append(VideoResult(
                    name=video_name,
                    status="ok",
                    out_video=out_video_check,
                    duration=get_media_duration(video_path) if os.path.exists(video_path) else 0.0,
                    processing_time=0.0,
                ))
                job.current_video = video_name
                job.current_stage = "skipped (already dubbed)"
                job.stage_progress = 0
                job.stage_total = 0
            logger.info("Job %s: skipping %s (already dubbed)", job_id, video_name)
            continue

        with _jobs_lock:
            job.current_video = video_name
            job.current_stage = "starting"
            job.stage_progress = 0
            job.stage_total = 0

        result = VideoResult(name=video_name, status="ok")
        t0 = time.perf_counter()
        try:
            tts_dir = os.path.join(out_dir, f"tts_{job_id[:8]}_{vi}")
            os.makedirs(tts_dir, exist_ok=True)

            # 0. watermark removal (optional)
            clean_path = None
            src_video = video_path
            if job.remove_watermark:
                with _jobs_lock:
                    job.current_stage = "0_watermark"
                    job.stage_progress = 0
                    job.stage_total = 0

                def _wm_progress(completed, total):
                    with _jobs_lock:
                        job.stage_progress = completed
                        job.stage_total = total

                from app.services.watermark import remove_watermark as _rm_wm
                clean_path = os.path.join(out_dir, video_stem + "_clean.mp4")
                _rm_wm(video_path, clean_path, regions="auto",
                       progress_callback=_wm_progress)
                src_video = clean_path

            # 1. duration
            with _jobs_lock:
                job.current_stage = "1_duration"
                job.stage_progress = 0
                job.stage_total = 0
            duration = get_media_duration(src_video)

            # 2. alignment
            with _jobs_lock:
                job.current_stage = "2_alignment"
                job.stage_progress = 0
                job.stage_total = 0

            def _align_progress(completed, total):
                with _jobs_lock:
                    job.stage_progress = completed
                    job.stage_total = total

            words = aligner.align(src_video, progress_callback=_align_progress,
                                  checkpoint_dir=out_dir)

            # 3. cues
            with _jobs_lock:
                job.current_stage = "3_cues"
                job.stage_progress = 0
                job.stage_total = 0
            cues = build_aligned_cues(words, duration)

            # 4. subtitles
            with _jobs_lock:
                job.current_stage = "4_subtitles"
                job.stage_progress = 0
                job.stage_total = 0
            srt_path = os.path.join(out_dir, video_stem + ".srt")
            word_srt_path = os.path.join(out_dir, video_stem + ".words.srt")
            write_srt(cues, srt_path, max_words_per_segment=job.max_words_per_segment)
            write_word_srt(words, word_srt_path, group_size=1)

            # 5. respeed synthesis
            with _jobs_lock:
                job.current_stage = "5_synthesis"
                job.stage_progress = 0
                job.stage_total = len(cues)

            def _synth_progress(completed, total):
                with _jobs_lock:
                    job.stage_progress = completed
                    job.stage_total = total

            synthesizer = RespeedSynthesizer(
                backend=backend,
                max_retries=MAX_RETRIES,
                max_concurrency=MAX_CONCURRENCY,
            )
            synth_paths = synthesizer.synthesize(cues, tts_dir,
                                                   progress_callback=_synth_progress)

            # 6. placement stitching
            with _jobs_lock:
                job.current_stage = "6_stitching"
                job.stage_progress = 0
                job.stage_total = 0
            sentence_audio = {}
            for ci in range(len(cues)):
                wav_path = os.path.join(tts_dir, f"sent_{ci:04d}.wav")
                if not os.path.exists(wav_path):
                    continue
                ad, sr = sf.read(wav_path)
                if sr != sample_rate:
                    import librosa

                    ad = librosa.resample(
                        np.asarray(ad, dtype=np.float32),
                        orig_sr=sr,
                        target_sr=sample_rate,
                    )
                if ad.ndim > 1:
                    ad = ad[:, 0]
                sentence_audio[ci] = ad
            stitcher = PlaceStitcher(sample_rate=sample_rate)
            # Extract room tone from the original video's silent gaps.
            # This fills inter-sentence silence with low-level ambient noise
            # so the dub doesn't sound choppy.
            room_tone_audio = None
            if job.room_tone:
                try:
                    from app.services.sync import extract_room_tone
                    room_tone_audio = extract_room_tone(video_path, target_sr=sample_rate)
                except Exception as e:
                    logger.warning("room tone extraction failed: %s", e)
            final_audio = stitcher.stitch(
                sentence_audio, cues, duration,
                room_tone=room_tone_audio,
            )
            final_wav = os.path.join(tts_dir, "dubbed_audio.wav")
            sf.write(final_wav, final_audio, sample_rate)

            # 7. muxing
            with _jobs_lock:
                job.current_stage = "7_muxing"
                job.stage_progress = 0
                job.stage_total = 0
            out_video = os.path.join(out_dir, video_stem + "_dubbed.mp4")
            if os.path.exists(out_video):
                os.remove(out_video)
            mux_video_audio(
                video_path=src_video,
                audio_path=final_wav,
                output_path=out_video,
                audio_bitrate=128_000,
                shortest=True,
            )

            # cleanup intermediates (keep the original — user wants it untouched)
            import shutil

            if os.path.isdir(tts_dir):
                shutil.rmtree(tts_dir, ignore_errors=True)
            # Delete the _clean.mp4 watermark-removal intermediate (if any).
            if clean_path and os.path.exists(clean_path):
                os.remove(clean_path)

            result.out_video = out_video
            result.out_srt = srt_path
            result.duration = duration
            result.processing_time = time.perf_counter() - t0

        except Exception as e:
            logger.exception("dubbing failed for %s", video_name)
            result.status = "failed"
            result.error = str(e)
            result.processing_time = time.perf_counter() - t0

        with _jobs_lock:
            job.results.append(result)
            job.completed += 1
            if result.status == "failed":
                job.failed += 1
            job.current_stage = None
            job.stage_progress = 0
            job.stage_total = 0

    with _jobs_lock:
        job.status = "completed" if job.failed < job.total else "failed"
        job.current_video = None
        job.finished_at = time.time()
    logger.info("Job %s done: %d ok, %d failed", job_id, job.completed - job.failed, job.failed)


# ── endpoints ────────────────────────────────────────────────────────────


@router.post("/start", response_model=StartResponse)
def start_dubbing(req: DubRequest):
    """Start a dubbing job for a file or folder path.

    Returns immediately with a ``job_id``. Poll ``GET /dubbing/progress/{job_id}``
    for status.
    """
    path = req.path.strip()
    if not path:
        raise HTTPException(status_code=400, detail="path is required")
    if not os.path.exists(path):
        raise HTTPException(status_code=404, detail=f"path not found: {path}")

    output_path, input_type = _resolve_output(path)
    job_id = uuid.uuid4().hex[:12]

    job = JobState(
        job_id=job_id,
        status="pending",
        input_path=os.path.abspath(path),
        input_type=input_type,
        output_path=output_path,
        voice=req.voice,
        max_words_per_segment=req.max_words_per_segment,
        room_tone=req.room_tone,
        remove_watermark=req.remove_watermark,
        started_at=time.time(),
    )
    with _jobs_lock:
        _jobs[job_id] = job

    # Launch background thread.
    t = threading.Thread(target=_run_dub_job, args=(job_id,), daemon=True)
    t.start()

    total = 1 if input_type == "file" else len(_find_videos(job.input_path))
    with _jobs_lock:
        job.total = total
        job.status = "running"

    return StartResponse(
        job_id=job_id,
        status="running",
        total=total,
        input_type=input_type,
        output_path=output_path,
    )


@router.get("/progress/{job_id}")
def get_progress(job_id: str):
    """Poll the progress of a dubbing job."""
    with _jobs_lock:
        job = _jobs.get(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail=f"job not found: {job_id}")
    return job.to_dict()


@router.get("/jobs")
def list_jobs():
    """List all dubbing jobs (most recent first)."""
    with _jobs_lock:
        jobs = sorted(
            _jobs.values(),
            key=lambda j: j.started_at,
            reverse=True,
        )
    return {"jobs": [j.to_dict() for j in jobs]}


@router.get("/voices")
def list_voices():
    """Return preset edge-tts voice options for the UI."""
    return {
        "voices": [
            {"label": "Maisie (en-GB, Female)", "value": "en-GB-MaisieNeural"},
            {"label": "Sonia (en-GB, Female)", "value": "en-GB-SoniaNeural"},
            {"label": "Libby (en-GB, Female)", "value": "en-GB-LibbyNeural"},
            {"label": "Ryan (en-GB, Male)", "value": "en-GB-RyanNeural"},
            {"label": "Aria (en-US, Female)", "value": "en-US-AriaNeural"},
            {"label": "Guy (en-US, Male)", "value": "en-US-GuyNeural"},
        ]
    }
