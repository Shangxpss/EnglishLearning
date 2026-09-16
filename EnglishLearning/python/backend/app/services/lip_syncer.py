"""Lip synchronization (optional stage 5).

For talking-head videos, regenerating the audio leaves the original speaker's
lip movements out of sync with the new (cloned / converted) voice. This
module re-syncs the lips to the new audio by editing only the lower-face
region of each frame, preserving lighting, head pose, and background.

Architecture (pluggable backends):

    LipSyncer (facade)
        └── LipSyncBackend (abstract)
            ├── VideoReTalkingBackend  (primary — 3-stage, Apache-2.0, best OSS)
            ├── Wav2LipBackend           (baseline — research license, soft mouth)
            └── LatentSyncBackend        (future — diffusion-based, sharper)

VideoReTalking pipeline (3 stages):
  1. D-Net: normalize facial expression to a neutral template.
  2. L-Net: Wav2Lip-based generator drives lip movements from the new audio.
  3. E-Net: GFPGAN/GPEN face enhancement restores photo-realism.

This stage is OPT-IN (slow: ~3 min per 1 min video on RTX 4090) and only
runs when a face is detected in the video. Non-talking-head footage is
passed through unchanged.

Reference: see ``docs/NATURAL_ACCENT_DUBBING_DESIGN.md`` §3.3 / §5.3.
"""

from __future__ import annotations

import os
import subprocess
import logging
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)

FFMPEG_PATH = "/usr/bin/ffmpeg"
FFPROBE_PATH = "/usr/bin/ffprobe"


@dataclass
class SyncResult:
    """Outcome of a lip-sync run."""
    success: bool
    video_path: str = ""           # path to the lipsynced output video
    backend: str = ""
    faces_detected: bool = False
    error: str = ""
    stats: Dict[str, Any] = field(default_factory=dict)


@dataclass
class LipSyncConfig:
    """Configuration for a lip-sync run."""
    checkpoint_dir: str = ""        # dir containing pretrained models
    device: str = "cuda:0"
    face_enhancer: str = "gfpgan"   # "gfpgan" | "gpen" | "none"
    output_fps: int = 25
    resize_factor: int = 1         # downscale for speed (1 = full res)


# --- Abstract backend ------------------------------------------------------

class LipSyncBackend(ABC):
    """Interface every lip-sync backend implements."""

    name: str = "abstract"

    @abstractmethod
    def is_available(self) -> bool:
        ...

    @abstractmethod
    def sync(self,
             video_path: str,
             audio_path: str,
             output_path: str,
             config: LipSyncConfig) -> SyncResult:
        ...


# --- VideoReTalking backend (primary) -------------------------------------

class VideoReTalkingBackend(LipSyncBackend):
    """VideoReTalking — 3-stage audio-driven lip sync.

    Edits the lower-face region of existing talking-head footage to match a
    new audio track while preserving the original lighting / head pose /
    background. Best open-source quality (7.2k+ GitHub stars).

    Repo: github.com/OpenTalker/video-retalking
    License: Apache-2.0
    Hardware: 8 GB VRAM minimum, 24 GB recommended.
    """

    name = "videoretalking"

    def is_available(self) -> bool:
        # We detect the repo via the VIDEORETALKING_DIR env var (the project
        # has no pip-installable package, so it must be cloned + configured).
        repo = os.environ.get("VIDEORETALKING_DIR", "")
        return bool(repo) and os.path.isdir(repo)

    def sync(self,
             video_path: str,
             audio_path: str,
             output_path: str,
             config: LipSyncConfig) -> SyncResult:
        repo = os.environ.get("VIDEORETALKING_DIR", "")
        if not repo or not os.path.isdir(repo):
            return SyncResult(success=False, backend=self.name,
                              error="VIDEORETALKING_DIR env var not set / not a dir")

        # Pre-check: does the video contain a detectable face? If not, skip
        # lip sync entirely (non-talking-head footage like a slide deck).
        has_face = _detect_face(video_path)
        if not has_face:
            logger.info("VideoReTalking: no face detected; skipping lip sync")
            return SyncResult(
                success=True, video_path=video_path, backend=self.name,
                faces_detected=False,
                stats={"skipped": True, "reason": "no_face_detected"},
            )

        try:
            cmd = [
                "python", os.path.join(repo, "inference.py"),
                "--face", video_path,
                "--audio", audio_path,
                "--outfile", output_path,
                "--face_enhancer", config.face_enhancer,
                "--resize_factor", str(config.resize_factor),
                "--output_fps", str(config.output_fps),
                "--device", config.device,
            ]
            result = subprocess.run(
                cmd, capture_output=True, text=True, cwd=repo, timeout=3600)
            if result.returncode != 0:
                return SyncResult(success=False, backend=self.name,
                                  error=f"VideoReTalking failed: {result.stderr[-2000:]}")
            if not os.path.exists(output_path):
                return SyncResult(success=False, backend=self.name,
                                  error="VideoReTalking produced no output")
            return SyncResult(
                success=True, video_path=output_path, backend=self.name,
                faces_detected=True,
                stats={"face_enhancer": config.face_enhancer,
                       "output_fps": config.output_fps},
            )
        except subprocess.TimeoutExpired:
            return SyncResult(success=False, backend=self.name,
                              error="VideoReTalking timed out (>1h)")
        except Exception as exc:
            logger.exception("VideoReTalking failed")
            return SyncResult(success=False, backend=self.name, error=str(exc))


# --- Wav2Lip backend (baseline) -------------------------------------------

class Wav2LipBackend(LipSyncBackend):
    """Original Wav2Lip — cheapest baseline, soft mouth region.

    Operates at 96×96 mouth resolution then upscales. Acceptable quality
    on frontal well-lit footage; softer on side angles. Use only when
    VideoReTalking is unavailable.

    Repo: github.com/Rudrabha/Wav2Lip
    """

    name = "wav2lip"

    def is_available(self) -> bool:
        repo = os.environ.get("WAV2LIP_DIR", "")
        return bool(repo) and os.path.isdir(repo)

    def sync(self,
             video_path: str,
             audio_path: str,
             output_path: str,
             config: LipSyncConfig) -> SyncResult:
        repo = os.environ.get("WAV2LIP_DIR", "")
        if not repo or not os.path.isdir(repo):
            return SyncResult(success=False, backend=self.name,
                              error="WAV2LIP_DIR env var not set / not a dir")

        has_face = _detect_face(video_path)
        if not has_face:
            return SyncResult(
                success=True, video_path=video_path, backend=self.name,
                faces_detected=False,
                stats={"skipped": True, "reason": "no_face_detected"},
            )

        try:
            checkpoint = os.environ.get(
                "WAV2LIP_CHECKPOINT",
                os.path.join(repo, "checkpoints", "wav2lip_gan.pth"))
            cmd = [
                "python", os.path.join(repo, "inference.py"),
                "--checkpoint_path", checkpoint,
                "--face", video_path,
                "--audio", audio_path,
                "--outfile", output_path,
                "--resize_factor", str(config.resize_factor),
                "--device", config.device,
            ]
            result = subprocess.run(
                cmd, capture_output=True, text=True, cwd=repo, timeout=3600)
            if result.returncode != 0:
                return SyncResult(success=False, backend=self.name,
                                  error=f"Wav2Lip failed: {result.stderr[-2000:]}")
            return SyncResult(
                success=True, video_path=output_path, backend=self.name,
                faces_detected=True,
                stats={"checkpoint": os.path.basename(checkpoint)},
            )
        except Exception as exc:
            logger.exception("Wav2Lip failed")
            return SyncResult(success=False, backend=self.name, error=str(exc))


# --- LatentSync backend (future — stub) ------------------------------------

class LatentSyncBackend(LipSyncBackend):
    """LatentSync — diffusion-based lip sync (placeholder).

    Newer than Wav2Lip; produces sharper mouth region. Implement once the
    repo is released / cloned locally.
    """

    name = "latentsync"

    def is_available(self) -> bool:
        return bool(os.environ.get("LATENTSYNC_DIR")) and \
            os.path.isdir(os.environ["LATENTSYNC_DIR"])

    def sync(self, video_path, audio_path, output_path, config) -> SyncResult:
        return SyncResult(success=False, backend=self.name,
                          error="LatentSync backend not yet implemented.")


# --- Facade ----------------------------------------------------------------

_DEFAULT_BACKEND_ORDER = [
    VideoReTalkingBackend(),
    Wav2LipBackend(),
    LatentSyncBackend(),
]


class LipSyncer:
    """Facade that picks an available lip-sync backend.

    Usage::

        syncer = LipSyncer()
        if syncer.is_available():
            result = syncer.sync(video, audio, out_path,
                                config=LipSyncConfig(face_enhancer="gfpgan"))
    """

    def __init__(self,
                 backend: Optional[str] = None,
                 backends: Optional[list] = None):
        self._backends = backends if backends is not None else _DEFAULT_BACKEND_ORDER
        self._pinned = backend.lower() if backend else None
        self._active: Optional[LipSyncBackend] = None

    @property
    def active_backend(self) -> Optional[LipSyncBackend]:
        if self._active is None:
            self._active = self._select_backend()
        return self._active

    def is_available(self) -> bool:
        return self.active_backend is not None and self.active_backend.is_available()

    def list_backends(self) -> Dict[str, bool]:
        return {b.name: b.is_available() for b in self._backends}

    def sync(self,
             video_path: str,
             audio_path: str,
             output_path: str,
             config: Optional[LipSyncConfig] = None) -> SyncResult:
        if not os.path.exists(video_path):
            return SyncResult(success=False, error=f"Video not found: {video_path}")
        if not os.path.exists(audio_path):
            return SyncResult(success=False, error=f"Audio not found: {audio_path}")

        backend = self.active_backend
        if backend is None:
            return SyncResult(
                success=False,
                error=f"No lip-sync backend available. Installed: {self.list_backends()}",
            )

        cfg = config or LipSyncConfig()
        if not cfg.device:
            cfg.device = os.environ.get("LIPSYNC_DEVICE", "cuda:0")
        return backend.sync(video_path, audio_path, output_path, cfg)

    def _select_backend(self) -> Optional[LipSyncBackend]:
        if self._pinned:
            for b in self._backends:
                if b.name == self._pinned:
                    return b if b.is_available() else None
            return None
        for b in self._backends:
            if b.is_available():
                logger.info("LipSyncer: using backend %r", b.name)
                return b
        return None


# --- Face detection (gate) -------------------------------------------------

def _detect_face(video_path: str) -> bool:
    """Quick check: does the first ~2s of the video contain a face?

    Uses ffmpeg to extract a couple of frames at 0.5s and 1.5s, then probes
    with OpenCV's Haar cascade (lightweight, no torch needed). Returns True
    if a face is found in any sampled frame — used only as a gate so we don't
    waste 3 minutes running VideoReTalking on a slide deck.
    """
    try:
        import cv2  # type: ignore
    except Exception:
        # No OpenCV — assume there IS a face and let the backend decide.
        return True

    tmp_dir = video_path + ".frames"
    os.makedirs(tmp_dir, exist_ok=True)
    try:
        for t in ("0.5", "1.5"):
            frame = os.path.join(tmp_dir, f"frame_{t}.jpg")
            subprocess.run(
                [FFMPEG_PATH, "-y", "-ss", t, "-i", video_path,
                 "-frames:v", "1", "-q:v", "2", frame],
                capture_output=True, check=False,
            )
            if not os.path.exists(frame):
                continue
            img = cv2.imread(frame)
            if img is None:
                continue
            gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
            cascade = cv2.CascadeClassifier(cv2.data.haarcascades + "haarcascade_frontalface_default.xml")
            faces = cascade.detectMultiScale(gray, scaleFactor=1.1, minNeighbors=4)
            if len(faces) > 0:
                return True
        return False
    finally:
        import shutil
        shutil.rmtree(tmp_dir, ignore_errors=True)


# Convenience singleton.
lip_syncer = LipSyncer()
