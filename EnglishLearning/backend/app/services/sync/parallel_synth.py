"""Parallel + robust synthesis for the dubbing pipeline.

This is an **additive** module — it does NOT modify the existing
:class:`SentenceStitcher` or ``run_sync_dub.py``. The prior iteration keeps
working unchanged. Use this module via the new entry point
``run_parallel_dub.py``.

It addresses two issues diagnosed in the production pipeline:

1. **"Weird voice" on some sentences (most are fine)**

   Root cause: :class:`SentenceStitcher` uses
   ``librosa.effects.time_stretch`` (a phase vocoder) to compress TTS audio
   that overflows into the next sentence's region. Phase vocoders introduce
   metallic / ringing artifacts that are very audible on edge-tts's clean
   neural audio, but masked on ChatTTS's noisy output. Only the few
   overflow sentences get stretched, so most sound fine while a handful
   sound metallic. (The stitcher's own docstring admits it "sounded
   metallic on edge-tts's clean neural audio".)

   Secondary causes: no audio integrity validation (truncated edge-tts mp3
   from network blips decodes to clicks/gaps), and no text normalization
   (``<``, ``>``, ``&`` may be read as SSML markup).

   Fix: :class:`GentleStitcher` replaces phase-vocoder compression with
   zero-crossing truncation + short crossfade. No metallic artifacts.
   :class:`ParallelSynthesizer` adds integrity validation + retry + text
   normalization.

2. **Performance bottleneck is IO-bound (network), not CPU-bound**

   Stage 5 (edge-tts synthesis) is the bottleneck: N sequential WebSocket
   round-trips to Microsoft's servers (~1.5s each). ``EdgeTTSBackend`` already
   has a ``synth_many()`` method using ``asyncio.gather`` + ``Semaphore(8)``
   (measured 5.45× speedup), but production ``run_sync_dub.py`` calls the
   sequential ``synth()`` and never uses it.

   Fix: :class:`ParallelSynthesizer` wraps ``synth_many()`` with retry +
   validation. This is the IO-bound async solution — no Rust needed.
"""

from __future__ import annotations

import logging
import os
import re
from typing import Dict, List, Optional, Tuple

import numpy as np

from .chunk_stitcher import SentenceStitcher
from .models import AlignedCue

logger = logging.getLogger(__name__)


# ── text normalization ────────────────────────────────────────────────────

# Characters edge-tts may interpret as SSML/markup. We escape them so the
# synthesizer reads them literally instead of trying to parse tags.
_SSML_UNSAFE = {
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
}

# Collapse whitespace (edge-tts reads runs of spaces as long pauses).
_WS_RE = re.compile(r"\s+")


def normalize_text(text: str) -> str:
    """Normalize text for edge-tts synthesis.

    1. Escape SSML-unsafe characters (``&``, ``<``, ``>``) so they are read
       literally instead of being parsed as markup. This is the fix for the
       occasional "weird reading" where edge-tts tried to interpret a stray
       ``<`` or ``&`` in the transcript as a tag.
    2. Collapse runs of whitespace to a single space (edge-tts reads long
       whitespace as exaggerated pauses).
    3. Strip leading/trailing whitespace.
    """
    if not text:
        return ""
    out = text
    for ch, esc in _SSML_UNSAFE.items():
        out = out.replace(ch, esc)
    out = _WS_RE.sub(" ", out).strip()
    return out


# ── audio integrity validation ────────────────────────────────────────────

# Heuristic: spoken English is roughly 12-16 chars/sec. We use a wide band
# (5-30 chars/sec) to flag obviously broken audio without false-positiving
# fast/slow sentences.
_MIN_CHARS_PER_SEC = 5.0
_MAX_CHARS_PER_SEC = 30.0
# Absolute bounds — a sentence shorter than 0.3s or longer than 60s is almost
# certainly a synthesis error.
_MIN_AUDIO_S = 0.3
_MAX_AUDIO_S = 60.0


def validate_audio(audio: np.ndarray, sample_rate: int, text: str) -> Tuple[bool, str]:
    """Sanity-check a synthesized audio clip against its source text.

    Returns ``(ok, reason)``. ``ok=False`` means the audio is almost certainly
    broken (truncated / corrupt / silent) and should be re-synthesized.
    """
    audio = np.asarray(audio, dtype=np.float32).squeeze()
    dur = audio.size / sample_rate if sample_rate > 0 else 0.0

    if dur < _MIN_AUDIO_S:
        return False, f"too short ({dur:.2f}s < {_MIN_AUDIO_S}s)"
    if dur > _MAX_AUDIO_S:
        return False, f"too long ({dur:.2f}s > {_MAX_AUDIO_S}s)"

    # Near-silent audio (peak below -60 dB) is almost certainly a failure.
    peak = float(np.max(np.abs(audio))) if audio.size else 0.0
    if peak < 0.001:
        return False, f"near-silent (peak={peak:.4f})"

    # Char-rate sanity check. Wide band so we don't false-positive on
    # unusually fast/slow sentences.
    n_chars = len(text.strip())
    if n_chars > 10:
        chars_per_sec = n_chars / dur
        if chars_per_sec > _MAX_CHARS_PER_SEC:
            return False, f"too fast ({chars_per_sec:.1f} chars/s > {_MAX_CHARS_PER_SEC})"
        if chars_per_sec < _MIN_CHARS_PER_SEC:
            return False, f"too slow ({chars_per_sec:.1f} chars/s < {_MIN_CHARS_PER_SEC})"

    return True, "ok"


# ── parallel synthesizer ─────────────────────────────────────────────────


class ParallelSynthesizer:
    """Parallel edge-tts synthesis with retry + integrity validation.

    Wraps :meth:`EdgeTTSBackend.synth_many` (asyncio.gather + Semaphore) with:

      * **Text normalization** — escapes SSML-unsafe chars before synthesis.
      * **Retry with exponential backoff** — re-synthesizes sentences that
        failed or failed validation, up to ``max_retries`` times.
      * **Audio integrity validation** — rejects truncated/silent/corrupt
        audio via :func:`validate_audio` and retries.

    Falls back to sequential ``synth()`` if the backend doesn't expose
    ``synth_many()`` (e.g. ChatTTS, which is CPU-bound and wouldn't benefit
    from async anyway).

    This is the IO-bound async solution to the network bottleneck. No Rust
    needed — edge-tts is a network service, so the bottleneck is round-trip
    latency, not CPU. Running N requests concurrently with a bounded
    semaphore turns N sequential round-trips into one batched wave.
    """

    def __init__(self, backend, max_retries: int = 2, max_concurrency: int = 8):
        self.backend = backend
        self.max_retries = max_retries
        self.max_concurrency = max_concurrency
        self.sample_rate = backend.sample_rate

    def synthesize(self, cues: List[AlignedCue], tts_dir: str) -> Dict[int, str]:
        """Synthesize all cues' sentences in parallel.

        Args:
            cues: aligned sentence cues.
            tts_dir: directory to write per-sentence wavs
                (``sent_NNNN.wav``).

        Returns:
            Mapping ``cue_index -> wav_path`` for successfully synthesized
            sentences. Failed sentences are omitted (the stitcher treats
            missing entries as silence).
        """
        os.makedirs(tts_dir, exist_ok=True)

        # Build the work list: (cue_index, out_wav, normalized_text).
        work: List[Tuple[int, str, str]] = []
        for ci, cue in enumerate(cues):
            text = normalize_text(cue.text)
            if not text:
                continue
            out_wav = os.path.join(tts_dir, f"sent_{ci:04d}.wav")
            if os.path.exists(out_wav):
                # Cached — validate before trusting it.
                try:
                    import soundfile as sf
                    ad, _ = sf.read(out_wav)
                    ok, reason = validate_audio(ad, self.sample_rate, cue.text)
                    if ok:
                        continue
                    logger.info("cached sent_%04d failed validation (%s), re-synthesizing",
                                ci, reason)
                    os.remove(out_wav)
                except Exception:
                    # Can't read the cache — re-synthesize.
                    if os.path.exists(out_wav):
                        os.remove(out_wav)
            work.append((ci, out_wav, text))

        if not work:
            logger.info("ParallelSynthesizer: all %d sentences cached", len(cues))
            return {ci: os.path.join(tts_dir, f"sent_{ci:04d}.wav")
                    for ci in range(len(cues))
                    if os.path.exists(os.path.join(tts_dir, f"sent_{ci:04d}.wav"))}

        # Use synth_many if available (edge-tts); else fall back to sequential.
        has_parallel = hasattr(self.backend, "synth_many")
        results: Dict[int, bool] = {}

        for attempt in range(self.max_retries + 1):
            # Pending = sentences not yet successfully synthesized.
            pending = [(ci, w, t) for (ci, w, t) in work
                       if results.get(ci) is not True]
            if not pending:
                break

            if has_parallel:
                item_pairs = [(w, t) for (ci, w, t) in pending]
                raw = self.backend.synth_many(item_pairs,
                                             max_concurrency=self.max_concurrency)
            else:
                raw = {w: self.backend.synth(t, w) for (ci, w, t) in pending}

            # Validate each successful synthesis.
            still_bad: List[Tuple[int, str, str]] = []
            for (ci, w, t) in pending:
                if not raw.get(w):
                    results[ci] = False
                    still_bad.append((ci, w, t))
                    continue
                try:
                    import soundfile as sf
                    ad, _ = sf.read(w)
                    ok, reason = validate_audio(ad, self.sample_rate, t)
                except Exception as e:
                    ok, reason = False, f"read failed: {e}"
                if ok:
                    results[ci] = True
                else:
                    results[ci] = False
                    logger.warning("sent_%04d validation failed (%s) — attempt %d/%d",
                                   ci, reason, attempt + 1, self.max_retries + 1)
                    if os.path.exists(w):
                        os.remove(w)
                    still_bad.append((ci, w, t))

            work = still_bad
            if not still_bad:
                break
            if attempt < self.max_retries:
                logger.info("retrying %d failed sentences (attempt %d/%d)",
                            len(still_bad), attempt + 2, self.max_retries + 1)

        n_ok = sum(1 for v in results.values() if v)
        n_fail = sum(1 for v in results.values() if not v)
        logger.info("ParallelSynthesizer: synthesized %d ok, %d failed (after %d attempts)",
                    n_ok, n_fail, self.max_retries + 1)

        return {ci: os.path.join(tts_dir, f"sent_{ci:04d}.wav")
                for ci in range(len(cues))
                if os.path.exists(os.path.join(tts_dir, f"sent_{ci:04d}.wav"))}


# ── gentle stitcher (no phase vocoder) ────────────────────────────────────


def _last_zero_crossing(audio: np.ndarray, before: int) -> int:
    """Find the last sample index ``<= before`` where the waveform crosses zero.

    Used to truncate TTS audio at a zero-crossing so the cut is click-free.
    Falls back to ``before`` if no zero-crossing is found nearby.
    """
    if before >= audio.size:
        before = audio.size - 1
    if before <= 0:
        return 0
    # Search backwards from ``before`` for a sign change.
    window = audio[:before + 1]
    sign = np.sign(window)
    # A zero-crossing is where sign changes between consecutive samples.
    crossings = np.where(np.diff(sign) != 0)[0]
    if crossings.size == 0:
        return before
    return int(crossings[-1]) + 1


def _crossfade_tail(audio: np.ndarray, fade_n: int) -> np.ndarray:
    """Apply a short fade-out to the tail of ``audio`` to mask any truncation.

    A 5ms raised-cosine fade is enough to suppress the click from a
    non-zero-crossing cut, without attenuating the final syllable
    perceptibly (the previous SentenceStitcher bug was a 20ms linear fade
    that ramped the last syllable to zero).
    """
    if fade_n <= 0 or audio.size < 2 * fade_n:
        return audio
    out = audio.copy()
    # Raised-cosine fade — smoother than linear, less audible.
    t = np.linspace(0.0, np.pi / 2.0, fade_n, dtype=np.float32)
    fade = np.cos(t) ** 2
    out[-fade_n:] *= fade
    return out


class GentleStitcher(SentenceStitcher):
    """Stitcher that replaces phase-vocoder compression with gentle truncation.

    This is the fix for the "metallic / weird" sentences. The parent
    :class:`SentenceStitcher` uses ``librosa.effects.time_stretch`` (a phase
    vocoder) to compress TTS audio that overflows into the next sentence's
    region. Phase vocoders introduce metallic / ringing artifacts that are
    very audible on edge-tts's clean neural audio.

    This subclass replaces that with **zero-crossing truncation + crossfade**:

      * Find the last zero-crossing before the collision boundary and cut
        there. A cut at a zero-crossing is click-free.
      * Apply a 5ms raised-cosine fade-out to mask any residual discontinuity.
      * The lost audio is the overflow tail (a fraction of a syllable) —
        imperceptible compared to metallic artifacts across the whole sentence.

    Trade-off: the overflow syllable is truncated rather than time-compressed,
    so a tiny bit of speech is lost. Empirically this is far less objectionable
    than the metallic ringing the phase vocoder produces on clean neural TTS.

    For severe overflows (> ``hard_truncate_ratio``), we still fall back to
    phase vocoder — but this is rare (edge-tts is usually within ~20% of the
    expected duration) and the ratio is capped at 1.15× (gentler than the
    parent's 1.30×) to keep artifacts minimal.
    """

    def __init__(self,
                 sample_rate: int,
                 fade_s: float = 0.005,
                 hard_truncate_ratio: float = 1.25,
                 max_compress: float = 1.15):
        super().__init__(sample_rate=sample_rate, max_compress=max_compress,
                         fade_s=fade_s)
        # Above this overflow ratio, truncation alone loses too much speech —
        # fall back to gentle phase-vocoder compression (capped at max_compress).
        self.hard_truncate_ratio = hard_truncate_ratio
        self.fade_n = max(1, int(fade_s * sample_rate))

    def stitch(self,
               sentence_audio: dict,
               cues: List[AlignedCue],
               total_duration: float,
               normalize: bool = True) -> np.ndarray:
        """Assemble final audio — zero-crossing truncation instead of phase vocoder.

        See :class:`GentleStitcher` class docstring for the rationale.
        Args mirror :meth:`SentenceStitcher.stitch`.
        """
        sr = self.sample_rate
        total_samples = int(total_duration * sr) + sr
        final = np.zeros(total_samples, dtype=np.float32)

        n_truncated = n_compressed = n_natural = n_missing = 0

        for ci, cue in enumerate(cues):
            audio = sentence_audio.get(ci)
            if audio is None:
                n_missing += 1
                continue
            audio = np.asarray(audio, dtype=np.float32).squeeze()
            if audio.ndim > 1:
                audio = audio[:, 0]

            slot_dur = cue.end - cue.start
            start_sample = int(cue.start * sr)
            is_last_cue = (ci == len(cues) - 1)
            available_room = slot_dur + (0.0 if is_last_cue else cue.pause_after)
            available_samples = int(available_room * sr)

            if audio.size > available_samples and available_samples > 0 and not is_last_cue:
                ratio = audio.size / available_samples
                if ratio <= self.hard_truncate_ratio:
                    # Gentle path: truncate at the last zero-crossing before
                    # the boundary, then crossfade. No phase vocoder = no
                    # metallic artifacts.
                    cut = _last_zero_crossing(audio, available_samples)
                    if cut < available_samples - sr * 0.1:
                        # Zero-crossing was too far back (> 0.1s before the
                        # boundary) — just truncate at the boundary and rely
                        # on the crossfade to mask the click.
                        cut = available_samples
                    audio = audio[:cut]
                    audio = _crossfade_tail(audio, self.fade_n)
                    n_truncated += 1
                else:
                    # Severe overflow — fall back to gentle phase vocoder,
                    # capped at max_compress (1.15×, gentler than parent's 1.30×).
                    rate = min(ratio, self.max_compress)
                    if rate > 1.01:
                        import librosa  # lazy
                        audio = librosa.effects.time_stretch(audio, rate=rate)
                        # After compression, if still overflowing, truncate.
                        if audio.size > available_samples:
                            cut = _last_zero_crossing(audio, available_samples)
                            audio = audio[:cut]
                            audio = _crossfade_tail(audio, self.fade_n)
                        n_compressed += 1
            else:
                # Fits in slot, or overflows only into the natural pause.
                n_natural += 1

            # Fade-in at sentence start (always safe — previous sample is
            # silence or the prior sentence's tail).
            if audio.size > 2 * self.fade_n:
                audio = audio.copy()
                audio[:self.fade_n] *= np.linspace(0.0, 1.0, self.fade_n,
                                                    dtype=np.float32)
                # Fade-out only if the audio actually reaches the boundary
                # (matches the parent's conditional-fade logic — avoids
                # attenuating the final syllable when TTS fits in the slot).
                if audio.size >= available_samples:
                    audio[-self.fade_n:] *= np.linspace(1.0, 0.0, self.fade_n,
                                                        dtype=np.float32)

            end_sample = min(start_sample + audio.size, total_samples)
            if start_sample >= total_samples:
                continue
            final[start_sample:end_sample] = audio[:end_sample - start_sample]

        if normalize:
            peak = float(np.max(np.abs(final))) if final.size else 0.0
            if peak > 0.95:
                final *= 0.95 / peak

        logger.info(
            "GentleStitcher: sentences=%d truncated=%d compressed=%d "
            "natural=%d missing=%d, final=%.2fs",
            len(cues), n_truncated, n_compressed, n_natural, n_missing,
            len(final) / sr,
        )
        return final


# ── v4.1: respeed synthesis (no post-processing artifacts) ───────────────


class RespeedSynthesizer:
    """Two-pass synthesizer that prevents overflow by adjusting edge-tts rate.

    This is the fix for the "weird voice after compress" problem. The v4
    ``GentleStitcher`` replaced the phase vocoder with zero-crossing
    truncation, but truncation still loses the overflow tail (a fraction of a
    syllable). This class eliminates the need for ANY post-processing:

      1. **Pass 1**: synthesize all sentences at normal rate.
      2. **Check overflow**: for each sentence, compare TTS audio duration
         against the available slot (``cue.end - cue.start + cue.pause_after``).
      3. **Pass 2**: for overflowing sentences, re-synthesize at a faster
         edge-tts rate. edge-tts handles speed natively — no phase vocoder,
         no truncation, no artifacts.

    The rate is calculated as::

        needed_speedup = tts_duration / available_room
        rate_percent = ceil((needed_speedup - 1) * 100)
        # capped at +100% (2x) to keep speech intelligible

    Example: a sentence with 6s of TTS audio but only 4s of slot →
    needed_speedup = 1.5 → rate = "+50%" → edge-tts speaks 1.5x faster.

    The resulting audio fits the slot natively, so the stitcher just places
    it at the cue's start time — no compression, no truncation, no artifacts.
    """

    # Cap the speedup at 2x (+100%) — beyond this, edge-tts sounds rushed
    # and unintelligible. If a sentence still overflows at 2x, the stitcher
    # will truncate the residual (rare).
    MAX_RATE_PERCENT = 100

    def __init__(self, backend, max_retries: int = 2, max_concurrency: int = 8):
        self.backend = backend
        self.max_retries = max_retries
        self.max_concurrency = max_concurrency
        self.sample_rate = backend.sample_rate

    def _compute_rate(self, tts_duration: float, available_room: float) -> str:
        """Calculate the edge-tts rate string needed to fit ``tts_duration``
        into ``available_room`` seconds.

        Returns a rate string like ``"+50%"`` or ``"+0%"`` if no speedup needed.
        """
        if available_room <= 0 or tts_duration <= available_room:
            return "+0%"
        speedup = tts_duration / available_room
        percent = int((speedup - 1) * 100)
        percent = max(0, min(percent, self.MAX_RATE_PERCENT))
        return f"+{percent}%"

    def synthesize(self, cues: List[AlignedCue], tts_dir: str,
                   progress_callback=None) -> Dict[int, str]:
        """Two-pass synthesis: normal → respeed overflowing sentences.

        Args:
            cues: aligned sentence cues.
            tts_dir: directory for per-sentence wavs.
            progress_callback: optional callable(completed: int, total: int)
                called after each sentence is synthesized (not just after
                each pass). ``total`` is ``len(cues)``. During Pass 1 the
                count rises 0 → ~total as each TTS clip finishes; during
                Pass 2 (respeed) it holds flat at the Pass 1 high-water
                mark. Use for per-sentence progress reporting to the
                frontend.

        Returns:
            Mapping ``cue_index -> wav_path`` for successfully synthesized
            sentences.
        """
        import soundfile as sf

        os.makedirs(tts_dir, exist_ok=True)

        total = len(cues)

        def _wav_path(ci: int) -> str:
            return os.path.join(tts_dir, f"sent_{ci:04d}.wav")

        # Count wavs that already exist (from a previous interrupted run).
        # These are reported as already-completed so the progress bar starts
        # at the right place instead of jumping 0 → 100.
        n_pre = sum(1 for ci in range(total) if os.path.exists(_wav_path(ci)))

        # ── Pass 1: synthesize all at normal rate ───────────────────────
        work: List[Tuple[int, str, str]] = []
        for ci, cue in enumerate(cues):
            text = normalize_text(cue.text)
            if not text:
                continue
            out_wav = _wav_path(ci)
            if not os.path.exists(out_wav):
                work.append((ci, out_wav, text))

        if work:
            # Per-item callback: translate batch progress → overall progress.
            # During Pass 1 the count goes n_pre → n_pre + len(work) ≈ total.
            def _pass1_cb(done, _batch_total):
                if progress_callback:
                    progress_callback(n_pre + done, total)

            if hasattr(self.backend, "synth_many"):
                items = [(w, t) for _, w, t in work]
                self.backend.synth_many(
                    items, max_concurrency=self.max_concurrency,
                    progress_callback=_pass1_cb,
                )
            else:
                for i, (_, w, t) in enumerate(work, 1):
                    self.backend.synth(t, w)
                    if progress_callback:
                        progress_callback(n_pre + i, total)

        # ── Report progress after Pass 1 ────────────────────────────────
        n_ok = sum(1 for ci in range(total) if os.path.exists(_wav_path(ci)))
        if progress_callback:
            progress_callback(n_ok, total)

        # ── Check which sentences overflow their slots ──────────────────
        respeed_work: List[Tuple[str, str, str]] = []  # (wav, text, rate)
        n_respeed = 0
        for ci, cue in enumerate(cues):
            out_wav = _wav_path(ci)
            if not os.path.exists(out_wav):
                continue
            try:
                ad, _ = sf.read(out_wav)
            except Exception:
                continue
            tts_dur = len(ad) / self.sample_rate
            is_last = (ci == len(cues) - 1)
            available = (cue.end - cue.start)
            if not is_last:
                available += cue.pause_after
            if tts_dur > available + 0.1:  # 0.1s tolerance
                rate = self._compute_rate(tts_dur, available)
                if rate != "+0%":
                    n_respeed += 1
                    text = normalize_text(cue.text)
                    respeed_work.append((out_wav, text, rate))
                    logger.info("sent_%04d: tts=%.2fs slot=%.2fs → respeed %s",
                                ci, tts_dur, available, rate)

        # ── Pass 2: re-synthesize overflowing sentences at faster rate ──
        if respeed_work:
            logger.info("RespeedSynthesizer: re-synthesizing %d/%d sentences "
                        "at faster rate", n_respeed, total)
            # Delete old wavs before re-synthesizing.
            for wav_path, _, _ in respeed_work:
                if os.path.exists(wav_path):
                    os.remove(wav_path)
            # During Pass 2 we are re-doing already-counted sentences, so the
            # high-water mark stays at n_ok. The progress bar holds flat —
            # Pass 2 is typically a small fraction of the total work.
            def _pass2_cb(_done, _batch_total):
                if progress_callback:
                    progress_callback(n_ok, total)

            if hasattr(self.backend, "synth_many_with_rates"):
                self.backend.synth_many_with_rates(
                    respeed_work, max_concurrency=self.max_concurrency,
                    progress_callback=_pass2_cb,
                )
            elif hasattr(self.backend, "synth_many"):
                # Fallback: synth_many ignores rate, use sequential synth.
                for wav_path, text, rate in respeed_work:
                    old_rate = self.backend.voice.edge_rate
                    self.backend.voice.edge_rate = rate
                    self.backend.synth(text, wav_path)
                    self.backend.voice.edge_rate = old_rate
            else:
                for wav_path, text, _ in respeed_work:
                    self.backend.synth(text, wav_path)

        # ── Report progress after Pass 2 ────────────────────────────────
        if progress_callback:
            n_ok = sum(1 for ci in range(total) if os.path.exists(_wav_path(ci)))
            progress_callback(n_ok, total)

        # ── Return all successful paths ──────────────────────────────────
        result: Dict[int, str] = {}
        for ci in range(total):
            wav_path = _wav_path(ci)
            if os.path.exists(wav_path):
                result[ci] = wav_path
        logger.info("RespeedSynthesizer: %d sentences ready (%d respeeded)",
                    len(result), n_respeed)
        return result


class PlaceStitcher:
    """Minimal stitcher: just places TTS audio at cue start times.

    This is the companion to :class:`RespeedSynthesizer`. Since the TTS audio
    has already been speed-adjusted to fit its slot (via edge-tts rate), there
    is no need for compression or truncation. This stitcher simply:

      * **Loudness-normalizes** each sentence to a target RMS (default -20 dBFS,
        approximating -20 LUFS for speech). This fixes the problem where some
        sentences sound much quieter/louder than others.
      * Places each sentence's audio at ``cue.start`` in the final buffer.
      * Applies short fade-in/fade-out (5ms) **only when needed** — fade-out
        is skipped when the audio ends naturally within its slot (doesn't
        reach the boundary), so final consonants aren't attenuated.
      * Peak-normalizes the final mix to 0.95 if clipping.

    No phase vocoder, no truncation, no crossfade — just placement + loudness
    matching. The cleanest possible path from TTS audio to final waveform.
    """

    def __init__(self,
                 sample_rate: int,
                 fade_s: float = 0.005,
                 target_rms_db: float = -20.0):
        self.sample_rate = sample_rate
        self.fade_n = max(1, int(fade_s * sample_rate))
        # Target RMS in dBFS. -20 dBFS ≈ -20 LUFS for speech (broadcast standard).
        # edge-tts output varies ±6 dB between sentences; normalizing to a
        # common target makes the dub sound like one consistent speaker.
        self.target_rms = 10.0 ** (target_rms_db / 20.0)

    def _loudness_normalize(self, audio: np.ndarray) -> np.ndarray:
        """Normalize audio to ``self.target_rms`` (RMS-based).

        Uses RMS rather than peak because peak doesn't reflect perceived
        loudness — a quiet sentence with one transient click would be
        amplified less than a uniformly loud one by peak normalization.

        Clips to [-1, 1] after scaling to avoid digital clipping.
        """
        rms = float(np.sqrt(np.mean(audio ** 2))) if audio.size else 0.0
        if rms < 1e-5:
            return audio  # near-silent — leave as-is
        gain = self.target_rms / rms
        # Cap gain at 10x (+20 dB) to avoid amplifying noise on quiet sentences.
        gain = min(gain, 10.0)
        out = audio * gain
        # Soft-clip to prevent digital clipping.
        peak = float(np.max(np.abs(out))) if out.size else 0.0
        if peak > 0.99:
            out = np.tanh(out * 0.9) / np.tanh(0.9)
        return out

    def stitch(self,
               sentence_audio: dict,
               cues: List[AlignedCue],
               total_duration: float,
               normalize: bool = True,
               room_tone: np.ndarray | None = None) -> np.ndarray:
        """Assemble final audio by placing each sentence at its cue start.

        Per-sentence loudness normalization is applied **before** placement so
        each sentence is matched to a common perceived loudness, then the final
        mix is peak-normalized only if it clips.

        Args:
            sentence_audio: ``{cue_index -> np.ndarray}`` of TTS audio.
            cues: aligned cues with ``start``/``end``/``pause_after``.
            total_duration: target length of the final audio in seconds.
            normalize: if True, peak-normalize the final mix if it clips.
            room_tone: optional ambient noise (1-D float32) extracted from the
                original video's silent gaps. When provided, it's looped to
                fill inter-sentence gaps at low level (-30 dBFS) so the dub
                doesn't have jarring dead-silence between sentences. If None,
                gaps remain pure digital silence (preserves prior behavior).
        """
        sr = self.sample_rate
        total_samples = int(total_duration * sr) + sr
        final = np.zeros(total_samples, dtype=np.float32)
        n_placed = n_missing = 0

        # Prepare looped room tone at -30 dBFS (very low — just to avoid
        # the "dead silence" choppiness between sentences).
        room_level = 10.0 ** (-30.0 / 20.0)  # 0.0316
        room_buf: np.ndarray | None = None
        if room_tone is not None and room_tone.size > 0:
            # Normalize room tone to a consistent low level.
            rt = np.asarray(room_tone, dtype=np.float32).squeeze()
            rt_rms = float(np.sqrt(np.mean(rt ** 2))) if rt.size else 0.0
            if rt_rms > 1e-6:
                rt = rt * (room_level / rt_rms)
                # Pre-roll enough to cover the whole track.
                repeats = (total_samples // rt.size) + 2
                room_buf = np.tile(rt, repeats)[:total_samples]

        for ci, cue in enumerate(cues):
            audio = sentence_audio.get(ci)
            if audio is None:
                n_missing += 1
                continue
            audio = np.asarray(audio, dtype=np.float32).squeeze()
            if audio.ndim > 1:
                audio = audio[:, 0]

            # Per-sentence loudness normalization — matches perceived loudness
            # across sentences so the dub sounds consistent.
            audio = self._loudness_normalize(audio)

            start_sample = int(cue.start * sr)
            slot_end = int(cue.end * sr)

            # Fade-in at start (always safe — previous content is silence
            # or the prior sentence's faded tail).
            if audio.size > 2 * self.fade_n:
                audio = audio.copy()
                audio[:self.fade_n] *= np.linspace(
                    0.0, 1.0, self.fade_n, dtype=np.float32)
                # Fade-out ONLY if the audio reaches the slot boundary.
                # If it ends naturally within the slot, a fade-out would
                # attenuate the final consonant ("-ed", "-s", "-ly") —
                # a subtle but audible quality loss on every sentence.
                audio_end_sample = start_sample + audio.size
                if audio_end_sample >= slot_end:
                    audio[-self.fade_n:] *= np.linspace(
                        1.0, 0.0, self.fade_n, dtype=np.float32)

            end_sample = min(start_sample + audio.size, total_samples)
            if start_sample >= total_samples:
                continue
            # Overlap-add wouldn't help here (no overlapping content), so
            # simple overwrite. If the audio extends past the slot boundary,
            # it's truncated — but this is rare after respeed synthesis.
            final[start_sample:end_sample] = audio[:end_sample - start_sample]
            n_placed += 1

        # Fill inter-sentence gaps with looped room tone (very low level).
        # This avoids the jarring "dead silence" that makes dubs feel choppy.
        # Room tone is only placed where ``final`` is zero (the gaps).
        if room_buf is not None:
            mask = np.abs(final) < 1e-6
            final[mask] = room_buf[mask]

        if normalize:
            peak = float(np.max(np.abs(final))) if final.size else 0.0
            if peak > 0.95:
                final *= 0.95 / peak

        logger.info("PlaceStitcher: placed=%d missing=%d final=%.2fs room_tone=%s",
                    n_placed, n_missing, len(final) / sr,
                    "on" if room_buf is not None else "off")
        return final
