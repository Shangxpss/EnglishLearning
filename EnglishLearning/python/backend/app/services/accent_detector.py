"""Accent / language detection (orchestrator helper).

Decides which dubbing path to use for a given source video:

    * Path A (Voice Conversion)   — best when source is English-but-accented.
    * Path B (TTS + cloning)      — fallback for non-English / unusable audio.

The decision is based on the **detected language** of the source audio
(faster-whisper). If Whisper is confident the audio is English (even heavily
accented), we prefer Path A because VC preserves the speaker's prosody. If
the audio is non-English, we must transcribe → translate → synthesise (Path B).

This module is intentionally lightweight: it wraps Whisper's language-
detection (which the existing ``AudioProcessor`` already runs) and exposes a
clean ``choose_path()`` decision the orchestrator can call.

Reference: see ``docs/NATURAL_ACCENT_DUBBING_DESIGN.md`` §4.1 / §5.4.
"""

from __future__ import annotations

import os
import logging
from dataclasses import dataclass
from typing import Optional

logger = logging.getLogger(__name__)


@dataclass
class LanguageDetection:
    """Result of detecting the source language."""
    language: str            # ISO-639-1 code, e.g. "en", "ja", "zh"
    confidence: float        # 0.0–1.0
    is_english: bool         # convenience: language == "en"
    is_accented: bool = False  # heuristic: English with low confidence → likely accented
    error: str = ""


@dataclass
class PathDecision:
    """The orchestrator's routing decision."""
    path: str                # "A" (voice conversion) | "B" (TTS) | "C" (legacy edge-tts)
    reason: str
    language: str
    confidence: float


# Whisper language code → human name (for logging / UI).
_LANGUAGE_NAMES = {
    "en": "English", "ja": "Japanese", "zh": "Chinese", "es": "Spanish",
    "fr": "French", "de": "German", "ko": "Korean", "it": "Italian",
    "pt": "Portuguese", "ru": "Russian", "ar": "Arabic", "hi": "Hindi",
    "nl": "Dutch", "tr": "Turkish", "pl": "Polish", "cs": "Czech",
}


class AccentDetector:
    """Detects the source language and recommends a dubbing path.

    Usage::

        detector = AccentDetector()
        detection = detector.detect_language("/tmp/source.wav")
        decision = detector.choose_path(detection, prefer_vc=True)
        if decision.path == "A":
            # voice conversion
        else:
            # TTS + clone
    """

    def detect_language(self, audio_path: str) -> LanguageDetection:
        """Run faster-whisper language detection on ``audio_path``.

        Reuses the same Whisper infrastructure as ``AudioProcessor`` so there
        is no extra model download — we just call the lower-level
        ``detect_language`` API.
        """
        if not os.path.exists(audio_path):
            return LanguageDetection(language="en", confidence=0.0,
                                     is_english=True,
                                     error=f"Audio not found: {audio_path}")

        try:
            from faster_whisper import WhisperModel
        except Exception as exc:
            logger.warning("faster-whisper not available (%s); assuming English", exc)
            return LanguageDetection(language="en", confidence=0.0,
                                     is_english=True,
                                     error=str(exc))

        try:
            # Use the small model for detection — fast and good enough for
            # language ID. Cached by AudioProcessor's model dict in practice.
            model = WhisperModel("base", device="cpu", compute_type="int8")
            segments, info = model.transcribe(audio_path, beam_size=1)
            # Consume the generator so detection actually runs.
            _ = list(segments)
            lang = info.language
            conf = float(info.language_probability)
            is_en = lang == "en"
            # Heuristic: English with <0.7 confidence is likely heavily accented
            # or noisy — still Path A material, but flag it.
            accented = is_en and conf < 0.70
            name = _LANGUAGE_NAMES.get(lang, lang)
            logger.info("Detected language: %s (%s) confidence=%.2f accented=%s",
                        lang, name, conf, accented)
            return LanguageDetection(
                language=lang, confidence=conf,
                is_english=is_en, is_accented=accented,
            )
        except Exception as exc:
            logger.exception("Language detection failed")
            return LanguageDetection(language="en", confidence=0.0,
                                     is_english=True, error=str(exc))

    def choose_path(self,
                    detection: LanguageDetection,
                    prefer_vc: bool = True,
                    force_path: Optional[str] = None) -> PathDecision:
        """Recommend "A" (VC) or "B" (TTS) based on the detection.

        Parameters
        ----------
        detection
            Output of :meth:`detect_language`.
        prefer_vc
            If True (default), prefer Path A for English source even when
            accented. Set False to always use Path B.
        force_path
            If set to "A" or "B", skip the heuristic and use the given path.
            Useful for explicit API overrides.
        """
        if force_path in ("A", "B", "C"):
            return PathDecision(
                path=force_path,
                reason="explicitly forced by caller",
                language=detection.language,
                confidence=detection.confidence,
            )

        if detection.is_english and prefer_vc:
            return PathDecision(
                path="A",
                reason=("English source detected (accented=%s); voice "
                        "conversion preserves prosody without time-stretch."
                        % detection.is_accented),
                language=detection.language,
                confidence=detection.confidence,
            )

        if detection.is_english and not prefer_vc:
            return PathDecision(
                path="B",
                reason="English source but caller prefers TTS path",
                language=detection.language,
                confidence=detection.confidence,
            )

        # Non-English → must transcribe + (optionally translate) + synthesise.
        return PathDecision(
            path="B",
            reason=("Non-English source (%s); transcription + TTS required"
                    % detection.language),
            language=detection.language,
            confidence=detection.confidence,
        )


# Convenience singleton.
accent_detector = AccentDetector()
