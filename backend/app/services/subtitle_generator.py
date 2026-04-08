import os
import tempfile
from typing import List, Dict, Any


class SubtitleGenerator:
    def __init__(self):
        # Initialize with a default model - will be loaded on demand
        self.models = {}

    def generate_subtitles(self, audio_path: str, language: str = "en", model_size: str = "small") -> List[Dict[str, Any]]:
        """
        Generate subtitles for audio file
        """
        try:
            from faster_whisper import WhisperModel
            # Load model if not already loaded
            if model_size not in self.models:
                self.models[model_size] = WhisperModel(
                    model_size, device="cpu", compute_type="int8")

            model = self.models[model_size]

            # Transcribe audio
            segments, info = model.transcribe(audio_path, language=language)

            subtitles = []
            for i, segment in enumerate(segments, 1):
                subtitle = {
                    "id": i,
                    "start": segment.start,
                    "end": segment.end,
                    "text": segment.text.strip(),
                    # Use logprob as confidence if available
                    "confidence": getattr(segment, 'avg_logprob', 0)
                }
                subtitles.append(subtitle)

            return subtitles
        except Exception as e:
            raise Exception(f"Error generating subtitles: {str(e)}")

    def generate_srt_subtitles(self, audio_path: str, output_path: str, language: str = "en", model_size: str = "small"):
        """
        Generate SRT subtitle file
        """
        try:
            import ffmpeg
            subtitles = self.generate_subtitles(
                audio_path, language, model_size)

            with open(output_path, 'w', encoding='utf-8') as f:
                for sub in subtitles:
                    # Write SRT format
                    f.write(f"{sub['id']}\n")
                    f.write(
                        f"{self._format_time(sub['start'])} --> {self._format_time(sub['end'])}\n")
                    f.write(f"{sub['text']}\n\n")

            return True
        except Exception as e:
            raise Exception(f"Error generating SRT file: {str(e)}")

    def _format_time(self, seconds: float) -> str:
        """
        Format seconds to SRT time format (HH:MM:SS,mmm)
        """
        hours = int(seconds // 3600)
        minutes = int((seconds % 3600) // 60)
        secs = int(seconds % 60)
        millisecs = int((seconds % 1) * 1000)
        return f"{hours:02d}:{minutes:02d}:{secs:02d},{millisecs:03d}"

    def parse_subtitle_file(self, subtitle_path: str) -> List[Dict[str, Any]]:
        """
        Parse subtitle file (SRT format) and return structured data
        """
        try:
            subtitles = []
            with open(subtitle_path, 'r', encoding='utf-8') as f:
                content = f.read()
                
            # Split by subtitle blocks
            blocks = content.strip().split('\n\n')
            
            for i, block in enumerate(blocks, 1):
                lines = block.strip().split('\n')
                if len(lines) >= 3:
                    # Extract ID, time, and text
                    try:
                        # ID is lines[0]
                        # Time is lines[1]
                        time_line = lines[1]
                        start_time, end_time = time_line.split(' --> ')
                        
                        # Extract text (lines[2] and beyond)
                        text = ' '.join(lines[2:]).strip()
                        
                        subtitle = {
                            "id": i,
                            "start": self._parse_srt_time(start_time),
                            "end": self._parse_srt_time(end_time),
                            "text": text
                        }
                        subtitles.append(subtitle)
                    except Exception as e:
                        # Skip malformed blocks
                        continue
            
            return subtitles
        except Exception as e:
            raise Exception(f"Error parsing subtitle file: {str(e)}")

    def _parse_srt_time(self, time_str: str) -> float:
        """
        Parse SRT time format (HH:MM:SS,mmm) to seconds
        """
        try:
            parts = time_str.split(':')
            hours = int(parts[0])
            minutes = int(parts[1])
            sec_millis = parts[2].split(',')
            seconds = int(sec_millis[0])
            milliseconds = int(sec_millis[1]) if len(sec_millis) > 1 else 0
            
            total_seconds = hours * 3600 + minutes * 60 + seconds + milliseconds / 1000
            return total_seconds
        except Exception as e:
            raise Exception(f"Error parsing time: {str(e)}")

    def extract_keywords(self, subtitles: List[Dict[str, Any]]) -> List[str]:
        """
        Extract keywords from subtitles, filtering out simple words
        """
        try:
            # Common simple words to filter out
            simple_words = {
                'the', 'a', 'an', 'and', 'or', 'but', 'is', 'are', 'was', 'were',
                'in', 'on', 'at', 'to', 'for', 'with', 'by', 'from', 'as', 'of',
                'I', 'you', 'he', 'she', 'it', 'we', 'they', 'me', 'him', 'her',
                'us', 'them', 'my', 'your', 'his', 'her', 'its', 'our', 'their',
                'this', 'that', 'these', 'those', 'am', 'be', 'been', 'being',
                'have', 'has', 'had', 'do', 'does', 'did', 'will', 'would', 'should',
                'could', 'can', 'may', 'might', 'must', 'shall', 'what', 'when',
                'where', 'why', 'how', 'who', 'whom', 'whose', 'which', 'if', 'then',
                'than', 'so', 'because', 'since', 'until', 'while', 'after', 'before',
                'during', 'though', 'although', 'unless', 'except', 'without', 'within'
            }
            
            import re
            keywords = set()
            
            for subtitle in subtitles:
                text = subtitle.get('text', '')
                # Extract words using regex, removing punctuation
                words = re.findall(r'\b\w+\b', text.lower())
                # Filter out simple words and single-character words
                filtered_words = [word for word in words if word not in simple_words and len(word) > 1]
                keywords.update(filtered_words)
            
            # Convert to sorted list
            return sorted(list(keywords))
        except Exception as e:
            raise Exception(f"Error extracting keywords: {str(e)}")
