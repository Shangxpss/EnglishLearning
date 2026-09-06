import librosa
import numpy as np
import subprocess
import tempfile
import os
from typing import Dict, Any, List
import av


class AudioProcessor:
    def __init__(self):
        pass

    def process_audio_with_subtitles(self, audio_path: str, language: str = "en", model_size: str = "medium.en") -> Dict[str, Any]:
        """
        Process audio file, generate subtitles, and extract keywords
        """
        try:
            from .subtitle_processor import SubtitleProcessor
            subtitle_processor = SubtitleProcessor()

            # Generate subtitles
            try:
                subtitles = subtitle_processor.generate_subtitles(
                    audio_path, language, model_size)
            except Exception as subtitle_error:
                # If subtitle generation fails (e.g., model download timeout), return audio analysis only
                audio_analysis = self.analyze_audio(audio_path)
                return {
                    "audio_analysis": audio_analysis,
                    "subtitles": [],
                    "keywords": [],
                    "warning": f"Subtitle generation failed: {str(subtitle_error)}"
                }

            # Extract keywords
            keywords = subtitle_processor.extract_keywords(subtitles)

            # Analyze audio
            audio_analysis = self.analyze_audio(audio_path)

            return {
                "audio_analysis": audio_analysis,
                "subtitles": subtitles,
                "keywords": keywords
            }
        except Exception as e:
            raise Exception(f"Error processing audio with subtitles: {str(e)}")

    def analyze_audio(self, audio_path: str) -> Dict[str, Any]:
        """
        Analyze audio file for English learning metrics
        """
        try:
            # Load audio file
            y, sr = librosa.load(audio_path)

            # Calculate audio metrics
            duration = librosa.get_duration(y=y, sr=sr)
            tempo, _ = librosa.beat.beat_track(y=y, sr=sr)
            # Handle case where tempo is an array
            if isinstance(tempo, np.ndarray):
                tempo = float(np.mean(tempo))

            # Extract features for English learning
            mfccs = librosa.feature.mfcc(y=y, sr=sr, n_mels=13)
            spectral_centroids = librosa.feature.spectral_centroid(y=y, sr=sr)[
                0]

            # Calculate average values
            avg_mfcc = float(np.mean(mfccs))
            avg_spectral_centroid = float(np.mean(spectral_centroids))

            # Estimate speaking speed based on zero crossing rate
            zcr = librosa.feature.zero_crossing_rate(y)
            avg_zcr = float(np.mean(zcr))

            # Detect silence ratio
            intervals = librosa.effects.split(y, top_db=20)
            total_frames = len(y)
            silence_frames = total_frames - \
                sum(interval[1] - interval[0] for interval in intervals)
            silence_ratio = silence_frames / total_frames if total_frames > 0 else 0

            return {
                "duration": round(duration, 2),
                "tempo": round(float(tempo), 2),
                "avg_mfcc": round(avg_mfcc, 2),
                "avg_spectral_centroid": round(avg_spectral_centroid, 2),
                "avg_zero_crossing_rate": round(avg_zcr, 4),
                "silence_ratio": round(silence_ratio, 4),
                "frame_rate": sr,
                "total_frames": len(y),
                "suggested_learning_level": self._determine_learning_level(duration, avg_zcr, silence_ratio)
            }
        except Exception as e:
            raise Exception(f"Error analyzing audio: {str(e)}")

    def _determine_learning_level(self, duration: float, avg_zcr: float, silence_ratio: float) -> str:
        """
        Determine suggested English learning level based on audio characteristics
        """
        # Simple heuristic based on speaking pace and clarity
        speaking_pace = avg_zcr  # Higher ZCR indicates more frequent sound changes
        clarity = 1 - silence_ratio  # Less silence indicates clearer speech

        if duration < 30 and clarity > 0.7 and speaking_pace > 0.01:
            return "Beginner - Short, clear audio suitable for beginners"
        elif duration < 60 and clarity > 0.6:
            return "Intermediate - Moderate length with reasonable clarity"
        elif duration >= 60 and clarity > 0.5:
            return "Advanced - Longer content for advanced learners"
        else:
            return "Mixed - Varies in difficulty"

    def generate_subtitles_from_video(self, video_path: str, language: str = "en", model_size: str = "base.en") -> Dict[str, Any]:
        """
        Generate subtitles from a video file.
        Extracts audio, generates subtitles, and auto-cleans temp files.
        """
        temp_audio = None
        try:
            from .subtitle_processor import SubtitleProcessor
            subtitle_processor = SubtitleProcessor()

            with tempfile.NamedTemporaryFile(suffix='.wav', delete=False) as temp_file:
                temp_audio = temp_file.name

            self.extract_audio_from_video(video_path, temp_audio)

            subtitles = subtitle_processor.generate_subtitles(
                temp_audio, language, model_size)
            keywords = subtitle_processor.extract_keywords(subtitles)
            audio_analysis = self.analyze_audio(temp_audio)

            return {
                "success": True,
                "subtitles": subtitles,
                "keywords": keywords,
                "audio_analysis": audio_analysis,
                "video_path": video_path
            }
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "subtitles": [],
                "keywords": [],
                "audio_analysis": {}
            }
        finally:
            if temp_audio and os.path.exists(temp_audio):
                try:
                    os.remove(temp_audio)
                except Exception:
                    pass

    def extract_audio_from_video(self, video_path: str, audio_path: str) -> bool:
        """
        Extract audio from video file using PyAV
        """
        try:
            container = av.open(video_path)
            audio_stream = next(
                s for s in container.streams if s.type == 'audio')

            with av.open(audio_path, 'w') as output:
                output_stream = output.add_stream('pcm_s16le', rate=16000)
                output_stream.layout = 'mono'

                for pkt in container.demux(audio_stream):
                    for frame in pkt.decode():
                        frame.pts = None
                        for encoded_pkt in output_stream.encode(frame):
                            output.mux(encoded_pkt)

                for encoded_pkt in output_stream.encode():
                    output.mux(encoded_pkt)

            return True
        except Exception as e:
            raise Exception(f"Error extracting audio from video: {str(e)}")

    def get_video_duration(self, video_path: str) -> float:
        """
        Get video duration in seconds using PyAV
        """
        try:
            container = av.open(video_path)
            duration = 0.0
            for stream in container.streams:
                if stream.duration:
                    stream_duration = float(stream.duration * stream.time_base)
                    duration = max(duration, stream_duration)
            container.close()
            return duration
        except Exception as e:
            raise Exception(f"Error getting video duration: {str(e)}")

    def clip_video_segment(self, video_path: str, output_path: str, start_time: float, end_time: float) -> bool:
        """
        Clip a segment from video using ffmpeg
        """
        import subprocess
        try:
            duration = end_time - start_time
            cmd = [
                'ffmpeg', '-y', '-i', video_path,
                '-ss', str(start_time), '-t', str(duration),
                '-c', 'copy', '-avoid_negative_ts', 'make_zero',
                output_path
            ]
            result = subprocess.run(
                cmd, capture_output=True, text=True, timeout=300)
            if result.returncode != 0:
                raise Exception(f"ffmpeg error: {result.stderr}")
            return True
        except Exception as e:
            raise Exception(f"Error clipping video segment: {str(e)}")

    def clip_audio_segment(self, video_path: str, output_audio_path: str, start_time: float, end_time: float) -> bool:
        """
        Extract audio segment from video using ffmpeg
        """
        import subprocess
        try:
            duration = end_time - start_time
            cmd = [
                'ffmpeg', '-y', '-i', video_path,
                '-ss', str(start_time), '-t', str(duration),
                '-vn', '-acodec', 'pcm_s16le', '-ar', '16000', '-ac', '1',
                output_audio_path
            ]
            result = subprocess.run(
                cmd, capture_output=True, text=True, timeout=300)
            if result.returncode != 0:
                raise Exception(f"ffmpeg error: {result.stderr}")
            return True
        except Exception as e:
            raise Exception(f"Error clipping audio segment: {str(e)}")

    def generate_translated_subtitles_from_video(
        self,
        video_path: str,
        source_language: str = "ja",
        model_size: str = "base",
        clip_duration: int = 600,
        output_dir: str = None
    ) -> Dict[str, Any]:
        """
        Generate English subtitles from a video (translates from source language).
        Clips video into segments and processes one by one.
        Saves clipped videos and subtitles to disk.

        Args:
            video_path: Path to the video file
            source_language: Source language code (default: "ja" for Japanese)
            model_size: Whisper model size (use non-English models for translation)
            clip_duration: Duration of each clip in seconds (default: 600 = 10 minutes)
            output_dir: Directory to save clips and subtitles (default: same dir as video)
        """
        if output_dir is None:
            output_dir = os.path.dirname(video_path)
        if not output_dir:
            output_dir = "."

        video_basename = os.path.splitext(os.path.basename(video_path))[0]
        segments_base_dir = os.path.join(
            output_dir, f"{video_basename}_segments")

        os.makedirs(segments_base_dir, exist_ok=True)

        try:
            from .subtitle_processor import SubtitleProcessor
            subtitle_processor = SubtitleProcessor()

            video_duration = self.get_video_duration(video_path)
            all_subtitles = []
            all_keywords = set()
            segment_count = int(
                (video_duration + clip_duration - 1) // clip_duration)
            clip_paths = []
            subtitle_paths = []
            video_subtitle_paths = []

            for segment_idx in range(segment_count):
                start_time = segment_idx * clip_duration
                end_time = min((segment_idx + 1) *
                               clip_duration, video_duration)

                audio_path = os.path.join(
                    segments_base_dir, f"audio_{segment_idx:03d}.wav")
                clip_paths.append(audio_path)

                self.clip_audio_segment(
                    video_path, audio_path, start_time, end_time)

                subtitles = subtitle_processor.generate_subtitles(
                    audio_path,
                    source_language,
                    model_size,
                    task="translate"
                )

                import copy
                original_subtitles = copy.deepcopy(subtitles)

                for sub in subtitles:
                    sub["start"] += start_time
                    sub["end"] += start_time
                    sub["id"] = len(all_subtitles) + 1
                    all_subtitles.append(sub)

                keywords = subtitle_processor.extract_keywords(subtitles)
                all_keywords.update(keywords)

                clip_video_path = os.path.join(
                    segments_base_dir, f"{video_basename}_{segment_idx:03d}.mp4")
                self.clip_video_segment(
                    video_path, clip_video_path, start_time, end_time)
                video_subtitle_paths.append(clip_video_path)

                subtitle_path = os.path.join(
                    segments_base_dir, f"{video_basename}_{segment_idx:03d}.srt")
                subtitle_paths.append(subtitle_path)
                self._write_segment_srt(original_subtitles, 0, subtitle_path)

            merged_subtitle_path = os.path.join(
                segments_base_dir, f"merged.srt")
            self._write_merged_srt(all_subtitles, merged_subtitle_path)

            return {
                "success": True,
                "subtitles": all_subtitles,
                "keywords": sorted(list(all_keywords)),
                "video_duration": video_duration,
                "segments_processed": segment_count,
                "source_language": source_language,
                "video_path": video_path,
                "clip_paths": clip_paths,
                "subtitle_paths": subtitle_paths,
                "video_subtitle_paths": video_subtitle_paths,
                "merged_subtitle_path": merged_subtitle_path,
                "segments_base_dir": segments_base_dir
            }
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "subtitles": [],
                "keywords": []
            }

    def _write_merged_srt(self, subtitles: List[Dict[str, Any]], output_path: str):
        """
        Write merged subtitles to a single SRT file
        """
        with open(output_path, 'w', encoding='utf-8') as f:
            for sub in subtitles:
                f.write(f"{sub['id']}\n")
                f.write(
                    f"{self._format_time(sub['start'])} --> {self._format_time(sub['end'])}\n")
                f.write(f"{sub['text']}\n\n")

    def _write_segment_srt(self, subtitles: List[Dict[str, Any]], start_time: float, output_path: str):
        """
        Write subtitles for a segment with correct video timeline offset.
        The subtitles list contains subtitles with timestamps already adjusted (start_time added).
        """
        with open(output_path, 'w', encoding='utf-8') as f:
            for i, sub in enumerate(subtitles, 1):
                f.write(f"{i}\n")
                f.write(
                    f"{self._format_time(sub['start'])} --> {self._format_time(sub['end'])}\n")
                f.write(f"{sub['text']}\n\n")

    def _format_time(self, seconds: float) -> str:
        """
        Format seconds to SRT time format (HH:MM:SS,mmm)
        """
        hours = int(seconds // 3600)
        minutes = int((seconds % 3600) // 60)
        secs = int(seconds % 60)
        millisecs = int((seconds % 1) * 1000)
        return f"{hours:02d}:{minutes:02d}:{secs:02d},{millisecs:03d}"
