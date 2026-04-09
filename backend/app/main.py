from fastapi import FastAPI, UploadFile, File, Form
from fastapi.responses import JSONResponse
from typing import Optional
import os
import tempfile
import subprocess
import uuid
from .models.response_models import SubtitleItem, SubtitleResponse, AudioAnalysisResponse, WordExtractionResponse

app = FastAPI(
    title="English Learning API",
    description="API for English learning applications including audio processing and subtitle generation",
    version="1.0.0"
)

# Initialize services
try:
    from .services.audio_processor import AudioProcessor
    audio_processor = AudioProcessor()
except ImportError:
    audio_processor = None

try:
    from .services.subtitle_processor import SubtitleProcessor
    subtitle_generator = SubtitleProcessor()
except ImportError:
    subtitle_generator = None


@app.get("/")
async def root():
    return {"message": "Welcome to English Learning API"}


@app.post("/transcribe-audio/", response_model=SubtitleResponse)
async def transcribe_audio(
    file: UploadFile = File(...),
    language: str = Form("en"),
    model_size: str = Form("small")
):
    """
    Transcribe audio file and generate subtitles
    """
    try:
        # Save uploaded file temporarily
        temp_filename = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_filename, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        # Process audio and generate subtitles
        result = subtitle_generator.generate_subtitles(
            temp_filename, language, model_size)

        # Clean up temporary file
        os.remove(temp_filename)

        return SubtitleResponse(
            success=True,
            subtitles=[SubtitleItem(**item) for item in result],
            message="Transcription completed successfully"
        )
    except Exception as e:
        return SubtitleResponse(
            success=False,
            subtitles=[],
            message=f"Error processing audio: {str(e)}"
        )


@app.post("/analyze-audio/", response_model=AudioAnalysisResponse)
async def analyze_audio(
    file: UploadFile = File(...)
):
    """
    Analyze audio file for English learning metrics
    """
    try:
        # Save uploaded file temporarily
        temp_filename = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_filename, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        # Analyze audio
        analysis = audio_processor.analyze_audio(temp_filename)

        # Clean up temporary file
        os.remove(temp_filename)

        return AudioAnalysisResponse(
            success=True,
            analysis=analysis,
            message="Audio analysis completed successfully"
        )
    except Exception as e:
        return AudioAnalysisResponse(
            success=False,
            analysis={},
            message=f"Error analyzing audio: {str(e)}"
        )


@app.get("/health")
async def health_check():
    return {"status": "healthy", "service": "English Learning API"}


@app.post("/upload-subtitle/", response_model=WordExtractionResponse)
async def upload_subtitle(
    file: UploadFile = File(...)
):
    """
    Upload subtitle file and extract keywords
    """
    try:
        if not subtitle_generator:
            return WordExtractionResponse(
                success=False,
                words=[],
                message="Subtitle generator service is not available"
            )
        
        # Save uploaded file temporarily
        temp_filename = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_filename, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        # Parse subtitle file
        subtitles = subtitle_generator.parse_subtitle_file(temp_filename)
        
        # Extract keywords
        keywords = subtitle_generator.extract_keywords(subtitles)

        # Clean up temporary file
        os.remove(temp_filename)

        return WordExtractionResponse(
            success=True,
            words=keywords,
            message="Subtitle processed successfully"
        )
    except Exception as e:
        return WordExtractionResponse(
            success=False,
            words=[],
            message=f"Error processing subtitle: {str(e)}"
        )


@app.post("/process-audio-with-subtitles/")
async def process_audio_with_subtitles(
    file: UploadFile = File(...),
    language: str = Form("en"),
    model_size: str = Form("small")
):
    """
    Process audio file, generate subtitles, and extract keywords
    """
    try:
        if not audio_processor:
            return {
                "success": False,
                "data": {},
                "message": "Audio processor service is not available"
            }
        
        # Save uploaded file temporarily
        temp_filename = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_filename, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        # Process audio with subtitles
        result = audio_processor.process_audio_with_subtitles(
            temp_filename, language, model_size
        )

        # Clean up temporary file
        os.remove(temp_filename)

        return {
            "success": True,
            "data": result,
            "message": "Audio processed successfully with subtitles"
        }
    except Exception as e:
        return {
            "success": False,
            "data": {},
            "message": f"Error processing audio: {str(e)}"
        }
