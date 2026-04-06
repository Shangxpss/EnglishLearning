import librosa
import numpy as np
import subprocess
import tempfile
import os
from typing import Dict, Any
import ffmpeg


class AudioProcessor:
    def __init__(self):
        pass

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

    def extract_audio_from_video(self, video_path: str, audio_path: str) -> bool:
        """
        Extract audio from video file
        """
        try:
            stream = ffmpeg.input(video_path)
            stream = ffmpeg.output(
                stream, audio_path, acodec='pcm_s16le', ac=1, ar='16k')
            ffmpeg.run(stream, overwrite_output=True, quiet=True)
            return True
        except Exception as e:
            raise Exception(f"Error extracting audio from video: {str(e)}")
