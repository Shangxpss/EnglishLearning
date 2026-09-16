"""Video dubber.

Replaces the audio track of an existing video with a clean English
voiceover synthesised from the video's own subtitles.

Pipeline (reuses existing services):

    1. Extract audio from the source video
       (AudioProcessor.extract_audio_from_video)
    2. Transcribe the audio to subtitles. If the source is non-English
       (or has a heavy accent), the Whisper ``translate`` task is used so
       the subtitles are in English.
       (AudioProcessor.generate_subtitles_from_video /
        generate_translated_subtitles_from_video)
    3. Write the subtitles to an .srt file.
    4. Run the "Generate & Time-Stretch" pipeline on the .srt to produce a
       single dubbed_audio.mp3 whose timeline matches the original video.
       (SubtitleVideoGenerator.generate_audio_only)
    5. Mux the dubbed audio back onto the original video with ffmpeg,
       optionally burning in the English subtitles.

This is exactly the "separate video and audio, regenerate the audio, then
merge them" workflow described in the request.
"""

from __future__ import annotations

import os
import subprocess
import uuid
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

logger = __import__("logging").getLogger(__name__)


@dataclass
class DubbingResult:
    success: bool
    video_path: str = ""
    audio_path: str = ""
    subtitle_path: str = ""
    extracted_audio_path: str = ""
    cues: List[Dict[str, Any]] = field(default_factory=list)
    stats: Dict[str, Any] = field(default_factory=dict)
    error: str = ""


class VideoDubber:
    """Dubs a video into clean English speech."""

    def dub(
        self,
        video_path: str,
        output_dir: Optional[str] = None,
        source_language: str = "en",
        translate: bool = False,
        whisper_model: str = "base",
        voice: str = "en-US-AriaNeural",
        rate: str = "+0%",
        volume: str = "+0%",
        pitch: str = "+0Hz",
        burn_subtitles: bool = False,
        keep_original_audio: bool = False,
        original_audio_volume: int = 0,  # 0-100, only used with keep_original_audio
        concurrency: int = 8,
        clip_duration: int = 600,
    ) -> DubbingResult:
        """Dub ``video_path`` into English.

        Parameters
        ----------
        video_path
            Path to the source video.
        source_language
            Language code of the source audio (e.g. ``"ja"``, ``"en"``).
        translate
            If True, Whisper's ``translate`` task is used so the subtitles
            (and therefore the voiceover) are in English even when the
            source is a different language. Set this for non-English videos.
            If False, the source is transcribed as-is (use for heavy-accent
            English where you want the same language re-spoken cleanly).
        whisper_model
            Whisper model size (``"base"``, ``"small"``, ``"medium"``, ...).
            Use a non-English model when ``translate=True``.
        voice
            Edge TTS voice name for the voiceover.
        rate / volume / pitch
            Edge TTS prosody parameters.
        burn_subtitles
            If True, the English subtitles are burned into the output video.
        keep_original_audio
            If True, the original audio track is kept (mixed under the
            voiceover at reduced volume) instead of being replaced. Useful
            for preserving background music / ambience.
        original_audio_volume
            Volume (0-100) of the original audio when ``keep_original_audio``
            is True. 0 = silent, 100 = original level.
        concurrency
            How many TTS requests to run in parallel.
        clip_duration
            For long videos, Whisper transcription is done in chunks of this
            many seconds (passed through to AudioProcessor).
        """
        if not os.path.exists(video_path):
            return DubbingResult(success=False, error=f"Video not found: {video_path}")

        if output_dir is None:
            output_dir = os.path.dirname(video_path) or "."
        os.makedirs(output_dir, exist_ok=True)

        run_id = uuid.uuid4().hex[:8]
        work_dir = os.path.join(output_dir, f"dub_{run_id}")
        os.makedirs(work_dir, exist_ok=True)

        video_basename = os.path.splitext(os.path.basename(video_path))[0]
        output_video = os.path.join(
            output_dir, f"{video_basename}_dubbed_en.mp4"
        )

        try:
            # Lazily import the heavy services so the module can still be
            # imported when optional deps are missing.
            from .audio_processor import AudioProcessor
            from .subtitle_video_generator import (
                subtitle_video_generator,
                _write_clean_srt,
                SubtitleCue,
            )

            audio_processor = AudioProcessor()

            # --- 1. Transcribe / translate the video ---------------------
            logger.info(
                "VideoDubber: transcribing video (lang=%s, translate=%s, model=%s)",
                source_language, translate, whisper_model,
            )
            if translate:
                transcribe_result = audio_processor.generate_translated_subtitles_from_video(
                    video_path,
                    source_language=source_language,
                    model_size=whisper_model,
                    clip_duration=clip_duration,
                )
            else:
                transcribe_result = audio_processor.generate_subtitles_from_video(
                    video_path,
                    language=source_language,
                    model_size=whisper_model,
                )

            if not transcribe_result.get("success"):
                err = transcribe_result.get("error", "Transcription failed")
                return DubbingResult(success=False, error=str(err))

            raw_subs = transcribe_result.get("subtitles", [])
            if not raw_subs:
                return DubbingResult(
                    success=False,
                    error="No subtitles could be generated from the video audio.",
                )

            # --- 2. Persist subtitles to a clean .srt --------------------
            cues: List[SubtitleCue] = [
                SubtitleCue(
                    index=i,
                    start=float(s.get("start", 0.0)),
                    end=float(s.get("end", 0.0)),
                    text=str(s.get("text", "")).strip(),
                )
                for i, s in enumerate(raw_subs, 1)
                if str(s.get("text", "")).strip()
            ]
            if not cues:
                return DubbingResult(success=False, error="All subtitle cues were empty.")

            srt_path = os.path.join(work_dir, "subs.srt")
            _write_clean_srt(cues, srt_path)

            # --- 3. Get the video duration so the dubbed audio lines up --
            video_duration = transcribe_result.get("video_duration") or \
                transcribe_result.get("audio_analysis", {}).get("duration") or \
                audio_processor.get_video_duration(video_path)

            # --- 4. Generate the English voiceover -----------------------
            logger.info(
                "VideoDubber: generating voiceover for %d cues (voice=%s)",
                len(cues), voice,
            )
            audio_result = subtitle_video_generator.generate_audio_only(
                subtitle_path=srt_path,
                output_dir=work_dir,
                voice=voice,
                rate=rate,
                volume=volume,
                pitch=pitch,
                concurrency=concurrency,
                target_total_duration=float(video_duration) if video_duration else None,
            )
            if not audio_result.success:
                return DubbingResult(success=False, error=audio_result.error)

            # --- 5. Mux the dubbed audio back onto the original video ----
            logger.info("VideoDubber: muxing audio + video -> %s", output_video)
            self._mux(
                video_path=video_path,
                dubbed_audio_path=audio_result.audio_path,
                output_path=output_video,
                burn_subtitles=burn_subtitles,
                subtitle_path=srt_path if burn_subtitles else None,
                keep_original_audio=keep_original_audio,
                original_audio_volume=max(0, min(100, original_audio_volume)),
            )

            return DubbingResult(
                success=True,
                video_path=output_video,
                audio_path=audio_result.audio_path,
                subtitle_path=srt_path,
                extracted_audio_path=transcribe_result.get("video_path", ""),
                cues=[
                    {"index": c.index, "start": c.start, "end": c.end, "text": c.text}
                    for c in cues
                ],
                stats={
                    "source_language": source_language,
                    "translated": translate,
                    "whisper_model": whisper_model,
                    "video_duration": video_duration,
                    "voice": voice,
                    "cues": len(cues),
                    "stretch_methods": audio_result.stats.get("stretch_methods", {}),
                    "min_stretch_ratio": audio_result.stats.get("min_stretch_ratio"),
                    "max_stretch_ratio": audio_result.stats.get("max_stretch_ratio"),
                    "total_audio_duration": audio_result.stats.get("total_duration"),
                    "work_dir": work_dir,
                    "burn_subtitles": burn_subtitles,
                    "keep_original_audio": keep_original_audio,
                },
            )
        except Exception as exc:
            logger.exception("Video dubbing failed")
            return DubbingResult(success=False, error=str(exc))

    def dub_from_subtitle(
        self,
        video_path: str,
        subtitle_path: str,
        output_dir: Optional[str] = None,
        voice: str = "en-US-AriaNeural",
        rate: str = "+0%",
        volume: str = "+0%",
        pitch: str = "+0Hz",
        burn_subtitles: bool = False,
        keep_original_audio: bool = False,
        original_audio_volume: int = 0,
        concurrency: int = 8,
    ) -> DubbingResult:
        """Dub a silent (or already-separated) video using a ready subtitle file.

        This is the third dubbing mode: the user already has the video and a
        subtitle file (.srt / .vtt) with correct timings, so we skip the
        Whisper transcription step entirely and go straight to:

            1. Read the video duration (so the audio timeline lines up).
            2. Run the "Generate & Time-Stretch" pipeline on the subtitle
               file to produce a single dubbed_audio.mp3
               (SubtitleVideoGenerator.generate_audio_only — reused, no
               duplicated code).
            3. Mux the dubbed audio onto the source video with the same
               ``_mux`` helper used by :meth:`dub` (replace / mix-under /
               burn-subtitles modes).

        Use this when:
          * the source video is already silent (audio stripped), or
          * you have a corrected/translated .srt you'd rather use than the
            auto-transcribed one.
        """
        if not os.path.exists(video_path):
            return DubbingResult(success=False, error=f"Video not found: {video_path}")
        if not os.path.exists(subtitle_path):
            return DubbingResult(success=False, error=f"Subtitle not found: {subtitle_path}")

        if output_dir is None:
            output_dir = os.path.dirname(video_path) or "."
        os.makedirs(output_dir, exist_ok=True)

        run_id = uuid.uuid4().hex[:8]
        work_dir = os.path.join(output_dir, f"dub_{run_id}")
        os.makedirs(work_dir, exist_ok=True)

        video_basename = os.path.splitext(os.path.basename(video_path))[0]
        output_video = os.path.join(
            output_dir, f"{video_basename}_dubbed_en.mp4"
        )

        try:
            from .audio_processor import AudioProcessor
            from .subtitle_video_generator import subtitle_video_generator

            # 1. Video duration so the audio is padded to match the timeline.
            video_duration = AudioProcessor().get_video_duration(video_path)

            # 2. Generate the voiceover (reused pipeline — no duplication).
            logger.info(
                "VideoDubber.dub_from_subtitle: generating voiceover "
                "(voice=%s, target_duration=%s)",
                voice, video_duration,
            )
            audio_result = subtitle_video_generator.generate_audio_only(
                subtitle_path=subtitle_path,
                output_dir=work_dir,
                voice=voice,
                rate=rate,
                volume=volume,
                pitch=pitch,
                concurrency=concurrency,
                target_total_duration=float(video_duration) if video_duration else None,
            )
            if not audio_result.success:
                return DubbingResult(success=False, error=audio_result.error)

            # 3. Mux (same helper as the auto-transcribe mode).
            logger.info("VideoDubber.dub_from_subtitle: muxing -> %s", output_video)
            self._mux(
                video_path=video_path,
                dubbed_audio_path=audio_result.audio_path,
                output_path=output_video,
                burn_subtitles=burn_subtitles,
                subtitle_path=audio_result.subtitle_path if burn_subtitles else None,
                keep_original_audio=keep_original_audio,
                original_audio_volume=max(0, min(100, original_audio_volume)),
            )

            return DubbingResult(
                success=True,
                video_path=output_video,
                audio_path=audio_result.audio_path,
                subtitle_path=audio_result.subtitle_path,
                cues=audio_result.cues,
                stats={
                    "source_language": "subtitle",
                    "translated": False,
                    "whisper_model": None,
                    "video_duration": video_duration,
                    "voice": voice,
                    "cues": len(audio_result.cues),
                    "stretch_methods": audio_result.stats.get("stretch_methods", {}),
                    "min_stretch_ratio": audio_result.stats.get("min_stretch_ratio"),
                    "max_stretch_ratio": audio_result.stats.get("max_stretch_ratio"),
                    "total_audio_duration": audio_result.stats.get("total_duration"),
                    "work_dir": work_dir,
                    "burn_subtitles": burn_subtitles,
                    "keep_original_audio": keep_original_audio,
                    "mode": "subtitle",
                },
            )
        except Exception as exc:
            logger.exception("Video dubbing from subtitle failed")
            return DubbingResult(success=False, error=str(exc))

    # --- Natural-quality dubbing (new pipeline) ----------------------------
    #
    # The methods below implement the dual-path architecture described in
    # ``docs/NATURAL_ACCENT_DUBBING_DESIGN.md``. They wire together:
    #
    #   * accent_detector  — decides Path A (VC) vs Path B (TTS)
    #   * voice_converter   — Path A: waveform → target voice (no time-stretch)
    #   * natural_tts       — Path B: text → cloned-voice speech
    #   * lip_syncer        — optional VideoReTalking pass for talking-heads
    #
    # The existing ``dub()`` and ``dub_from_subtitle()`` are unchanged for
    # backward compatibility. The new ``dub_natural()`` is the entry point
    # for the high-quality pipeline.

    def dub_natural(
        self,
        video_path: str,
        output_dir: Optional[str] = None,
        target_accent: str = "london",
        reference_voice: Optional[str] = None,
        preserve_speaker: bool = True,
        enable_lip_sync: bool = False,
        force_path: Optional[str] = None,
        whisper_model: str = "base",
        translate: bool = False,
        clip_duration: int = 600,
        keep_original_audio: bool = False,
        original_audio_volume: int = 0,
        burn_subtitles: bool = False,
        concurrency: int = 4,
        tts_speed: float = 1.0,
    ) -> DubbingResult:
        """Natural-quality accent replacement.

        Picks the best path automatically:

          * **Path A** (default, ``preserve_speaker=True``) — Voice Conversion.
            Converts the bad-accent waveform into a clear London-accent voice
            while preserving the speaker's prosody, timing, and emotion.
            **No time-stretch needed** — this is the biggest quality win.
            Only available when the source is English (even if heavily
            accented).

          * **Path B** (``preserve_speaker=False``, or non-English source) —
            TTS with voice cloning (F5-TTS / OpenVoice v2 / CosyVoice 2).
            Transcribes the source with Whisper, optionally translates, then
            synthesises fresh speech in the cloned target-accent voice.

          * **Path C** (fallback) — legacy edge-tts + time-stretch (the
            existing ``dub()`` pipeline). Used when no ML backend is installed.

        Parameters
        ----------
        target_accent
            "london" | "american" | "indian" | "australian" — drives the
            reference voice / TTS accent selection.
        reference_voice
            Path to a 5–15 s wav of the desired target voice. If None, uses
            the pre-bundled default at ``app/assets/reference_voices/<accent>.wav``.
            For Path A with RVC, this should be a trained ``.pth`` model
            configured via ``RVC_MODEL_PATH`` env var.
        preserve_speaker
            If True (default), prefer Path A so the speaker's own cadence is
            kept. Set False to force Path B (re-synthesise from text).
        enable_lip_sync
            If True, run VideoReTalking after audio replacement to re-sync
            lip movements (talking-head videos only; slow).
        force_path
            "A" | "B" | "C" to override the automatic path selection.
        """
        if not os.path.exists(video_path):
            return DubbingResult(success=False, error=f"Video not found: {video_path}")

        if output_dir is None:
            output_dir = os.path.dirname(video_path) or "."
        os.makedirs(output_dir, exist_ok=True)

        run_id = uuid.uuid4().hex[:8]
        work_dir = os.path.join(output_dir, f"dubnat_{run_id}")
        os.makedirs(work_dir, exist_ok=True)

        video_basename = os.path.splitext(os.path.basename(video_path))[0]
        output_video = os.path.join(output_dir, f"{video_basename}_natural.mp4")

        try:
            from .audio_processor import AudioProcessor
            from .accent_detector import accent_detector
            from .voice_converter import voice_converter, VoiceConversionConfig
            from .natural_tts import natural_tts, SynthesisRequest
            from .lip_syncer import lip_syncer, LipSyncConfig
            from .subtitle_video_generator import _write_clean_srt, SubtitleCue

            audio_processor = AudioProcessor()

            # --- 1. Extract source audio ----------------------------------
            source_wav = os.path.join(work_dir, "source.wav")
            logger.info("VideoDubber.dub_natural: extracting audio -> %s", source_wav)
            audio_processor.extract_audio_from_video(video_path, source_wav)

            video_duration = audio_processor.get_video_duration(video_path) or 0.0

            # --- 2. Detect language & choose path -------------------------
            detection = accent_detector.detect_language(source_wav)
            decision = accent_detector.choose_path(
                detection, prefer_vc=preserve_speaker, force_path=force_path)

            logger.info(
                "VideoDubber.dub_natural: path=%s lang=%s conf=%.2f reason=%s",
                decision.path, decision.language, decision.confidence, decision.reason,
            )

            # Resolve the reference voice (for both paths).
            ref_voice = reference_voice or self._resolve_reference_voice(target_accent)

            # --- 3. Run the chosen path -----------------------------------
            srt_path: Optional[str] = None
            cues: List[Dict[str, Any]] = []

            if decision.path == "A" and voice_converter.is_available():
                # ---- Path A: Voice Conversion ----
                dubbed_audio = os.path.join(work_dir, "converted.wav")
                cfg = VoiceConversionConfig()
                result = voice_converter.convert(source_wav, dubbed_audio, cfg)
                if not result.success:
                    logger.warning(
                        "Path A failed (%s); falling back to Path B", result.error)
                    dubbed_audio, srt_path, cues = self._run_path_b(
                        audio_processor, video_path, work_dir, ref_voice,
                        target_accent, whisper_model, translate, clip_duration,
                        concurrency, tts_speed, video_duration, _write_clean_srt,
                        SubtitleCue)
                else:
                    dubbed_audio = result.audio_path

            elif decision.path == "B" or not voice_converter.is_available():
                # ---- Path B: TTS + cloning (or fallback to C) ----
                if not natural_tts.is_available():
                    # Path C: legacy edge-tts pipeline (existing dub() logic).
                    logger.info("No ML TTS backend; falling back to Path C (edge-tts)")
                    return self.dub(
                        video_path=video_path, output_dir=output_dir,
                        source_language=decision.language,
                        translate=translate or not detection.is_english,
                        whisper_model=whisper_model,
                        voice=self._accent_to_edge_voice(target_accent),
                        keep_original_audio=keep_original_audio,
                        original_audio_volume=original_audio_volume,
                        burn_subtitles=burn_subtitles,
                        clip_duration=clip_duration,
                    )
                dubbed_audio, srt_path, cues = self._run_path_b(
                    audio_processor, video_path, work_dir, ref_voice,
                    target_accent, whisper_model, translate, clip_duration,
                    concurrency, tts_speed, video_duration, _write_clean_srt,
                    SubtitleCue)

            else:
                return DubbingResult(
                    success=False,
                    error=("No dubbing backend available. Install RVC, "
                           "F5-TTS, or edge-tts."))

            # --- 4. Optional lip sync -------------------------------------
            final_video_path = video_path
            if enable_lip_sync and lip_syncer.is_available():
                ls_out = os.path.join(work_dir, "lipsynced.mp4")
                logger.info("VideoDubber.dub_natural: running lip sync -> %s", ls_out)
                ls_result = lip_syncer.sync(
                    video_path, dubbed_audio, ls_out,
                    config=LipSyncConfig(face_enhancer="gfpgan"))
                if ls_result.success:
                    final_video_path = ls_result.video_path
                else:
                    logger.warning("Lip sync skipped/failed: %s", ls_result.error)

            # --- 5. Mux ---------------------------------------------------
            logger.info("VideoDubber.dub_natural: muxing -> %s", output_video)
            self._mux(
                video_path=final_video_path,
                dubbed_audio_path=dubbed_audio,
                output_path=output_video,
                burn_subtitles=burn_subtitles,
                subtitle_path=srt_path if burn_subtitles else None,
                keep_original_audio=keep_original_audio,
                original_audio_volume=max(0, min(100, original_audio_volume)),
            )

            return DubbingResult(
                success=True,
                video_path=output_video,
                audio_path=dubbed_audio,
                subtitle_path=srt_path or "",
                extracted_audio_path=source_wav,
                cues=cues,
                stats={
                    "path": decision.path,
                    "path_reason": decision.reason,
                    "source_language": decision.language,
                    "language_confidence": decision.confidence,
                    "target_accent": target_accent,
                    "preserve_speaker": preserve_speaker,
                    "enable_lip_sync": enable_lip_sync,
                    "video_duration": video_duration,
                    "vc_backends": voice_converter.list_backends() if decision.path == "A" else {},
                    "tts_backends": natural_tts.list_backends() if decision.path == "B" else {},
                    "work_dir": work_dir,
                },
            )
        except Exception as exc:
            logger.exception("Natural dubbing failed")
            return DubbingResult(success=False, error=str(exc))

    # --- Path B helper ------------------------------------------------------

    def _run_path_b(self, audio_processor, video_path, work_dir, ref_voice,
                    target_accent, whisper_model, translate, clip_duration,
                    concurrency, tts_speed, video_duration,
                    _write_clean_srt, SubtitleCue):
        """Transcribe → synthesise with natural TTS → return audio + cues."""
        from .natural_tts import natural_tts

        logger.info("Path B: transcribing video (translate=%s)", translate)
        if translate:
            tr = audio_processor.generate_translated_subtitles_from_video(
                video_path, source_language="auto",
                model_size=whisper_model, clip_duration=clip_duration)
        else:
            tr = audio_processor.generate_subtitles_from_video(
                video_path, language="en", model_size=whisper_model)

        if not tr.get("success"):
            raise RuntimeError(tr.get("error", "Transcription failed"))

        raw_subs = tr.get("subtitles", [])
        cues = [
            {"index": i, "start": float(s.get("start", 0)),
             "end": float(s.get("end", 0)), "text": str(s.get("text", "")).strip()}
            for i, s in enumerate(raw_subs, 1)
            if str(s.get("text", "")).strip()
        ]
        if not cues:
            raise RuntimeError("No subtitle cues generated")

        # Write SRT (for burn-in option + reference).
        cue_objs = [
            SubtitleCue(index=c["index"], start=c["start"], end=c["end"], text=c["text"])
            for c in cues
        ]
        srt_path = os.path.join(work_dir, "subs.srt")
        _write_clean_srt(cue_objs, srt_path)

        # Synthesise with natural TTS.
        batch = natural_tts.synthesize_many(
            cues=cues,
            reference_audio=ref_voice,
            output_dir=os.path.join(work_dir, "tts"),
            accent=target_accent,
            speed=tts_speed,
            concurrency=concurrency,
            target_total_duration=video_duration or None,
        )
        if not batch.success:
            raise RuntimeError(batch.error)

        return batch.audio_path, srt_path, cues

    # --- Reference-voice resolution -----------------------------------------

    def _resolve_reference_voice(self, accent: str) -> str:
        """Return the path to the pre-bundled reference wav for ``accent``.

        Falls back to the London default if the requested accent's file is
        missing. The caller can always override by passing ``reference_voice``
        explicitly.
        """
        base = os.path.join(os.path.dirname(os.path.dirname(__file__)),
                            "assets", "reference_voices")
        candidates = [
            os.path.join(base, f"{accent}.wav"),
            os.path.join(base, f"{accent}_default.wav"),
            os.path.join(base, "london_default.wav"),
        ]
        for c in candidates:
            if os.path.exists(c):
                return c
        # Last resort: return the expected path even if missing; the backend
        # will give a clearer error than a silent default.
        return candidates[0]

    @staticmethod
    def _accent_to_edge_voice(accent: str) -> str:
        """Map accent name → edge-tts voice name (for Path C fallback)."""
        mapping = {
            "london": "en-GB-SoniaNeural",
            "british": "en-GB-SoniaNeural",
            "american": "en-US-AriaNeural",
            "indian": "en-IN-NeerjaNeural",
            "australian": "en-AU-NatashaNeural",
        }
        return mapping.get((accent or "london").lower(), "en-GB-SoniaNeural")

    # --- ffmpeg muxing ------------------------------------------------------

    def _mux(
        self,
        video_path: str,
        dubbed_audio_path: str,
        output_path: str,
        burn_subtitles: bool,
        subtitle_path: Optional[str],
        keep_original_audio: bool,
        original_audio_volume: int,
    ) -> None:
        """Mux the dubbed audio onto the source video.

        * If ``keep_original_audio`` is True, the original audio is mixed under
          the voiceover at ``original_audio_volume`` percent.
        * If ``burn_subtitles`` is True, the English subtitles are burned into
          the video (requires re-encoding the video stream).
        * Otherwise the video stream is copied (``-c:v copy``) for speed.
        """
        # Build the audio filter chain.
        if keep_original_audio:
            # mix original (at reduced volume) + dubbed voiceover
            vol = original_audio_volume / 100.0
            audio_filter = (
                f"[0:a]volume={vol:.2f}[a0];"
                f"[a0][1:a]amix=inputs=2:duration=first:dropout_transition=0[a]"
            )
            audio_map = "[a]"
        else:
            audio_filter = None
            audio_map = "1:a"

        if burn_subtitles and subtitle_path:
            from .subtitle_video_generator import _escape_filter_path
            sub_filter = (
                f"subtitles={_escape_filter_path(subtitle_path)}:force_style="
                "'Alignment=2,MarginV=60,FontSize=18,Outline=2,Shadow=1,"
                "PrimaryColour=&HFFFFFFFF,OutlineColour=&H00000000'"
            )
            if keep_original_audio:
                cmd = [
                    "ffmpeg", "-y",
                    "-i", video_path,
                    "-i", dubbed_audio_path,
                    "-filter_complex",
                    f"[0:v]{sub_filter}[v];{audio_filter}",
                    "-map", "[v]",
                    "-map", audio_map,
                    "-c:v", "libx264",
                    "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-shortest",
                    output_path,
                ]
            else:
                cmd = [
                    "ffmpeg", "-y",
                    "-i", video_path,
                    "-i", dubbed_audio_path,
                    "-filter_complex", f"[0:v]{sub_filter}[v]",
                    "-map", "[v]",
                    "-map", "1:a",
                    "-c:v", "libx264",
                    "-pix_fmt", "yuv420p",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-shortest",
                    output_path,
                ]
        else:
            # Fast path: copy the video stream, replace audio.
            if keep_original_audio:
                cmd = [
                    "ffmpeg", "-y",
                    "-i", video_path,
                    "-i", dubbed_audio_path,
                    "-filter_complex", audio_filter,
                    "-map", "0:v",
                    "-map", audio_map,
                    "-c:v", "copy",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-shortest",
                    output_path,
                ]
            else:
                cmd = [
                    "ffmpeg", "-y",
                    "-i", video_path,
                    "-i", dubbed_audio_path,
                    "-map", "0:v",
                    "-map", "1:a",
                    "-c:v", "copy",
                    "-c:a", "aac",
                    "-b:a", "192k",
                    "-shortest",
                    output_path,
                ]

        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(
                f"ffmpeg mux failed: {result.stderr[-2000:]}"
            )


# Convenience singleton, mirroring the pattern used by other services.
video_dubber = VideoDubber()
