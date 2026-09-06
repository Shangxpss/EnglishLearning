"""Voice configuration for the dubbing pipeline.

Makes the previously-hard-coded speaker parameters (backend, gender, seed,
accent, decoding temperature / top_P / top_K, speed) configurable.

Key research findings (see ``docs/TTS_SYNC_MECHANISM.md`` §8 and §10):

  * **ChatTTS exposes no native accent control.** The speaker embedding
    (``chat.sample_random_speaker()``) controls timbre, pitch and perceived
    gender, but NOT regional accent. The accent you hear comes from the
    training-data mix and the input text's vocabulary/phrasing. ChatTTS's
    London accent is *approximated* by British English vocabulary
    substitution (``color``→``colour``) + a measured RP-style refine prompt.

  * **edge-tts (Microsoft Neural) has native accent support.** Voices like
    ``en-GB-SoniaNeural`` / ``en-GB-LibbyNeural`` / ``en-GB-MaisieNeural``
    are genuine Southern British / RP female speakers. Use the
    ``EdgeTTSBackend`` when real accent control matters.

  * Speaker gender (ChatTTS only) is selected by the random seed passed to
    ``torch.manual_seed()`` before sampling. Empirically tested
    seed→gender mapping (community-verified):

        Male seeds:   2222, 7869, 6653
        Female seeds: 3333, 4099, 5099

  * edge-tts does NOT honor ``http_proxy`` / ``https_proxy`` env vars for
    its WebSocket synthesis endpoint — the proxy must be passed explicitly
    via ``Communicate(proxy=...)``. This is the cause of the previous
    "WebSocket connection timeout" failure.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass, field
from typing import Optional


# Community-verified seed -> gender mapping for ChatTTS.
# Source: https://pyvideotrans.com/chattts-faq (固定发音人音色 section)
MALE_SEEDS = (2222, 7869, 6653)
FEMALE_SEEDS = (3333, 4099, 5099)

# Available Microsoft edge-tts voices with native en-GB (Southern British /
# RP) accent. All verified live on the public Edge Read Aloud endpoint.
# Female voices:
EDGE_EN_GB_FEMALE_VOICES = (
    "en-GB-SoniaNeural",   # friendly, positive — recommended neutral modern Southern British
    "en-GB-LibbyNeural",   # friendly, positive — slightly younger/brighter
    "en-GB-MaisieNeural",  # friendly, positive — youngest of the three
)
# Male voices:
EDGE_EN_GB_MALE_VOICES = (
    "en-GB-RyanNeural",
    "en-GB-ThomasNeural",
)

# British English vocabulary substitutions applied to the source transcript
# before TTS (ChatTTS only — edge-tts voices are already British). These nudge
# ChatTTS toward British pronunciation/lexical stress because the model
# conditions on the input tokens. Keep it conservative — only well-known
# British spellings, not dialectal slang.
BRITISH_HINTS = {
    "color": "colour",
    "colors": "colours",
    "favorite": "favourite",
    "favor": "favour",
    "program": "programme",
    "center": "centre",
    "meter": "metre",
    "liter": "litre",
    "defense": "defence",
    "license": "licence",  # noun form
    "practice": "practise",  # verb form
    "organize": "organise",
    "organizing": "organising",
    "realize": "realise",
    "recognize": "recognise",
    "analyze": "analyse",
    "catalog": "catalogue",
    "dialog": "dialogue",
}


def _detect_proxy() -> Optional[str]:
    """Return a usable proxy URL for edge-tts.

    edge-tts ignores ``http_proxy`` / ``https_proxy`` env vars for its
    WebSocket synthesis endpoint, so we have to pass them explicitly. Pick
    up the value from the env so the user doesn't have to repeat it.
    """
    return os.environ.get("https_proxy") or os.environ.get("HTTPS_PROXY") \
        or os.environ.get("http_proxy") or os.environ.get("HTTP_PROXY")


@dataclass
class VoiceConfig:
    """All knobs that affect voice output across both TTS backends.

    Defaults are tuned for a **female, London/British-accented** narrator
    using the edge-tts backend with ``en-GB-SoniaNeural`` (native accent).

    Attributes:
        backend: ``"edge_tts"`` (default — native accent) or ``"chattts"``
            (offline, gender via seed, accent approximated via vocab).
        gender: ``"female"`` or ``"male"``. For edge_tts, picks the voice
            pool (Sonia/Libby/Maisie for female; Ryan/Thomas for male).
            For chattts, picks the seed pool.
        edge_voice: explicit edge-tts voice name overriding the
            gender-based default. e.g. ``"en-GB-SoniaNeural"``.
        edge_rate: edge-tts speaking-rate adjustment (``"+0%"`` default).
            e.g. ``"-10%"`` slower, ``"+15%"`` faster.
        edge_volume: edge-tts volume (``"+0%"`` default).
        proxy: proxy URL for edge-tts (defaults to env ``http_proxy`` /
            ``https_proxy`` — see module docstring for why this is needed).
        seed: explicit ChatTTS seed overriding the gender default. ``None``
            means use the first seed of the chosen gender's pool.
        temperature: ChatTTS decoding temperature (0.1-1.0). Lower = more
            stable/measured delivery.
        top_P: ChatTTS nucleus sampling threshold (0.5-0.9). Lower = focused.
        top_K: ChatTTS top-K sampling (10-50). Lower = consistent.
        speed_prompt: ChatTTS speed token (e.g. ``"[speed_5]"``).
        refine_prompt: ChatTTS refine-text prompt. Suppress oral/laugh
            markers for a measured delivery.
        british_vocabulary: if True (ChatTTS only), apply :data:`BRITISH_HINTS`
            to the input text before synthesis. Ignored by edge_tts (the
            voice IS already British).
    """

    backend: str = "edge_tts"
    gender: str = "female"

    # edge-tts specific
    edge_voice: Optional[str] = None
    edge_rate: str = "+0%"
    edge_volume: str = "+0%"
    proxy: Optional[str] = field(default_factory=_detect_proxy)

    # ChatTTS specific
    seed: Optional[int] = None
    temperature: float = 0.3
    top_P: float = 0.7
    top_K: int = 20
    speed_prompt: str = "[speed_5]"
    refine_prompt: str = "[oral_0][laugh_0][break_4]"
    british_vocabulary: bool = True

    def resolve_seed(self) -> int:
        """Return the ChatTTS seed (explicit override or gender default)."""
        if self.seed is not None:
            return self.seed
        pool = FEMALE_SEEDS if self.gender.lower() == "female" else MALE_SEEDS
        return pool[0]

    def resolve_edge_voice(self) -> str:
        """Return the edge-tts voice name (explicit override or gender default)."""
        if self.edge_voice:
            return self.edge_voice
        pool = EDGE_EN_GB_FEMALE_VOICES if self.gender.lower() == "female" \
            else EDGE_EN_GB_MALE_VOICES
        return pool[0]

    def apply_british_vocabulary(self, text: str) -> str:
        """Apply British English spelling substitutions to ``text``.

        Only used by the ChatTTS backend. edge-tts voices are already
        British, so this is a no-op for them.
        """
        if not self.british_vocabulary:
            return text
        out = text
        for us, uk in BRITISH_HINTS.items():
            # word-boundary replace, case-insensitive, preserving case of
            # the first letter where possible.
            def _sub(match, uk=uk):
                w = match.group(0)
                if w[0].isupper():
                    return uk.capitalize()
                return uk
            out = re.sub(rf"\b{us}\b", _sub, out, flags=re.IGNORECASE)
        return out

    def describe(self) -> str:
        """Human-readable one-line summary for logging."""
        if self.backend.lower() == "edge_tts":
            return (f"VoiceConfig(backend=edge_tts, voice={self.resolve_edge_voice()}, "
                    f"gender={self.gender}, rate={self.edge_rate}, "
                    f"proxy={'set' if self.proxy else 'none'})")
        return (f"VoiceConfig(backend=chattts, gender={self.gender}, "
                f"seed={self.resolve_seed()}, temp={self.temperature}, "
                f"top_P={self.top_P}, top_K={self.top_K}, "
                f"british_vocab={self.british_vocabulary})")
