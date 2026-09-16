"""Voice Conversion (Path A).

Converts a source audio waveform into a target voice (e.g. a London-accent
reference speaker) while **preserving the linguistic content, prosody, timing,
and emotion** of the original speech.

This is the preferred path for "fix the accent, keep the speaker's cadence"
because — unlike TTS — it operates directly on the waveform and produces
output whose duration matches the input duration. **No time-stretch is
needed downstream**, which is the single biggest quality win over the
"Generate & Time-Stretch" pipeline.

Architecture (pluggable backends):

    VoiceConverter (facade)
        └── VoiceConversionBackend (abstract)
            ├── RVCBackend         (primary — Retrieval-based VC, MIT)
            ├── KNNVCBackend        (alternative — kNN-VC, MIT)
            └── CosyAccentBackend   (future — duration-controllable accent norm)

Each backend lazy-imports its heavy ML deps (torch / RVC / etc.) so the module
imports cleanly when no model is installed — same pattern as
``subtitle_processor.py`` and ``subtitle_video_generator.py``.

Reference: see ``docs/NATURAL_ACCENT_DUBBING_DESIGN.md`` §3.1 / §5.1.
"""

from __future__ import annotations

import os
import subprocess
import logging
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)

# Use system ffmpeg (consistent with subtitle_video_generator.py).
FFMPEG_PATH = "/usr/bin/ffmpeg"
FFPROBE_PATH = "/usr/bin/ffprobe"


# --- Result / config dataclasses -------------------------------------------

@dataclass
class ConversionResult:
    """Outcome of a single ``convert()`` call."""
    success: bool
    audio_path: str = ""
    sample_rate: int = 22050
    duration: float = 0.0          # seconds (≈ input duration for VC)
    backend: str = ""
    error: str = ""
    stats: Dict[str, Any] = field(default_factory=dict)


@dataclass
class VoiceConversionConfig:
    """Configuration for a conversion run.

    Defaults are the RVC recommended values; tunable per-request.
    """
    model_path: str = ""               # RVC .pth checkpoint
    index_path: Optional[str] = None   # feature index (.index) for retrieval
    device: str = "cuda:0"             # "cpu" works but ~10x slower
    pitch_shift: int = 0               # semitones; 0 = keep original pitch
    search_feature_ratio: float = 0.66  # accent strength (0.0–1.0)
    filter_radius: int = 3             # pitch median-filter radius (0–10)
    protect_breath: float = 0.33       # preserve breath sounds (0.0–0.5)
    resample_rate: int = 0             # 0 = no resample; else target Hz


# --- Abstract backend ------------------------------------------------------

class VoiceConversionBackend(ABC):
    """Interface every voice-conversion backend must implement."""

    name: str = "abstract"

    @abstractmethod
    def is_available(self) -> bool:
        """Return True iff the backend's heavy deps are importable and a model
        is loaded / configured."""

    @abstractmethod
    def convert(self,
                source_wav_path: str,
                output_wav_path: str,
                config: VoiceConversionConfig) -> ConversionResult:
        """Convert ``source_wav_path`` into the target voice.

        The output duration MUST match the input duration (±50 ms). This is
        the contract that lets the orchestrator skip time-stretching.
        """


# --- RVC backend (primary) -------------------------------------------------

class RVCBackend(VoiceConversionBackend):
    """Retrieval-based Voice Conversion.

    Uses HuBERT feature extraction + FAISS retrieval + HiFi-GAN vocoder.
    Preserves emotion, prosody, and timing; only the timbre/accent changes.

    Requires the ``rvc`` Python package (or the RVC WebUI inference scripts on
    ``PYTHONPATH``) and a trained ``.pth`` model + optional ``.index`` file.

    To train a London-accent target model: record 10–30 min of clean
    London/Estuary English speech from one speaker, then run RVC's training
    pipeline. Ship the resulting ``.pth`` + ``.index`` under
    ``app/assets/rvc_models/``.
    """

    name = "rvc"

    def is_available(self) -> bool:
        try:
            # The RVC inference entry point. The official RVC WebUI exposes
            # ``infer.lib.infer_pack.models`` or a CLI; community forks vary.
            # We probe for the most common import paths.
            import importlib
            for mod in ("rvc", "infer.lib.infer_pack.models",
                        "config", "infer.modules.vc.modules"):
                try:
                    importlib.import_module(mod)
                    return True
                except Exception:
                    continue
            return False
        except Exception:
            return False

    def convert(self,
                source_wav_path: str,
                output_wav_path: str,
                config: VoiceConversionConfig) -> ConversionResult:
        if not config.model_path or not os.path.exists(config.model_path):
            return ConversionResult(
                success=False,
                backend=self.name,
                error=f"RVC model not found: {config.model_path!r}",
            )

        src_dur = _probe_duration(source_wav_path)
        try:
            converted = self._run_rvc_inference(source_wav_path, output_wav_path, config)
        except Exception as exc:
            logger.exception("RVC inference failed")
            return ConversionResult(success=False, backend=self.name, error=str(exc))

        out_dur = _probe_duration(converted)
        # RVC preserves duration, but tiny drift can occur at edges — force
        # exact match with ffmpeg apad/atrim so downstream mux is sample-aligned.
        if src_dur > 0 and abs(out_dur - src_dur) > 0.05:
            logger.info("RVC duration drift %.3fs -> forcing %.3fs",
                        out_dur - src_dur, src_dur)
            _force_duration(converted, src_dur)
            out_dur = src_dur

        return ConversionResult(
            success=True,
            audio_path=converted,
            sample_rate=config.resample_rate or 22050,
            duration=out_dur,
            backend=self.name,
            stats={
                "model": os.path.basename(config.model_path),
                "pitch_shift": config.pitch_shift,
                "search_feature_ratio": config.search_feature_ratio,
                "duration_drift_ms": int((out_dur - src_dur) * 1000),
            },
        )

    # --- internals ---------------------------------------------------------

    def _run_rvc_inference(self,
                           source_wav_path: str,
                           output_wav_path: str,
                           config: VoiceConversionConfig) -> str:
        """Run a single RVC inference.

        Tries the Python API first; falls back to invoking the RVC WebUI's
        ``infer-web.py`` via subprocess if the API import fails.
        """
        # --- API path (preferred) ---
        try:
            from infer.modules.vc.modules import VC  # type: ignore
            from infer.lib.infer_pack.models import (  # type: ignore
                SynthesizerTrnMs256NSFsid,
                SynthesizerTrnMs768NSFsid,
            )
            vc = VC(config.model_path, config.device)
            info = vc.get_vc(config.model_path, config.protect_breath, config.protect_breath)
            wav_opt = vc.vc_single(
                sid=0,
                input_audio_path=source_wav_path,
                f0_up_key=config.pitch_shift,
                f0_file=None,
                f0_method="rmvpe",           # robust multi-voice pitch estimation
                file_index=config.index_path or "",
                file_index2="",
                index_rate=config.search_feature_ratio,
                filter_radius=config.filter_radius,
                resample_sr=config.resample_rate,
                rms_mix_rate=0.25,           # keep original loudness envelope
                protect=config.protect_breath,
            )
            import soundfile as sf
            sf.write(output_wav_path, wav_opt[1], wav_opt[0])
            return output_wav_path
        except ImportError:
            logger.info("RVC Python API not available; trying subprocess CLI")
        except Exception as exc:
            logger.warning("RVC API path failed (%s); trying CLI", exc)

        # --- CLI fallback ---
        # Expects the RVC WebUI repo cloned somewhere on disk; configured via
        # the RVC_WEBUI_DIR env var.
        rvc_dir = os.environ.get("RVC_WEBUI_DIR", "")
        if not rvc_dir or not os.path.isdir(rvc_dir):
            raise RuntimeError(
                "RVC inference unavailable: neither the Python API nor "
                "RVC_WEBUI_DIR is configured. Install RVC or set RVC_WEBUI_DIR."
            )
        cmd = [
            "python", os.path.join(rvc_dir, "infer-web.py"),
            "--source", source_wav_path,
            "--output", output_wav_path,
            "--model", config.model_path,
            "--index", config.index_path or "",
            "--pitch", str(config.pitch_shift),
            "--index_rate", str(config.search_feature_ratio),
            "--filter_radius", str(config.filter_radius),
            "--protect", str(config.protect_breath),
            "--device", config.device,
        ]
        result = subprocess.run(cmd, capture_output=True, text=True, cwd=rvc_dir)
        if result.returncode != 0:
            raise RuntimeError(f"RVC CLI failed: {result.stderr[-2000:]}")
        if not os.path.exists(output_wav_path):
            raise RuntimeError("RVC CLI did not produce output")
        return output_wav_path


# --- kNN-VC backend (alternative) -----------------------------------------

class KNNVCBackend(VoiceConversionBackend):
    """k-Nearest-Neighbors Voice Conversion.

    Higher speaker-similarity than RVC; uses FAISS retrieval over ContentVec
    features. Good when accent preservation matters more than speed.

    Requires the ``knn-vc`` package (github.com/bshall/knn-vc).
    """

    name = "knnvc"

    def is_available(self) -> bool:
        try:
            import knn_vc  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def convert(self,
                source_wav_path: str,
                output_wav_path: str,
                config: VoiceConversionConfig) -> ConversionResult:
        try:
            import torch
            import torchaudio
            from knn_vc import KNNet  # type: ignore
        except Exception as exc:
            return ConversionResult(
                success=False, backend=self.name,
                error=f"kNN-VC not available: {exc}",
            )

        src_dur = _probe_duration(source_wav_path)
        try:
            device = torch.device(config.device if torch.cuda.is_available()
                                  else "cpu")
            model = KNNet(device=device)

            # Build the target-feature bank from the reference speaker's wav(s).
            ref_bank = model.extract_features(
                config.model_path, device=device) if config.model_path else None

            wav, sr = torchaudio.load(source_wav_path)
            wav = wav.to(device)
            converted = model.convert(wav, ref_bank,
                                      topk=int(config.search_feature_ratio * 100))
            torchaudio.save(output_wav_path, converted.cpu(), sr)
        except Exception as exc:
            logger.exception("kNN-VC inference failed")
            return ConversionResult(success=False, backend=self.name, error=str(exc))

        out_dur = _probe_duration(output_wav_path)
        if src_dur > 0 and abs(out_dur - src_dur) > 0.05:
            _force_duration(output_wav_path, src_dur)
            out_dur = src_dur

        return ConversionResult(
            success=True, audio_path=output_wav_path,
            sample_rate=config.resample_rate or 22050,
            duration=out_dur, backend=self.name,
            stats={"topk": int(config.search_feature_ratio * 100)},
        )


# --- CosyAccent backend (future — stub) ------------------------------------

class CosyAccentBackend(VoiceConversionBackend):
    """Duration-controllable accent normalization (placeholder).

    CosyAccent (arXiv:2602.19166, Feb 2026) is purpose-built for "remove L2
    accent → native accent, keep duration" — the ideal fit for this problem.
    It is non-autoregressive and offers explicit total-duration control.

    This is a stub: implement once the model + checkpoints are released.
    """

    name = "cosyaccent"

    def is_available(self) -> bool:
        try:
            import cosyaccent  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def convert(self,
                source_wav_path: str,
                output_wav_path: str,
                config: VoiceConversionConfig) -> ConversionResult:
        return ConversionResult(
            success=False, backend=self.name,
            error="CosyAccent backend not yet implemented (awaiting model release).",
        )


# --- Facade ----------------------------------------------------------------

# Ordered by preference: RVC (best general), kNN-VC (best similarity), CosyAccent.
_DEFAULT_BACKEND_ORDER = [RVCBackend(), KNNVCBackend(), CosyAccentBackend()]


class VoiceConverter:
    """Facade that picks an available backend and runs voice conversion.

    Usage::

        converter = VoiceConverter()                 # auto-select backend
        result = converter.convert(
            source_wav_path="/tmp/bad_accent.wav",
            output_wav_path="/tmp/london.wav",
            config=VoiceConversionConfig(model_path="assets/rvc_models/london.pth"),
        )

    Or pin a specific backend::

        converter = VoiceConverter(backend="knnvc")
    """

    def __init__(self,
                 backend: Optional[str] = None,
                 backends: Optional[list] = None):
        self._backends = backends if backends is not None else _DEFAULT_BACKEND_ORDER
        self._pinned = backend.lower() if backend else None
        self._active: Optional[VoiceConversionBackend] = None

    @property
    def active_backend(self) -> Optional[VoiceConversionBackend]:
        if self._active is None:
            self._active = self._select_backend()
        return self._active

    def is_available(self) -> bool:
        return self.active_backend is not None and self.active_backend.is_available()

    def list_backends(self) -> Dict[str, bool]:
        """Return availability of every known backend (for /health output)."""
        return {b.name: b.is_available() for b in self._backends}

    def convert(self,
                source_wav_path: str,
                output_wav_path: str,
                config: Optional[VoiceConversionConfig] = None) -> ConversionResult:
        if not os.path.exists(source_wav_path):
            return ConversionResult(success=False,
                                    error=f"Source not found: {source_wav_path}")

        backend = self.active_backend
        if backend is None:
            avail = self.list_backends()
            return ConversionResult(
                success=False,
                error=(f"No voice-conversion backend available. "
                       f"Installed: {avail}"),
            )

        cfg = config or VoiceConversionConfig()
        # Allow env-var overrides when the caller didn't pin a config.
        if not cfg.model_path:
            cfg.model_path = os.environ.get("RVC_MODEL_PATH", "")
        if cfg.index_path is None:
            cfg.index_path = os.environ.get("RVC_INDEX_PATH") or None
        if not cfg.device:
            cfg.device = os.environ.get("RVC_DEVICE", "cuda:0")

        return backend.convert(source_wav_path, output_wav_path, cfg)

    # --- internals ---------------------------------------------------------

    def _select_backend(self) -> Optional[VoiceConversionBackend]:
        if self._pinned:
            for b in self._backends:
                if b.name == self._pinned:
                    return b if b.is_available() else None
            logger.warning("Pinned backend %r is not available", self._pinned)
            return None
        for b in self._backends:
            if b.is_available():
                logger.info("VoiceConverter: using backend %r", b.name)
                return b
        return None


# --- ffmpeg helpers (shared with other modules) ----------------------------

def _probe_duration(path: str) -> float:
    """Return media duration in seconds via ffprobe."""
    try:
        out = subprocess.run(
            [FFPROBE_PATH, "-v", "error",
             "-show_entries", "format=duration",
             "-of", "default=noprint_wrappers=1:nokey=1", path],
            capture_output=True, text=True, check=True,
        )
        return float(out.stdout.strip())
    except Exception:
        return 0.0


def _force_duration(path: str, target: float) -> None:
    """Trim or pad audio to exactly ``target`` seconds (in-place)."""
    tmp = path + ".tmp.wav"
    subprocess.run(
        [FFMPEG_PATH, "-y", "-i", path,
         "-t", f"{target:.3f}",
         "-af", f"apad=whole_dur={target:.3f}",
         tmp],
        capture_output=True, check=True,
    )
    os.replace(tmp, path)


# Convenience singleton — mirrors the pattern used by audio_processor /
# subtitle_video_generator / video_dubber.
voice_converter = VoiceConverter()
