import ffmpeg
from faster_whisper import WhisperModel
import os

# 设置环境变量以避免网络问题
os.environ['HF_HUB_ENABLE_HF_TRANSFER'] = '1'
os.environ['HF_ENDPOINT'] = 'https://hf-mirror.com'  # 设置国内镜像

# 其他可能有用的环境变量
os.environ['HF_HUB_OFFLINE'] = '0'
os.environ['TRANSFORMERS_OFFLINE'] = '0'


def extract_audio(input_file, output_audio):
    stream = ffmpeg.input(input_file)  # type: ignore
    stream = ffmpeg.output(stream, output_audio)  # type: ignore
    ffmpeg.run(stream, overwrite_output=True)  # type: ignore


def transcribe_audio(audio_file):
    # Use "small", "medium", or "large" for accuracy
    model = WhisperModel(model_size_or_path="base.en")
    segments, _ = model.transcribe(audio_file)
    return [(segment.start, segment.end, segment.text) for segment in segments]


def main():
    print("Audio to Subtitle Converter")

    # Your audio file in video folder
    input_audio = "video/finance.m4s"
    output_subtitle = "finance_subtitle.srt"

    # Step 1: Transcribe audio to text
    print(f"Step 1: Transcribing audio from {input_audio}...")
    transcriptions = transcribe_audio(input_audio)
    print(f"Transcription completed. Found {len(transcriptions)} segments")

    # Step 2: Generate subtitle file
    print("Step 2: Generating subtitle file...")
    generate_subtitle(transcriptions, output_subtitle)
    print(f"Subtitle generated: {output_subtitle}")

    # Display sample of transcriptions
    print("\nSample transcriptions:")
    for i, (start, end, text) in enumerate(transcriptions[:5]):
        print(f"{i+1}. [{start:.2f}s - {end:.2f}s]: {text}")

    print(f"\nFull subtitle file saved as: {output_subtitle}")


def generate_subtitle(transcriptions, output_file):
    """Generate SRT format subtitle file"""
    with open(output_file, 'w', encoding='utf-8') as f:
        for i, (start, end, text) in enumerate(transcriptions, 1):
            # Format time to SRT format: 00:00:00,000 --> 00:00:00,000
            start_time = format_time(start)
            end_time = format_time(end)

            f.write(f"{i}\n")
            f.write(f"{start_time} --> {end_time}\n")
            f.write(f"{text}\n\n")


def format_time(seconds):
    """Convert seconds to SRT time format"""
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)
    secs = int(seconds % 60)
    millisecs = int((seconds % 1) * 1000)
    return f"{hours:02d}:{minutes:02d}:{secs:02d},{millisecs:03d}"


if __name__ == "__main__":
    main()
