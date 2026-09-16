from .core.auth import verify_password, get_password_hash, create_access_token, decode_token, get_current_user
from .core.database import get_db
from .models.user_models import User, UserWord
from .models.auth_models import UserCreate, UserLogin, UserResponse, Token, WordSave
from .models.response_models import SubtitleItem, SubtitleResponse, AudioAnalysisResponse, WordExtractionResponse
from fastapi import FastAPI, UploadFile, File, Form, Depends, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from fastapi.security import OAuth2PasswordRequestForm, OAuth2PasswordBearer
from typing import Optional
import os
import tempfile
import subprocess
import uuid
import logging
from datetime import datetime, timedelta
from sqlalchemy.orm import Session

# Set up logging
logger = logging.getLogger(__name__)


app = FastAPI(
    title="English Learning API",
    description="API for English learning applications including audio processing and subtitle generation",
    version="1.0.0"
)

# CORS — allow the Vite dev server (and any origin in dev) to call the API.
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Dubbing Studio router (batch dubbing with progress tracking).
from .api.dubbing import router as dubbing_router  # noqa: E402
app.include_router(dubbing_router)

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

# LangChain agent (optional)
try:
    from .services.langchain_agent import agent as langchain_agent
    logger.info("LangChain agent imported successfully")
except ImportError as e:
    langchain_agent = None
    logger.error(f"Failed to import LangChain agent: {e}")

# CopilotKit LangGraph AG-UI endpoint (optional — requires copilotkit,
# ag-ui-langgraph, langgraph). Mounted at /copilotkit-agent so the
# agent-runtime (Bun) can proxy /copilotkit requests here.
try:
    from .api.copilotkit import copilotkit_app, copilotkit_router
    app.mount("/copilotkit-agent", copilotkit_app)
    app.include_router(copilotkit_router)
    logger.info("CopilotKit LangGraph agent mounted at /copilotkit-agent")
except ImportError as e:
    logger.warning(
        f"CopilotKit agent not available (install copilotkit, ag-ui-langgraph, "
        f"langgraph to enable): {e}"
    )

# Subtitle-to-Video generator (optional — requires edge-tts, pydub and ffmpeg
# on the host). Implements the "Generate & Time-Stretch" pipeline.
try:
    from .services.subtitle_video_generator import (
        subtitle_video_generator,
        DEFAULT_VOICE,
    )
    logger.info("Subtitle-to-Video generator loaded")
except ImportError as e:
    subtitle_video_generator = None
    logger.warning(
        f"Subtitle-to-Video generator not available "
        f"(install edge-tts, pydub, audiostretchy to enable): {e}"
    )

# Video dubber (optional — builds on subtitle_video_generator +
# audio_processor). Replaces a video's audio with a clean English voiceover.
try:
    from .services.video_dubber import video_dubber
    logger.info("Video dubber loaded")
except ImportError as e:
    video_dubber = None
    logger.warning(
        f"Video dubber not available (requires subtitle_video_generator and "
        f"audio_processor): {e}"
    )

# Natural dubbing pipeline (optional — the high-quality accent-replacement
# stack described in docs/NATURAL_ACCENT_DUBBING_DESIGN.md). Each component
# is loaded independently so the API can report which backends are available.
try:
    from .services.voice_converter import voice_converter
    logger.info("Voice converter loaded (backends: %s)",
                voice_converter.list_backends())
except ImportError as e:
    voice_converter = None
    logger.warning(f"Voice converter not available: {e}")

try:
    from .services.natural_tts import natural_tts
    logger.info("Natural TTS loaded (backends: %s)", natural_tts.list_backends())
except ImportError as e:
    natural_tts = None
    logger.warning(f"Natural TTS not available: {e}")

try:
    from .services.lip_syncer import lip_syncer
    logger.info("Lip syncer loaded (backends: %s)", lip_syncer.list_backends())
except ImportError as e:
    lip_syncer = None
    logger.warning(f"Lip syncer not available: {e}")

try:
    from .services.accent_detector import accent_detector
    logger.info("Accent detector loaded")
except ImportError as e:
    accent_detector = None
    logger.warning(f"Accent detector not available: {e}")

# OAuth2 password bearer
oauth2_scheme = OAuth2PasswordBearer(tokenUrl="/token")


# Protected route helper - validates token before allowing access
async def require_auth(token: str = Depends(oauth2_scheme), db: Session = Depends(get_db)):
    """Validate token and return current user or raise 401"""
    if not token:
        raise HTTPException(
            status_code=401,
            detail="Authorization required",
            headers={"WWW-Authenticate": "Bearer"},
        )

    payload = decode_token(token)
    if payload is None:
        raise HTTPException(
            status_code=401,
            detail="Invalid or expired token",
            headers={"WWW-Authenticate": "Bearer"},
        )

    user_id: int = payload.get("sub")
    if user_id is None:
        raise HTTPException(
            status_code=401,
            detail="Invalid token payload",
            headers={"WWW-Authenticate": "Bearer"},
        )

    user = db.query(User).filter(User.id == user_id).first()
    if user is None:
        raise HTTPException(
            status_code=401,
            detail="User not found",
            headers={"WWW-Authenticate": "Bearer"},
        )

    return user


@app.get("/")
async def root():
    return {"message": "Welcome to English Learning API"}


@app.post("/signup", response_model=UserResponse)
async def signup(user: UserCreate, db: Session = Depends(get_db)):
    """Sign up a new user"""
    # Check if user already exists
    existing_user = db.query(User).filter(
        (User.email == user.email) | (User.username == user.username)).first()
    if existing_user:
        raise HTTPException(
            status_code=400, detail="Email or username already registered")

    # Create new user
    hashed_password = get_password_hash(user.password)
    now = datetime.utcnow()
    new_user = User(
        username=user.username,
        email=user.email,
        password_hash=hashed_password,
        created_at=now,
        updated_at=now
    )
    db.add(new_user)
    db.commit()
    db.refresh(new_user)

    return new_user


@app.post("/token", response_model=Token)
async def login(form_data: OAuth2PasswordRequestForm = Depends(), db: Session = Depends(get_db)):
    """Login and get access token"""
    # Find user by email
    user = db.query(User).filter(User.email == form_data.username).first()
    if not user or not verify_password(form_data.password, user.password_hash):
        raise HTTPException(
            status_code=401,
            detail="Incorrect email or password",
            headers={"WWW-Authenticate": "Bearer"},
        )

    # Create access token
    access_token_expires = timedelta(minutes=30)
    access_token = create_access_token(
        data={"sub": str(user.id)}
    )

    return {"access_token": access_token, "token_type": "bearer"}


@app.post("/save-word")
async def save_word(word_data: WordSave, current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    """Save an unfamiliar word to the database"""
    # Check if word already exists for this user
    existing_word = db.query(UserWord).filter(
        UserWord.user_id == current_user.id,
        UserWord.word == word_data.word
    ).first()

    now = datetime.utcnow()
    if existing_word:
        # Update existing word
        existing_word.score = word_data.score
        existing_word.familiarity = word_data.familiarity
        existing_word.updated_at = now
    else:
        # Create new word entry
        new_word = UserWord(
            user_id=current_user.id,
            word=word_data.word,
            score=word_data.score,
            familiarity=word_data.familiarity,
            created_at=now,
            updated_at=now
        )
        db.add(new_word)

    db.commit()

    return {"success": True, "message": "Word saved successfully"}


# Reading sessions endpoints
@app.post("/reading_sessions")
async def create_reading_session(payload: dict, current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    """Create a reading session for the current user. Payload: {"content": str}"""
    content = payload.get("content", "") if isinstance(payload, dict) else ""
    now = datetime.utcnow()
    try:
        from .models.reading_models import ReadingSession
    except Exception:
        ReadingSession = None

    if ReadingSession:
        session = ReadingSession(
            user_id=current_user.id, content=content, started_at=now, finished_at=None)
        db.add(session)
        db.commit()
        db.refresh(session)
        return {"success": True, "reading_session_id": session.id}

    return {"success": False, "message": "ReadingSession model not available"}


@app.post("/reading_sessions/{session_id}/mark_word")
async def mark_word(session_id: int, body: dict, current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    """Mark a word as unfamiliar during a reading session. Body: {"word": str, "snippet": str}"""
    word = body.get("word") if isinstance(body, dict) else None
    snippet = body.get("snippet", "") if isinstance(body, dict) else ""
    now = datetime.utcnow()

    if not word:
        raise HTTPException(status_code=400, detail="word is required")

    # Create or update UserWord
    existing_word = db.query(UserWord).filter(
        UserWord.user_id == current_user.id, UserWord.word == word).first()
    if existing_word:
        existing_word.updated_at = now
    else:
        new_word = UserWord(
            user_id=current_user.id,
            word=word,
            score=0,
            familiarity="unknown",
            created_at=now,
            updated_at=now
        )
        db.add(new_word)

    # Optionally associate with reading session (if model available)
    try:
        from .models.reading_models import ReadingSession
        # Could add a relation table if desired
    except Exception:
        pass

    db.commit()
    return {"success": True, "word": word}


@app.get("/users/{user_id}/unfamiliar_words")
async def get_unfamiliar_words(user_id: int, current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    """Return unfamiliar words for a given user id (only accessible by the user themselves)"""
    # Only allow users to access their own data
    if current_user.id != user_id:
        raise HTTPException(
            status_code=403,
            detail="You don't have permission to access this resource"
        )
    words = db.query(UserWord).filter(UserWord.user_id == user_id).all()
    return {"success": True, "words": [{"id": w.id, "word": w.word, "familiarity": w.familiarity} for w in words]}


@app.get("/my-words")
async def get_my_words(current_user: User = Depends(require_auth), db: Session = Depends(get_db)):
    """Get all words saved by the current user"""
    words = db.query(UserWord).filter(
        UserWord.user_id == current_user.id).all()

    return {
        "success": True,
        "words": [
            {
                "id": word.id,
                "word": word.word,
                "score": word.score,
                "familiarity": word.familiarity
            }
            for word in words
        ]
    }


@app.get("/me")
async def get_current_user_info(current_user: User = Depends(require_auth)):
    """Get the current user's information"""
    return {
        "id": current_user.id,
        "username": current_user.username,
        "email": current_user.email
    }


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
        if not subtitle_generator:
            return SubtitleResponse(
                success=False,
                subtitles=[],
                message="Subtitle generator service is not available"
            )

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


@app.post("/video/translate")
async def translate_video_subtitles(
    file: UploadFile = File(...),
    source_language: str = Form("ja"),
    model_size: str = Form("base"),
    clip_duration: int = Form(600)
):
    """
    Generate English subtitles from a video in another language (e.g., Japanese).
    Clips video into segments and translates each segment.

    Args:
        file: Video file to process
        source_language: Source language code (default: "ja" for Japanese)
        model_size: Whisper model size (use non-English models like "base", "small")
        clip_duration: Duration of each clip in seconds (default: 600 = 10 minutes)
    """
    temp_video = None
    try:
        temp_video = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_video, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        if not audio_processor:
            return JSONResponse(
                status_code=503,
                content={"success": False,
                         "message": "Audio processor service is not available"}
            )

        result = audio_processor.generate_translated_subtitles_from_video(
            temp_video, source_language, model_size, clip_duration
        )

        if result.get("success"):
            return {
                "success": True,
                "subtitles": result.get("subtitles", []),
                "keywords": result.get("keywords", []),
                "video_duration": result.get("video_duration"),
                "segments_processed": result.get("segments_processed"),
                "source_language": result.get("source_language"),
                "clip_paths": result.get("clip_paths", []),
                "subtitle_paths": result.get("subtitle_paths", []),
                "merged_subtitle_path": result.get("merged_subtitle_path"),
                "clips_dir": result.get("clips_dir"),
                "subtitles_dir": result.get("subtitles_dir"),
                "message": "Translation completed successfully"
            }
        else:
            return JSONResponse(
                status_code=500,
                content={
                    "success": False,
                    "error": result.get("error", "Unknown error"),
                    "message": "Failed to translate video"
                }
            )
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False,
                     "message": f"Error processing video: {str(e)}"}
        )
    finally:
        if temp_video and os.path.exists(temp_video):
            try:
                os.remove(temp_video)
            except Exception:
                pass


@app.post("/video/subtitles")
async def generate_subtitles_from_video(
    file: UploadFile = File(...),
    language: str = Form("en"),
    model_size: str = Form("base.en")
):
    """
    Generate subtitles from a video file.
    Extracts audio, transcribes, and returns subtitles with timestamps.
    """
    temp_video = None
    try:
        temp_video = f"temp_{uuid.uuid4()}_{file.filename}"
        with open(temp_video, "wb") as buffer:
            content = await file.read()
            buffer.write(content)

        if not audio_processor:
            return JSONResponse(
                status_code=503,
                content={"success": False,
                         "message": "Audio processor service is not available"}
            )

        result = audio_processor.generate_subtitles_from_video(
            temp_video, language, model_size)

        if result.get("success"):
            return {
                "success": True,
                "subtitles": result.get("subtitles", []),
                "keywords": result.get("keywords", []),
                "audio_analysis": result.get("audio_analysis", {}),
                "message": "Subtitle generation completed successfully"
            }
        else:
            return JSONResponse(
                status_code=500,
                content={
                    "success": False,
                    "error": result.get("error", "Unknown error"),
                    "message": "Failed to generate subtitles"
                }
            )
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False,
                     "message": f"Error processing video: {str(e)}"}
        )
    finally:
        if temp_video and os.path.exists(temp_video):
            try:
                os.remove(temp_video)
            except Exception:
                pass


@app.post("/video/dub")
async def dub_video(
    file: UploadFile = File(...),
    source_language: str = Form("en"),
    translate: bool = Form(False),
    whisper_model: str = Form("base"),
    voice: str = Form(DEFAULT_VOICE),
    rate: str = Form("+0%"),
    volume: str = Form("+0%"),
    pitch: str = Form("+0Hz"),
    burn_subtitles: bool = Form(False),
    keep_original_audio: bool = Form(False),
    original_audio_volume: int = Form(0),
    clip_duration: int = Form(600),
):
    """
    Replace a video's audio with a clean English voiceover.

    Pipeline ("separate video & audio, regenerate audio, merge"):

      1. Extract audio from the uploaded video (PyAV / ffmpeg).
      2. Transcribe the audio to subtitles with Whisper. If ``translate`` is
         True, Whisper's translate task is used so non-English audio is
         converted directly to English subtitles. For heavy-accent English,
         leave ``translate=False`` to re-speak the same words cleanly.
      3. Synthesise TTS for each subtitle block and pitch-preserve
         time-stretch it to fit the exact subtitle slot (edge-tts +
         audiostretchy/atempo).
      4. Stitch the per-block audio into a single track whose timeline
         matches the original video duration.
      5. Mux the dubbed audio back onto the original video with ffmpeg,
         replacing the original audio track (or mixing it under at reduced
         volume if ``keep_original_audio`` is True).

    Returns the path to the produced dubbed video plus per-cue statistics.
    """
    if not video_dubber:
        return JSONResponse(
            status_code=503,
            content={
                "success": False,
                "message": (
                    "Video dubber is not available. Install edge-tts, pydub, "
                    "audiostretchy, faster-whisper and ensure ffmpeg is on PATH."
                ),
            },
        )

    temp_video = None
    try:
        suffix = os.path.splitext(file.filename or "")[1] or ".mp4"
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_video = tmp.name

        # Write outputs into a stable subdir of the backend so the download
        # endpoint (which restricts to the backend work tree) can serve them.
        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        output_dir = os.path.join(base_dir, "outputs", "dubbed_videos")
        os.makedirs(output_dir, exist_ok=True)

        result = video_dubber.dub(
            video_path=temp_video,
            output_dir=output_dir,
            source_language=source_language,
            translate=translate,
            whisper_model=whisper_model,
            voice=voice,
            rate=rate,
            volume=volume,
            pitch=pitch,
            burn_subtitles=burn_subtitles,
            keep_original_audio=keep_original_audio,
            original_audio_volume=original_audio_volume,
            clip_duration=clip_duration,
        )

        if not result.success:
            return JSONResponse(
                status_code=500,
                content={"success": False, "message": result.error},
            )

        return {
            "success": True,
            "video_path": result.video_path,
            "audio_path": result.audio_path,
            "subtitle_path": result.subtitle_path,
            "stats": result.stats,
            "cues": result.cues,
            "message": "Video dubbed successfully",
        }
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False, "message": f"Error dubbing video: {e}"},
        )
    finally:
        if temp_video and os.path.exists(temp_video):
            try:
                os.remove(temp_video)
            except Exception:
                pass


@app.post("/video/dub-from-subtitle")
async def dub_video_from_subtitle(
    file: UploadFile = File(...),
    subtitle: UploadFile = File(...),
    voice: str = Form(DEFAULT_VOICE),
    rate: str = Form("+0%"),
    volume: str = Form("+0%"),
    pitch: str = Form("+0Hz"),
    burn_subtitles: bool = Form(False),
    keep_original_audio: bool = Form(False),
    original_audio_volume: int = Form(0),
):
    """
    Dub a silent (or already-separated) video using a ready subtitle file.

    Third dubbing mode — skips Whisper transcription entirely. Use when:
      * the source video is already silent (audio stripped), or
      * you have a corrected/translated .srt you'd rather use than the
        auto-transcribed one.

    Pipeline (reuses existing services, no duplicated logic):
      1. Read the video duration so the audio timeline lines up.
      2. Run the "Generate & Time-Stretch" pipeline on the uploaded .srt /
         .vtt to produce a single dubbed_audio.mp3
         (SubtitleVideoGenerator.generate_audio_only).
      3. Mux the dubbed audio onto the source video with ffmpeg
         (VideoDubber._mux — same helper as /video/dub).

    Returns the path to the produced dubbed video plus per-cue statistics.
    """
    if not video_dubber:
        return JSONResponse(
            status_code=503,
            content={
                "success": False,
                "message": (
                    "Video dubber is not available. Install edge-tts, pydub, "
                    "audiostretchy and ensure ffmpeg is on PATH."
                ),
            },
        )

    temp_video = None
    temp_sub = None
    try:
        v_suffix = os.path.splitext(file.filename or "")[1] or ".mp4"
        with tempfile.NamedTemporaryFile(suffix=v_suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_video = tmp.name

        s_suffix = os.path.splitext(subtitle.filename or "")[1] or ".srt"
        with tempfile.NamedTemporaryFile(suffix=s_suffix, delete=False) as tmp:
            tmp.write(await subtitle.read())
            temp_sub = tmp.name

        # Write outputs into a stable subdir of the backend so the download
        # endpoint (which restricts to the backend work tree) can serve them.
        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        output_dir = os.path.join(base_dir, "outputs", "dubbed_videos")
        os.makedirs(output_dir, exist_ok=True)

        result = video_dubber.dub_from_subtitle(
            video_path=temp_video,
            subtitle_path=temp_sub,
            output_dir=output_dir,
            voice=voice,
            rate=rate,
            volume=volume,
            pitch=pitch,
            burn_subtitles=burn_subtitles,
            keep_original_audio=keep_original_audio,
            original_audio_volume=original_audio_volume,
        )

        if not result.success:
            return JSONResponse(
                status_code=500,
                content={"success": False, "message": result.error},
            )

        return {
            "success": True,
            "video_path": result.video_path,
            "audio_path": result.audio_path,
            "subtitle_path": result.subtitle_path,
            "stats": result.stats,
            "cues": result.cues,
            "message": "Video dubbed successfully",
        }
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False, "message": f"Error dubbing video: {e}"},
        )
    finally:
        if temp_video and os.path.exists(temp_video):
            try:
                os.remove(temp_video)
            except Exception:
                pass
        if temp_sub and os.path.exists(temp_sub):
            try:
                os.remove(temp_sub)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# Natural-quality dubbing pipeline (new — see docs/NATURAL_ACCENT_DUBBING_DESIGN.md)
# ---------------------------------------------------------------------------

@app.post("/video/dub-natural")
async def dub_video_natural(
    file: UploadFile = File(...),
    reference_voice: Optional[UploadFile] = File(None),
    target_accent: str = Form("london"),
    preserve_speaker: bool = Form(True),
    enable_lip_sync: bool = Form(False),
    force_path: Optional[str] = Form(None),
    whisper_model: str = Form("base"),
    translate: bool = Form(False),
    keep_original_audio: bool = Form(False),
    original_audio_volume: int = Form(0),
    burn_subtitles: bool = Form(False),
    tts_speed: float = Form(1.0),
    concurrency: int = Form(4),
):
    """Natural-quality accent replacement (dual-path pipeline).

    Automatically picks the best path:

      * **Path A** (default, ``preserve_speaker=True``) — Voice Conversion.
        Converts the bad-accent waveform into a clear target-accent voice
        while preserving prosody, timing, and emotion. No time-stretch
        needed. Best for English-with-bad-accent source.

      * **Path B** (``preserve_speaker=False`` or non-English source) —
        TTS with voice cloning (F5-TTS / OpenVoice v2 / CosyVoice 2).

      * **Path C** (fallback) — legacy edge-tts + time-stretch. Used when
        no ML backend is installed.

    Parameters
    ----------
    target_accent : "london" | "american" | "indian" | "australian"
    reference_voice : optional 5–15 s wav of the desired target voice
    preserve_speaker : True → prefer Path A (keep speaker's cadence)
    enable_lip_sync : True → run VideoReTalking after audio replacement
    force_path : "A" | "B" | "C" to override automatic selection
    """
    if not video_dubber:
        return JSONResponse(status_code=503, content={
            "success": False,
            "message": "Video dubber is not available.",
        })

    temp_video = None
    temp_ref = None
    try:
        suffix = os.path.splitext(file.filename or "")[1] or ".mp4"
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_video = tmp.name

        ref_path = None
        if reference_voice is not None and reference_voice.filename:
            r_suffix = os.path.splitext(reference_voice.filename)[1] or ".wav"
            with tempfile.NamedTemporaryFile(suffix=r_suffix, delete=False) as tmp:
                tmp.write(await reference_voice.read())
                temp_ref = tmp.name
                ref_path = temp_ref

        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        output_dir = os.path.join(base_dir, "outputs", "dubbed_videos")
        os.makedirs(output_dir, exist_ok=True)

        result = video_dubber.dub_natural(
            video_path=temp_video,
            output_dir=output_dir,
            target_accent=target_accent,
            reference_voice=ref_path,
            preserve_speaker=preserve_speaker,
            enable_lip_sync=enable_lip_sync,
            force_path=force_path,
            whisper_model=whisper_model,
            translate=translate,
            keep_original_audio=keep_original_audio,
            original_audio_volume=original_audio_volume,
            burn_subtitles=burn_subtitles,
            tts_speed=tts_speed,
            concurrency=concurrency,
        )

        if not result.success:
            return JSONResponse(status_code=500,
                                content={"success": False, "message": result.error})

        return {
            "success": True,
            "video_path": result.video_path,
            "audio_path": result.audio_path,
            "subtitle_path": result.subtitle_path,
            "stats": result.stats,
            "cues": result.cues,
            "message": "Natural dubbing completed",
        }
    except Exception as e:
        return JSONResponse(status_code=500,
                            content={"success": False,
                                     "message": f"Error: {e}"})
    finally:
        if temp_video and os.path.exists(temp_video):
            try: os.remove(temp_video)
            except Exception: pass
        if temp_ref and os.path.exists(temp_ref):
            try: os.remove(temp_ref)
            except Exception: pass


@app.post("/video/convert-voice")
async def convert_voice(
    file: UploadFile = File(...),
    target_accent: str = Form("london"),
):
    """Standalone voice conversion (Path A only, no video mux).

    Converts an uploaded audio file into the target-accent voice using RVC /
    kNN-VC. Output duration matches input duration. Useful as a standalone
    accent-correction tool.
    """
    if not voice_converter:
        return JSONResponse(status_code=503, content={
            "success": False,
            "message": "Voice converter not available.",
        })

    temp_audio = None
    try:
        suffix = os.path.splitext(file.filename or "")[1] or ".wav"
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_audio = tmp.name

        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        output_dir = os.path.join(base_dir, "outputs", "converted_voice")
        os.makedirs(output_dir, exist_ok=True)

        out_path = os.path.join(
            output_dir,
            f"converted_{uuid.uuid4().hex[:8]}.wav")

        from .services.voice_converter import VoiceConversionConfig
        result = voice_converter.convert(
            source_wav_path=temp_audio,
            output_wav_path=out_path,
            config=VoiceConversionConfig(),
        )

        if not result.success:
            return JSONResponse(status_code=500,
                                content={"success": False, "message": result.error})

        return {
            "success": True,
            "audio_path": result.audio_path,
            "duration": result.duration,
            "backend": result.backend,
            "stats": result.stats,
            "available_backends": voice_converter.list_backends(),
        }
    except Exception as e:
        return JSONResponse(status_code=500,
                            content={"success": False, "message": str(e)})
    finally:
        if temp_audio and os.path.exists(temp_audio):
            try: os.remove(temp_audio)
            except Exception: pass


@app.post("/video/synthesize")
async def synthesize_speech(
    text: str = Form(...),
    reference_voice: Optional[UploadFile] = File(None),
    target_accent: str = Form("london"),
    language: str = Form("en"),
    speed: float = Form(1.0),
):
    """Standalone natural TTS synthesis (Path B only).

    Synthesises the given text in a cloned target-accent voice using F5-TTS /
    OpenVoice v2 / CosyVoice 2 / Edge-TTS (auto-selected best available).
    """
    if not natural_tts:
        return JSONResponse(status_code=503, content={
            "success": False,
            "message": "Natural TTS not available.",
        })

    temp_ref = None
    try:
        ref_path = None
        if reference_voice is not None and reference_voice.filename:
            r_suffix = os.path.splitext(reference_voice.filename)[1] or ".wav"
            with tempfile.NamedTemporaryFile(suffix=r_suffix, delete=False) as tmp:
                tmp.write(await reference_voice.read())
                temp_ref = tmp.name
                ref_path = temp_ref
        else:
            # Use the pre-bundled default reference voice.
            base = os.path.join(os.path.dirname(os.path.dirname(__file__)),
                                "assets", "reference_voices")
            for cand in (f"{target_accent}.wav", f"{target_accent}_default.wav",
                         "london_default.wav"):
                p = os.path.join(base, cand)
                if os.path.exists(p):
                    ref_path = p
                    break

        if not ref_path:
            return JSONResponse(status_code=400, content={
                "success": False,
                "message": (f"No reference voice found for accent '{target_accent}'. "
                            f"Upload one or place a default at "
                            f"app/assets/reference_voices/{target_accent}.wav"),
            })

        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        out_path = os.path.join(base_dir, "outputs", "synthesized",
                                f"tts_{uuid.uuid4().hex[:8]}.wav")
        os.makedirs(os.path.dirname(out_path), exist_ok=True)

        from .services.natural_tts import SynthesisRequest
        result = natural_tts.synthesize(SynthesisRequest(
            text=text,
            reference_audio=ref_path,
            output_path=out_path,
            language=language,
            speed=speed,
            accent=target_accent,
        ))

        if not result.success:
            return JSONResponse(status_code=500,
                                content={"success": False, "message": result.error})

        return {
            "success": True,
            "audio_path": result.audio_path,
            "duration": result.duration,
            "backend": result.backend,
            "available_backends": natural_tts.list_backends(),
        }
    except Exception as e:
        return JSONResponse(status_code=500,
                            content={"success": False, "message": str(e)})
    finally:
        if temp_ref and os.path.exists(temp_ref):
            try: os.remove(temp_ref)
            except Exception: pass


@app.post("/video/lip-sync")
async def lip_sync_video(
    file: UploadFile = File(...),
    audio: UploadFile = File(...),
    face_enhancer: str = Form("gfpgan"),
):
    """Standalone lip synchronization (optional stage 5).

    Re-syncs the lip movements in a talking-head video to match a new audio
    track using VideoReTalking / Wav2Lip. Skips automatically if no face is
    detected.
    """
    if not lip_syncer:
        return JSONResponse(status_code=503, content={
            "success": False,
            "message": "Lip syncer not available.",
        })

    temp_video = None
    temp_audio = None
    try:
        v_suffix = os.path.splitext(file.filename or "")[1] or ".mp4"
        with tempfile.NamedTemporaryFile(suffix=v_suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_video = tmp.name

        a_suffix = os.path.splitext(audio.filename or "")[1] or ".wav"
        with tempfile.NamedTemporaryFile(suffix=a_suffix, delete=False) as tmp:
            tmp.write(await audio.read())
            temp_audio = tmp.name

        base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
        out_path = os.path.join(base_dir, "outputs", "lipsynced",
                               f"lipsynced_{uuid.uuid4().hex[:8]}.mp4")
        os.makedirs(os.path.dirname(out_path), exist_ok=True)

        from .services.lip_syncer import LipSyncConfig
        result = lip_syncer.sync(
            video_path=temp_video,
            audio_path=temp_audio,
            output_path=out_path,
            config=LipSyncConfig(face_enhancer=face_enhancer),
        )

        if not result.success:
            return JSONResponse(status_code=500,
                                content={"success": False, "message": result.error})

        return {
            "success": True,
            "video_path": result.video_path,
            "faces_detected": result.faces_detected,
            "backend": result.backend,
            "stats": result.stats,
            "available_backends": lip_syncer.list_backends(),
        }
    except Exception as e:
        return JSONResponse(status_code=500,
                            content={"success": False, "message": str(e)})
    finally:
        for f in (temp_video, temp_audio):
            if f and os.path.exists(f):
                try: os.remove(f)
                except Exception: pass


@app.get("/dubbing/backends")
async def list_dubbing_backends():
    """Report which dubbing backends are available on this server.

    Useful for the frontend to decide which UI options to show (e.g. hide
    the "lip sync" toggle if no lip-sync backend is installed).
    """
    return {
        "voice_conversion": voice_converter.list_backends() if voice_converter else {},
        "tts": natural_tts.list_backends() if natural_tts else {},
        "lip_sync": lip_syncer.list_backends() if lip_syncer else {},
        "accent_detection": accent_detector is not None,
    }


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
        if not audio_processor:
            return AudioAnalysisResponse(
                success=False,
                analysis={},
                message="Audio processor service is not available"
            )

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


@app.post("/stories")
async def generate_story_endpoint(payload: dict):
    """Endpoint to generate a story from provided words.

    Expected JSON body: {"words": [str], "tone": str, "length": str, "max_length": int}
    """
    # Log the request
    logger.info(f"Received story generation request: {payload}")
    logger.info(f"LangChain agent is available: {langchain_agent is not None}")

    try:
        from .models.langchain_models import StoryRequest
    except Exception as e:
        StoryRequest = None
        logger.error(f"Failed to import StoryRequest: {e}")

    if not langchain_agent:
        # Mock story generation if LangChain agent is not available
        logger.warning(
            "Using mock story generation (LangChain agent not available)")
        words = payload.get("words", []) if isinstance(payload, dict) else []
        tone = payload.get("tone", "fantastic") if isinstance(
            payload, dict) else "fantastic"
        length = payload.get("length", "short") if isinstance(
            payload, dict) else "short"
        max_length = payload.get("max_length", 500) if isinstance(
            payload, dict) else 500

        mock_story = f"Once upon a time, there was a {tone} adventure with {', '.join(words[:3])} and other amazing things. It was a {length} but wonderful story!"
        if len(mock_story) > max_length:
            mock_story = mock_story[:max_length].rsplit(' ', 1)[0] + '...'
        logger.info(f"Generated mock story: {mock_story}")
        return {"success": True, "text": mock_story, "tokens_used": 0}

    # Normalize payload
    words = payload.get("words") if isinstance(payload, dict) else []
    tone = payload.get("tone", "fantastic") if isinstance(
        payload, dict) else "fantastic"
    length = payload.get("length", "short") if isinstance(
        payload, dict) else "short"
    max_length = payload.get("max_length", 500) if isinstance(
        payload, dict) else 500

    logger.info(
        f"Generating story with LangChain agent: words={words}, tone={tone}, length={length}")
    result = await langchain_agent.generate_story(words, tone=tone, length=length)
    story_text = result.get("text", "")

    # Ensure the story doesn't exceed the max length
    if len(story_text) > max_length:
        story_text = story_text[:max_length].rsplit(' ', 1)[0] + '...'

    logger.info(f"Generated story with LangChain agent: {story_text}")
    return {"success": True, "text": story_text, "tokens_used": result.get("tokens_used", 0)}


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


@app.post("/subtitle-to-video")
async def subtitle_to_video(
    file: UploadFile = File(...),
    voice: str = Form(DEFAULT_VOICE),
    rate: str = Form("+0%"),
    volume: str = Form("+0%"),
    pitch: str = Form("+0Hz"),
    width: int = Form(1280),
    height: int = Form(720),
    fps: int = Form(30),
    bg_color: str = Form("0x0F172A"),
    background_video: Optional[UploadFile] = File(None),
):
    """
    Generate a dubbed video from a subtitle file (.srt / .vtt).

    Pipeline ("Generate & Time-Stretch"):
      1. Parse the subtitle file to get text + exact start/end times.
      2. Synthesise TTS for each cue (Microsoft Edge TTS — no API key needed).
      3. Pitch-preserving time-stretch so each block fits its slot
         (audiostretchy, with ffmpeg atempo fallback; ratio clamped to
         0.75x–1.5x for natural speech).
      4. Stitch blocks together with sample-accurate silence.
      5. Render the final video with burned-in subtitles over a coloured
         background (or over an optional background video).

    Optional ``background_video`` is muxed under the dubbed audio. If omitted,
    a solid coloured video is generated automatically.

    Returns the path to the produced video/audio and per-cue statistics.
    """
    if not subtitle_video_generator:
        return JSONResponse(
            status_code=503,
            content={
                "success": False,
                "message": (
                    "Subtitle-to-Video generator is not available. "
                    "Install edge-tts, pydub, audiostretchy and ensure ffmpeg "
                    "is on PATH to enable."
                ),
            },
        )

    temp_sub = None
    temp_bg = None
    try:
        # Save uploaded subtitle to a temp file (preserve extension for parser).
        suffix = os.path.splitext(file.filename or "")[1] or ".srt"
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
            tmp.write(await file.read())
            temp_sub = tmp.name

        # Optional background video.
        bg_path = None
        if background_video is not None and background_video.filename:
            bg_suffix = os.path.splitext(background_video.filename)[1] or ".mp4"
            with tempfile.NamedTemporaryFile(suffix=bg_suffix, delete=False) as tmp:
                tmp.write(await background_video.read())
                temp_bg = tmp.name
                bg_path = temp_bg

        result = subtitle_video_generator.generate(
            subtitle_path=temp_sub,
            voice=voice,
            rate=rate,
            volume=volume,
            pitch=pitch,
            background_video=bg_path,
            width=width,
            height=height,
            fps=fps,
            bg_color=bg_color,
        )

        if not result.success:
            return JSONResponse(
                status_code=500,
                content={"success": False, "message": result.error},
            )

        return {
            "success": True,
            "video_path": result.video_path,
            "audio_path": result.audio_path,
            "subtitle_path": result.subtitle_path,
            "stats": result.stats,
            "cues": result.cues,
            "message": "Video generated successfully",
        }
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False, "message": f"Error generating video: {e}"},
        )
    finally:
        if temp_sub and os.path.exists(temp_sub):
            try:
                os.remove(temp_sub)
            except Exception:
                pass
        if temp_bg and os.path.exists(temp_bg):
            try:
                os.remove(temp_bg)
            except Exception:
                pass


@app.get("/subtitle-to-video/download")
async def download_generated_video(path: str):
    """Stream a previously generated video file back to the client."""
    if not path or not os.path.exists(path):
        raise HTTPException(status_code=404, detail="File not found")
    # Basic safety: only allow files inside the backend work dir.
    abs_path = os.path.abspath(path)
    base_dir = os.path.abspath(os.path.dirname(os.path.dirname(__file__)))
    if not abs_path.startswith(base_dir):
        raise HTTPException(status_code=403, detail="Access denied")

    from fastapi.responses import FileResponse
    return FileResponse(
        abs_path,
        media_type="video/mp4",
        filename=os.path.basename(abs_path),
    )


@app.get("/subtitle-to-video/voices")
async def list_tts_voices(locale: Optional[str] = None):
    """List Microsoft Edge TTS voices, optionally filtered by locale."""
    try:
        import edge_tts
    except ImportError:
        return JSONResponse(
            status_code=503,
            content={"success": False, "message": "edge-tts is not installed"},
        )

    try:
        voices = await edge_tts.list_voices()
        if locale:
            locale = locale.lower()
            voices = [v for v in voices if v.get("Locale", "").lower() == locale]
        return {
            "success": True,
            "voices": [
                {"name": v.get("ShortName"), "locale": v.get("Locale"),
                 "gender": v.get("Gender")}
                for v in voices
            ],
        }
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"success": False, "message": str(e)},
        )


@app.post("/process-audio-with-subtitles/")
async def process_audio_with_subtitles(
    file: UploadFile = File(...),
    language: str = Form("en"),
    model_size: str = Form("tiny.en")
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


@app.on_event("startup")
async def startup_event():
    """Initialize agents on application startup."""
    logger.info(f"LangChain agent is: {langchain_agent}")
    if langchain_agent:
        try:
            await langchain_agent.start()
            logger.info("LangChain agent initialized on startup")
        except Exception as e:
            logger.error(f"Failed to initialize LangChain agent: {e}")


@app.on_event("shutdown")
async def shutdown_event():
    """Cleanup agents on application shutdown."""
    if langchain_agent:
        try:
            await langchain_agent.stop()
            logger.info("LangChain agent stopped")
        except Exception as e:
            logger.error(f"Error stopping LangChain agent: {e}")
