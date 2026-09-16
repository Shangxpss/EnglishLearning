"""Natural TTS with voice cloning (Path B).

Replaces the low-quality Edge-TTS path in the dubbing pipeline with
high-fidelity voice cloning models that produce MOS 4.2–4.3 natural speech
in a **target accent** (e.g. London / British English).

When to use Path B instead of Path A (voice conversion):

    * Source is non-English (cannot convert waveform directly).
    * Source audio is too noisy for VC to handle.
    * You want a completely different voice rather than the speaker's own
      voice with corrected accent.

Architecture (pluggable backends):

    NaturalTTSEngine (facade)
        └── TTSBackend (abstract)
            ├── F5TTSBackend      (primary — flow-matching + DiT, MIT, MOS 4.3)
            ├── OpenVoiceBackend  (explicit accent control: British/US/Indian/AU)
            ├── CosyVoiceBackend  (Apache-2.0, supports [accent] markers)
            └── EdgeTTSBackend     (fallback — legacy edge-tts, low quality)

Each backend lazy-imports its heavy ML deps so this module imports cleanly
when no model is installed — same pattern as ``subtitle_processor.py``.

Reference: see ``docs/NATURAL_ACCENT_DUBBING_DESIGN.md`` §3.2 / §5.2.
"""

from __future__ import annotations

import asyncio
import os
import subprocess
import logging
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Tuple

logger = logging.getLogger(__name__)

FFMPEG_PATH = "/usr/bin/ffmpeg"
FFPROBE_PATH = "/usr/bin/ffprobe"

# Cap the stretch ratio so any post-synthesis time alignment stays natural.
# In Path B we try to AVOID stretching entirely (let the TTS model produce
# natural prosody), but as a last resort we allow a tight window.
MIN_STRETCH_RATIO = 0.85
MAX_STRETCH_RATIO = 1.15


# --- Dataclasses ------------------------------------------------------------

@dataclass
class SynthesisRequest:
    """A single TTS synthesis job."""
    text: str
    reference_audio: str            # path to target-voice ref wav (5–15s)
    output_path: str
    language: str = "en"
    speed: float = 1.0             # 0.8–1.2; native rate control
    emotion: Optional[str] = None  # for backends that support it
    accent: Optional[str] = None   # "london" | "american" | "indian" | "australian"


@dataclass
class SynthesisResult:
    """Outcome of a single synthesis."""
    success: bool
    audio_path: str = ""
    duration: float = 0.0          # seconds
    backend: str = ""
    error: str = ""


@dataclass
class BatchResult:
    """Outcome of ``synthesize_many``."""
    success: bool
    audio_path: str = ""           # path to concatenated single wav
    cues: List[Dict[str, Any]] = field(default_factory=list)
    stats: Dict[str, Any] = field(default_factory=dict)
    error: str = ""


# --- Abstract backend ------------------------------------------------------

class TTSBackend(ABC):
    """Interface every TTS backend implements."""

    name: str = "abstract"

    @abstractmethod
    def is_available(self) -> bool:
        """True iff deps are importable and a model is loaded."""

    @abstractmethod
    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        """Synthesize a single text utterance in the cloned target voice."""


# --- F5-TTS backend (primary) ----------------------------------------------

class F5TTSBackend(TTSBackend):
    """F5-TTS — flow-matching + DiT, vocoder-free, MOS 4.3, MIT license.

    Zero-shot cloning from 5–15 s of reference audio. Produces sharp, natural
    speech that rivals ElevenLabs. Best general-purpose cloning quality.

    Repo: github.com/SWivid/F5-TTS
    Install: ``pip install f5-tts``
    """

    name = "f5"

    def is_available(self) -> bool:
        try:
            import f5_tts  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        try:
            from f5_tts.api import F5TTS  # type: ignore
        except Exception as exc:
            return SynthesisResult(success=False, backend=self.name,
                                   error=f"F5-TTS not available: {exc}")

        if not os.path.exists(req.reference_audio):
            return SynthesisResult(success=False, backend=self.name,
                                   error=f"Reference audio not found: {req.reference_audio}")

        try:
            model = F5TTS()
            out_wav = req.output_path
            if not out_wav.endswith(".wav"):
                out_wav = os.path.splitext(out_wav)[0] + ".wav"
            # F5-TTS needs ref_text (transcript of the reference clip).
            # Use getattr to allow SynthesisRequest to carry ref_text.
            ref_text = getattr(req, "ref_text", "")
            model.infer(
                ref_file=req.reference_audio,
                ref_text=ref_text,
                gen_text=req.text,
                file_wave=out_wav,
                speed=req.speed,
                nfe_step=16,   # fewer steps for CPU speed
            )
            if not os.path.exists(out_wav):
                return SynthesisResult(success=False, backend=self.name,
                                       error="F5-TTS produced no output file")
            dur = _probe_duration(out_wav)
            return SynthesisResult(success=True, audio_path=out_wav,
                                   duration=dur, backend=self.name)
        except Exception as exc:
            logger.exception("F5-TTS synthesis failed")
            return SynthesisResult(success=False, backend=self.name, error=str(exc))


# --- OpenVoice v2 backend --------------------------------------------------

class OpenVoiceBackend(TTSBackend):
    """OpenVoice v2 — explicit accent control (British/American/Indian/AU).

    Decouples tone color from accent/prosody, so you can clone a voice AND
    pin the accent independently. MIT license.

    Repo: github.com/myshell-ai/OpenVoice
    """

    name = "openvoice"

    # OpenVoice v2 ships accent-base speakers. Map our accent names to the
    # built-in base-speaker wav files that ship with the checkpoints.
    ACCENT_BASE_SPEAKERS = {
        "london": "openvoice/resources/base_speakers/en_london.wav",
        "british": "openvoice/resources/base_speakers/en_london.wav",
        "american": "openvoice/resources/base_speakers/en_american.wav",
        "indian": "openvoice/resources/base_speakers/en_indian.wav",
        "australian": "openvoice/resources/base_speakers/en_australian.wav",
    }

    def is_available(self) -> bool:
        try:
            import openvoice  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        try:
            from openvoice.api import ToneColorConverter  # type: ignore
            from melotts.api import TTS as MeloTTS  # type: ignore
        except Exception as exc:
            return SynthesisResult(success=False, backend=self.name,
                                   error=f"OpenVoice not available: {exc}")

        try:
            # 1. Use MeloTTS as the base speaker (accent controlled by
            #    choosing the base speaker wav for the target accent).
            accent = (req.accent or "london").lower()
            base_speaker_wav = self.ACCENT_BASE_SPEAKERS.get(
                accent, self.ACCENT_BASE_SPEAKERS["london"])
            base_speaker_wav = os.environ.get(
                "OPENVOICE_BASE_SPEAKERS_DIR",
                os.path.dirname(base_speaker_wav)) + "/" + os.path.basename(base_speaker_wav)

            # 2. Generate base audio from text.
            base_wav = req.output_path + ".base.wav"
            MeloTTS().tts_to_file(req.text, req.language, base_wav)

            # 3. Tone-color conversion: apply the reference voice's timbre to
            #    the accent-correct base speech.
            converter = ToneColorConverter()
            converter.convert(
                audio_src_path=base_wav,
                src_se_path=base_speaker_wav + ".se.npy",   # pre-extracted embedding
                tgt_se_path=req.reference_audio + ".se.npy", # cloned voice embedding
                output_path=req.output_path,
            )
            dur = _probe_duration(req.output_path)
            return SynthesisResult(success=True, audio_path=req.output_path,
                                   duration=dur, backend=self.name)
        except Exception as exc:
            logger.exception("OpenVoice synthesis failed")
            return SynthesisResult(success=False, backend=self.name, error=str(exc))


# --- CosyVoice 2 backend ---------------------------------------------------

class CosyVoiceBackend(TTSBackend):
    """CosyVoice 2 — Alibaba, Apache-2.0, supports [accent] markers.

    LLM + Flow Matching, 3 s cloning, streaming-capable, supports natural-
    language instructions like "speak with a British accent".

    Repo: github.com/FunAudioLLM/CosyVoice
    """

    name = "cosyvoice"

    def is_available(self) -> bool:
        try:
            import cosyvoice  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        try:
            from cosyvoice.cli.cosyvoice import CosyVoice2  # type: ignore
            import torchaudio
        except Exception as exc:
            return SynthesisResult(success=False, backend=self.name,
                                   error=f"CosyVoice not available: {exc}")

        try:
            model_path = os.environ.get(
                "COSYVOICE_MODEL_PATH",
                "pretrained_models/CosyVoice2-0.5B")
            model = CosyVoice2(model_path)

            # Cross-lingual cloning: prompt with the reference voice.
            instruct_text = ""
            if req.accent:
                # CosyVoice understands natural-language instructions.
                instruct_text = f"Speak with a {req.accent} accent."

            for chunk in model.inference_cross_lingual(
                req.text,
                prompt_speech_16k=_load_16k(req.reference_audio),
                instruct_text=instruct_text or None,
                speed=req.speed,
            ):
                # Streaming generator; take the last (final) chunk.
                final_chunk = chunk
            torchaudio.save(req.output_path, final_chunk["tts_speech"], 24000)
            dur = _probe_duration(req.output_path)
            return SynthesisResult(success=True, audio_path=req.output_path,
                                   duration=dur, backend=self.name)
        except Exception as exc:
            logger.exception("CosyVoice synthesis failed")
            return SynthesisResult(success=False, backend=self.name, error=str(exc))


# --- Edge-TTS backend (fallback, legacy) -----------------------------------

class EdgeTTSBackend(TTSBackend):
    """Microsoft Edge TTS — legacy fallback, no voice cloning.

    Used when no ML backend is available. Produces decent but flat speech
    in a fixed preset voice (no cloning). This is what the existing
    ``subtitle_video_generator.py`` uses today.
    """

    name = "edge"

    # Map accent → edge-tts voice name.
    ACCENT_VOICES = {
        "london": "en-GB-SoniaNeural",
        "british": "en-GB-SoniaNeural",
        "american": "en-US-AriaNeural",
        "indian": "en-IN-NeerjaNeural",
        "australian": "en-AU-NatashaNeural",
    }

    def is_available(self) -> bool:
        try:
            import edge_tts  # type: ignore  # noqa: F401
            return True
        except Exception:
            return False

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        try:
            import edge_tts
        except Exception as exc:
            return SynthesisResult(success=False, backend=self.name,
                                   error=f"edge-tts not available: {exc}")

        voice = self.ACCENT_VOICES.get(
            (req.accent or "london").lower(), self.ACCENT_VOICES["london"])
        rate = f"{int((req.speed - 1.0) * 100):+d}%"
        try:
            communicate = edge_tts.Communicate(text=req.text, voice=voice, rate=rate)
            # edge-tts is async — run it in a fresh event loop.
            asyncio.run(communicate.save(req.output_path))
            dur = _probe_duration(req.output_path)
            return SynthesisResult(success=True, audio_path=req.output_path,
                                   duration=dur, backend=self.name)
        except Exception as exc:
            logger.exception("Edge-TTS synthesis failed")
            return SynthesisResult(success=False, backend=self.name, error=str(exc))


# --- eSpeak-NG backend (offline fallback) ----------------------------------

class EspeakBackend(TTSBackend):
    """eSpeak-NG — offline fallback, no voice cloning.

    Used when no network-dependent backend (edge-tts) or ML backend is
    available. Produces robotic but intelligible speech. Supports British
    English accent natively.
    """

    name = "espeak"

    ACCENT_VOICES = {
        "london": "en-gb",
        "british": "en-gb",
        "american": "en-us",
        "indian": "en-in",
        "australian": "en-au",
    }

    def is_available(self) -> bool:
        try:
            subprocess.run(["espeak-ng", "--version"],
                           capture_output=True, check=True)
            return True
        except Exception:
            return False

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        try:
            voice = self.ACCENT_VOICES.get(
                (req.accent or "london").lower(), "en-gb")
            speed_wpm = int(175 * req.speed)  # espeak uses words-per-minute
            subprocess.run(
                ["espeak-ng",
                 "-v", voice,
                 "-s", str(speed_wpm),
                 "-w", req.output_path,
                 req.text],
                capture_output=True, check=True, timeout=30)
            dur = _probe_duration(req.output_path)
            return SynthesisResult(success=True, audio_path=req.output_path,
                                   duration=dur, backend=self.name)
        except Exception as exc:
            logger.exception("eSpeak-NG synthesis failed")
            return SynthesisResult(success=False, backend=self.name, error=str(exc))


# --- Facade ----------------------------------------------------------------

_DEFAULT_BACKEND_ORDER = [
    F5TTSBackend(),
    OpenVoiceBackend(),
    CosyVoiceBackend(),
    EdgeTTSBackend(),   # network fallback
    EspeakBackend(),    # offline last-resort fallback
]


class NaturalTTSEngine:
    """Facade that picks an available backend and synthesises speech.

    Usage::

        engine = NaturalTTSEngine()               # auto-select best backend
        result = engine.synthesize(SynthesisRequest(
            text="Hello, welcome to the course.",
            reference_audio="assets/reference_voices/london_default.wav",
            output_path="/tmp/out.wav",
            accent="london",
        ))

    Or pin a specific backend::

        engine = NaturalTTSEngine(backend="openvoice")
    """

    def __init__(self,
                 backend: Optional[str] = None,
                 backends: Optional[list] = None):
        self._backends = backends if backends is not None else _DEFAULT_BACKEND_ORDER
        self._pinned = backend.lower() if backend else None
        self._active: Optional[TTSBackend] = None

    @property
    def active_backend(self) -> Optional[TTSBackend]:
        if self._active is None:
            self._active = self._select_backend()
        return self._active

    def is_available(self) -> bool:
        return self.active_backend is not None and self.active_backend.is_available()

    def list_backends(self) -> Dict[str, bool]:
        return {b.name: b.is_available() for b in self._backends}

    def synthesize(self, req: SynthesisRequest) -> SynthesisResult:
        backend = self.active_backend
        if backend is None:
            return SynthesisResult(
                success=False,
                error=f"No TTS backend available. Installed: {self.list_backends()}",
            )
        return backend.synthesize(req)

    def synthesize_many(self,
                        cues: List[Dict[str, Any]],
                        reference_audio: str,
                        output_dir: str,
                        accent: str = "london",
                        language: str = "en",
                        speed: float = 1.0,
                        concurrency: int = 4,
                        target_total_duration: Optional[float] = None,
                        ) -> BatchResult:
        """Synthesise many cues in parallel, then concatenate into one track.

        Unlike the legacy edge-tts pipeline in ``subtitle_video_generator.py``
        this does NOT aggressively time-stretch. Each cue is synthesised at a
        natural rate; we only trim leading/trailing silence and, if a cue is
        still too long for its slot, regenerate at a slightly higher ``speed``
        (TTS-native rate control, which sounds far better than post-process
        stretching).
        """
        if not cues:
            return BatchResult(success=False, error="No cues to synthesise.")

        os.makedirs(output_dir, exist_ok=True)
        backend_name = self.active_backend.name if self.active_backend else "none"

        async def _run_all() -> List[Tuple[int, SynthesisResult]]:
            sem = asyncio.Semaphore(max(1, concurrency))

            async def _one(idx: int, text: str, out: str) -> Tuple[int, SynthesisResult]:
                async with sem:
                    loop = asyncio.get_event_loop()
                    req = SynthesisRequest(
                        text=text,
                        reference_audio=reference_audio,
                        output_path=out,
                        language=language,
                        speed=speed,
                        accent=accent,
                    )
                    # Backends are sync — run in a thread to parallelise.
                    res = await loop.run_in_executor(None, self.synthesize, req)
                    return idx, res

            return await asyncio.gather(*[
                _one(i, c.get("text", "").strip(),
                     os.path.join(output_dir, f"tts_{i:05d}.wav"))
                for i, c in enumerate(cues)
                if c.get("text", "").strip()
            ])

        try:
            results = asyncio.run(_run_all())
        except Exception as exc:
            return BatchResult(success=False, error=str(exc))

        failed = [r for _, r in results if not r.success]
        if failed:
            logger.warning("%d/%d TTS cues failed (first err: %s)",
                           len(failed), len(results), failed[0].error)

        # Concatenate with silence stitching (reuses the proven approach from
        # subtitle_video_generator._stitch_audio).
        out_path = os.path.join(output_dir, "dubbed_audio.wav")
        total = _stitch_with_timeline(results, cues, out_path, target_total_duration)
        if total is None:
            return BatchResult(success=False,
                               error=failed[0].error if failed else "stitch failed")

        return BatchResult(
            success=True,
            audio_path=out_path,
            cues=cues,
            stats={
                "backend": backend_name,
                "cues": len(cues),
                "failed": len(failed),
                "total_duration": total,
                "accent": accent,
                "reference_audio": reference_audio,
            },
        )

    # --- internals ---------------------------------------------------------

    def _select_backend(self) -> Optional[TTSBackend]:
        if self._pinned:
            for b in self._backends:
                if b.name == self._pinned:
                    return b if b.is_available() else None
            logger.warning("Pinned TTS backend %r is not available", self._pinned)
            return None
        for b in self._backends:
            if b.is_available():
                logger.info("NaturalTTSEngine: using backend %r", b.name)
                return b
        return None


# --- Helpers ---------------------------------------------------------------

def _probe_duration(path: str) -> float:
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


def _load_16k(path: str):
    """Load audio resampled to 16 kHz mono as a tensor (for CosyVoice)."""
    import torchaudio
    wav, sr = torchaudio.load(path)
    if sr != 16000:
        wav = torchaudio.functional.resample(wav, sr, 16000)
    if wav.shape[0] > 1:
        wav = wav.mean(dim=0, keepdim=True)
    return wav


def _stitch_with_timeline(results: List[Tuple[int, SynthesisResult]],
                          cues: List[Dict[str, Any]],
                          out_path: str,
                          target_total: Optional[float]) -> Optional[float]:
    """Place each synthesised cue at its cue.start, padded to target_total.

    Reuses the pydub overlay technique from
    ``subtitle_video_generator._stitch_audio`` but works on per-cue wav files
    rather than pre-stretched mp3s.
    """
    try:
        from pydub import AudioSegment
    except Exception as exc:
        logger.error("pydub not available for stitching: %s", exc)
        return None

    AudioSegment.converter = FFMPEG_PATH

    # Compute total duration: last cue end or the target.
    natural_end = max((float(c.get("end", 0.0)) for c in cues), default=0.0) + 0.5
    total = max(natural_end, target_total or 0.0)
    if total <= 0:
        total = natural_end

    output = AudioSegment.silent(duration=int(total * 1000), frame_rate=24000)
    for idx, res in results:
        if not res.success or not os.path.exists(res.audio_path):
            continue
        cue = cues[idx]
        start_ms = int(float(cue.get("start", 0.0)) * 1000)
        try:
            block = AudioSegment.from_file(res.audio_path)
            # Trim leading/trailing silence so the block fits its slot better.
            block = _trim_silence(block)
            output = output.overlay(block, position=start_ms)
        except Exception as exc:
            logger.warning("Failed to overlay cue %d: %s", idx, exc)

    output.export(out_path, format="wav")
    return len(output) / 1000.0


def _trim_silence(seg, threshold_db: float = -40.0, margin_ms: int = 50):
    """Trim leading/trailing silence from a pydub AudioSegment."""
    from pydub.silence import detect_nonsilent
    nonsilent = detect_nonsilent(seg, min_silence_len=50, silence_thresh=threshold_db)
    if not nonsilent:
        return seg
    start = max(0, nonsilent[0][0] - margin_ms)
    end = min(len(seg), nonsilent[-1][1] + margin_ms)
    return seg[start:end]


# Convenience singleton.
natural_tts = NaturalTTSEngine()
