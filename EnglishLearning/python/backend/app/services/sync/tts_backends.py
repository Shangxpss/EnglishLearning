"""Pluggable TTS backends for the sync dubbing pipeline.

Two backends are provided:

- :class:`ChatTTSBackend` — local CPU ChatTTS. Gender via random seed
  (community-verified pool). Accent is **approximated** by British English
  vocabulary substitution (ChatTTS has no native accent API). Use when the
  network is unavailable.

- :class:`EdgeTTSBackend` — Microsoft Edge Neural TTS via the public "Read
  Aloud" service. **Native accent support** — ``en-GB-SoniaNeural`` /
  ``en-GB-LibbyNeural`` / ``en-GB-MaisieNeural`` are Southern British / RP
  female voices. Use this when real accent control is required.

The orchestrator (``run_sync_dub.py``) picks the backend from
``VoiceConfig.backend`` via :func:`build_backend`.

Fix history (this revision)
---------------------------
* EdgeTTS backend added — the previous "London accent" was a vocabulary
  substitution hack (``color``→``colour``). edge-tts gives genuine
  Southern British / RP female speakers.

* ChatTTSBackend now applies ``tanh`` soft-clip before writing the wav.
  ChatTTS was producing hard digital clipping (peaks pinned at exactly
  ±1.0, up to 152 consecutive clipped samples in one sentence), which was
  the root cause of the "electronic / robotic" sound on ~8 sentences.
  See ``docs/TTS_SYNC_MECHANISM.md`` §10 for the empirical diagnosis.

* EdgeTTS proxy handling: edge-tts does **not** honor ``http_proxy`` /
  ``https_proxy`` env vars for its WebSocket synthesis endpoint (it only
  honors them for the HTTPS voice-list GET). The proxy must be passed
  explicitly via ``Communicate(proxy=...)``. This is the cause of the
  previous "WebSocket connection timeout" failure documented in §8.4 of
  the design doc.
"""

from __future__ import annotations

import logging
import os
from abc import ABC, abstractmethod
from typing import Optional

import numpy as np
import soundfile as sf

from .voice_config import VoiceConfig

logger = logging.getLogger(__name__)


class TTSBackend(ABC):
    """Abstract TTS interface used by the dubbing pipeline."""

    sample_rate: int = 24000

    @abstractmethod
    def synth(self, text: str, out_wav: str) -> bool:
        """Synthesize ``text`` to ``out_wav`` (mono wav).

        Returns True on success, False on failure (caller skips).
        """


def _soft_clip(wav: np.ndarray, drive: float = 0.9) -> np.ndarray:
    """``tanh`` soft-clip — eliminates flat-top distortion from ChatTTS.

    ChatTTS sometimes produces samples pinned at exactly ±1.0 (hard digital
    clipping), which sounds harsh / electronic. ``tanh`` smooths the
    waveform at the extremes while preserving the inner dynamics. Output
    stays in ``[-1, 1]``.

    ``drive`` controls how aggressive the soft-knee is (0.9 is gentle —
    only the top ~15% of the dynamic range is reshaped).
    """
    wav = np.asarray(wav, dtype=np.float32)
    return np.tanh(wav * drive) / float(np.tanh(drive))


class ChatTTSBackend(TTSBackend):
    """ChatTTS backend — local CPU, gender via seed, accent approximated.

    Gender is selected by the random seed passed to ``torch.manual_seed``
    before ``chat.sample_random_speaker()``. Accent is approximated via
    British vocabulary substitution (ChatTTS has no native accent API —
    see ``docs/TTS_SYNC_MECHANISM.md`` §8).
    """

    def __init__(self, voice: VoiceConfig):
        import ChatTTS  # noqa: E402
        import torch  # noqa: E402

        self._torch = torch
        self.voice = voice
        torch.set_num_threads(2)

        self.chat = ChatTTS.Chat()
        self.chat.load(source="huggingface", compile=False, device="cpu")
        seed = voice.resolve_seed()
        torch.manual_seed(seed)
        self.rand_spk = self.chat.sample_random_speaker()
        # ChatTTS produces 24 kHz audio.
        self.sample_rate = 24000
        logger.info("ChatTTSBackend loaded: %s", voice.describe())

    def synth(self, text: str, out_wav: str) -> bool:
        import ChatTTS  # noqa: E402

        synth_text = self.voice.apply_british_vocabulary(text) if \
            self.voice.british_vocabulary else text

        params_infer = ChatTTS.Chat.InferCodeParams(
            spk_emb=self.rand_spk,
            temperature=self.voice.temperature,
            top_P=self.voice.top_P,
            top_K=self.voice.top_K,
        )
        params_refine = ChatTTS.Chat.RefineTextParams(
            temperature=0.7,
            prompt=self.voice.refine_prompt,
        )
        try:
            wavs = self.chat.infer(
                synth_text,
                params_refine_text=params_refine,
                params_infer_code=params_infer,
            )
            wav = np.array(wavs[0], dtype=np.float32).squeeze()
            # Soft-clip BEFORE writing — eliminates the hard digital clipping
            # that produces the "electronic" sound on some sentences.
            # See §10 of the design doc for the empirical diagnosis.
            wav = _soft_clip(wav)
            peak = float(np.max(np.abs(wav))) if wav.size else 0.0
            if peak > 0.95:
                wav *= 0.95 / peak
            sf.write(out_wav, wav, self.sample_rate)
            del wavs, wav
            return True
        except Exception as e:
            logger.warning("ChatTTS synth failed: %s", e)
            return False


class EdgeTTSBackend(TTSBackend):
    """edge-tts backend — Microsoft Neural TTS with **native accent**.

    Uses the public Edge "Read Aloud" service. No API key, no model
    download (~5 MB package). Synthesis happens on Microsoft's servers.

    Critically, this backend offers true native ``en-GB-*`` voices
    (``en-GB-SoniaNeural``, ``en-GB-LibbyNeural``, ``en-GB-MaisieNeural``)
    which are Southern British / RP female speakers — the real accent the
    user requested. This eliminates the ChatTTS vocabulary-substitution
    approximation.

    Requires network access. If ``VoiceConfig.proxy`` is set, passes it to
    edge-tts explicitly (edge-tts does NOT honor ``http_proxy`` /
    ``https_proxy`` env vars for the WebSocket synthesis endpoint — this is
    the cause of the previous "WebSocket connection timeout" failure).
    """

    # Microsoft neural voices are 24 kHz mono.
    sample_rate = 24000

    def __init__(self, voice: VoiceConfig):
        import edge_tts  # noqa: E402

        self._edge_tts = edge_tts
        self.voice = voice
        # Resolve the actual voice name (explicit override or gender default).
        self._voice_name = voice.resolve_edge_voice()
        logger.info(
            "EdgeTTSBackend: voice=%s rate=%s volume=%s proxy=%s",
            self._voice_name, voice.edge_rate, voice.edge_volume,
            voice.proxy or "(none)",
        )

    def synth(self, text: str, out_wav: str) -> bool:
        # edge-tts's voice IS British — no vocabulary substitution needed.
        clean = text
        # Save mp3 then convert to wav via PyAV (no system ffmpeg subprocess).
        tmp_mp3 = out_wav + ".tmp.mp3"
        try:
            comm = self._edge_tts.Communicate(
                clean,
                voice=self._voice_name,
                proxy=self.voice.proxy,
                rate=self.voice.edge_rate,
                volume=self.voice.edge_volume,
            )
            comm.save_sync(tmp_mp3)
        except Exception as e:
            logger.warning("edge-tts synth failed: %s", e)
            return False
        try:
            from .av_utils import mp3_to_wav
            mp3_to_wav(tmp_mp3, out_wav,
                       target_sr=self.sample_rate, mono=True)
        except Exception as e:
            logger.warning("PyAV mp3->wav failed: %s", e)
            return False
        finally:
            if os.path.exists(tmp_mp3):
                os.remove(tmp_mp3)
        return True

    def synth_many(self, items, max_concurrency: int = 8,
                   progress_callback=None) -> dict:
        """Synthesize many sentences concurrently via asyncio.gather().

        edge-tts is async-native (aiohttp WebSocket). Running all sentences
        concurrently turns N sequential network round-trips into one
        batched wave of parallel requests — bounded by ``max_concurrency``
        to avoid Microsoft rate-limiting.

        Args:
            items: iterable of ``(out_wav_path, text)`` tuples.
            max_concurrency: max simultaneous in-flight requests.
            progress_callback: optional callable(completed: int, total: int)
                called after each item finishes (success or fail). Use for
                per-sentence progress reporting to the frontend.

        Returns:
            dict mapping ``out_wav_path -> bool`` (True on success).
        """
        import asyncio
        from .av_utils import mp3_to_wav

        items = list(items)
        total = len(items)
        sem = asyncio.Semaphore(max_concurrency)
        results = {}
        completed = 0

        async def _one(out_wav: str, text: str) -> bool:
            tmp_mp3 = out_wav + ".tmp.mp3"
            async with sem:
                try:
                    comm = self._edge_tts.Communicate(
                        text,
                        voice=self._voice_name,
                        proxy=self.voice.proxy,
                        rate=self.voice.edge_rate,
                        volume=self.voice.edge_volume,
                    )
                    await comm.save(tmp_mp3)
                except Exception as e:
                    logger.warning("edge-tts synth failed: %s", e)
                    return False
            # PyAV mp3->wav conversion is sync but fast (~10ms) — run in
            # the default executor so we don't block the event loop.
            loop = asyncio.get_event_loop()
            try:
                await loop.run_in_executor(
                    None, mp3_to_wav, tmp_mp3, out_wav,
                    self.sample_rate, True,
                )
                return True
            except Exception as e:
                logger.warning("PyAV mp3->wav failed: %s", e)
                return False
            finally:
                if os.path.exists(tmp_mp3):
                    os.remove(tmp_mp3)

        async def _tracked(out_wav: str, text: str) -> bool:
            nonlocal completed
            try:
                return await _one(out_wav, text)
            finally:
                completed += 1
                if progress_callback:
                    progress_callback(completed, total)

        async def _run_all():
            tasks = [_tracked(w, t) for w, t in items]
            done = await asyncio.gather(*tasks)
            for (out_wav, _), ok in zip(items, done):
                results[out_wav] = ok

        # Run the coroutine. asyncio.run() creates and tears down its own
        # event loop, which is what we want for a one-shot batch.
        try:
            asyncio.run(_run_all())
        except RuntimeError:
            # If we're already inside an event loop (e.g. notebook), fall
            # back to the synchronous per-item path.
            results = {}
            for i, (w, t) in enumerate(items, 1):
                results[w] = self.synth(t, w)
                if progress_callback:
                    progress_callback(i, total)
        return results

    def synth_many_with_rates(self, items, max_concurrency: int = 8,
                               progress_callback=None) -> dict:
        """Like ``synth_many`` but each item can override the speaking rate.

        This is the key to avoiding post-processing artifacts: instead of
        compressing TTS audio that overflows its slot (phase vocoder → metallic,
        truncation → lost words), we re-synthesize the overflowing sentences
        at a faster edge-tts rate. edge-tts handles speed natively — no
        artifacts.

        Args:
            items: iterable of ``(out_wav_path, text, rate)`` tuples where
                ``rate`` is an edge-tts rate string (e.g. ``"+50%"`` for 1.5x,
                ``"+0%"`` for normal). Pass ``None`` to use the default rate.
            max_concurrency: max simultaneous in-flight requests.
            progress_callback: optional callable(completed: int, total: int)
                called after each item finishes (success or fail).

        Returns:
            dict mapping ``out_wav_path -> bool`` (True on success).
        """
        import asyncio
        from .av_utils import mp3_to_wav

        items = list(items)
        total = len(items)
        sem = asyncio.Semaphore(max_concurrency)
        results = {}
        completed = 0

        async def _one(out_wav: str, text: str, rate) -> bool:
            tmp_mp3 = out_wav + ".tmp.mp3"
            async with sem:
                try:
                    comm = self._edge_tts.Communicate(
                        text,
                        voice=self._voice_name,
                        proxy=self.voice.proxy,
                        rate=rate or self.voice.edge_rate,
                        volume=self.voice.edge_volume,
                    )
                    await comm.save(tmp_mp3)
                except Exception as e:
                    logger.warning("edge-tts synth failed: %s", e)
                    return False
            loop = asyncio.get_event_loop()
            try:
                await loop.run_in_executor(
                    None, mp3_to_wav, tmp_mp3, out_wav,
                    self.sample_rate, True,
                )
                return True
            except Exception as e:
                logger.warning("PyAV mp3->wav failed: %s", e)
                return False
            finally:
                if os.path.exists(tmp_mp3):
                    os.remove(tmp_mp3)

        async def _tracked(out_wav: str, text: str, rate) -> bool:
            nonlocal completed
            try:
                return await _one(out_wav, text, rate)
            finally:
                completed += 1
                if progress_callback:
                    progress_callback(completed, total)

        async def _run_all():
            tasks = [_tracked(w, t, r) for w, t, r in items]
            done = await asyncio.gather(*tasks)
            for (out_wav, _, _), ok in zip(items, done):
                results[out_wav] = ok

        try:
            asyncio.run(_run_all())
        except RuntimeError:
            results = {}
            for i, (w, t, r) in enumerate(items, 1):
                # Temporarily override edge_rate for sequential fallback.
                old_rate = self.voice.edge_rate
                if r is not None:
                    self.voice.edge_rate = r
                results[w] = self.synth(t, w)
                self.voice.edge_rate = old_rate
                if progress_callback:
                    progress_callback(i, total)
        return results


def build_backend(voice: VoiceConfig) -> TTSBackend:
    """Factory: instantiate the TTS backend selected by ``voice.backend``."""
    backend = (voice.backend or "").lower()
    if backend == "edge_tts":
        return EdgeTTSBackend(voice)
    if backend in ("chattts", "chat_tts", "chat-tts"):
        return ChatTTSBackend(voice)
    raise ValueError(
        f"Unknown TTS backend: {voice.backend!r} "
        "(expected 'edge_tts' or 'chattts')"
    )
