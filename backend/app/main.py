from fastapi import FastAPI, UploadFile, File, Form, Depends, HTTPException
from fastapi.responses import JSONResponse
from fastapi.security import OAuth2PasswordRequestForm, OAuth2PasswordBearer
from typing import Optional
import os
import tempfile
import subprocess
import uuid
from datetime import datetime, timedelta
from sqlalchemy.orm import Session

from .models.response_models import SubtitleItem, SubtitleResponse, AudioAnalysisResponse, WordExtractionResponse
from .models.auth_models import UserCreate, UserLogin, UserResponse, Token, WordSave
from .models.user_models import User, UserWord
from .core.database import get_db
from .core.auth import verify_password, get_password_hash, create_access_token, decode_token, get_current_user

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

# LangChain agent (optional)
try:
    from .services.langchain_agent import agent as langchain_agent
except ImportError:
    langchain_agent = None

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

    Expected JSON body: {"words": [str], "tone": str, "length": str}
    """
    try:
        from .models.langchain_models import StoryRequest
    except Exception:
        StoryRequest = None

    if not langchain_agent:
        return {"success": False, "text": "LangChain agent not available", "tokens_used": 0}

    # Normalize payload
    words = payload.get("words") if isinstance(payload, dict) else []
    tone = payload.get("tone", "fantastic") if isinstance(
        payload, dict) else "fantastic"
    length = payload.get("length", "short") if isinstance(
        payload, dict) else "short"

    result = await langchain_agent.generate_story(words, tone=tone, length=length)
    return {"success": True, "text": result.get("text"), "tokens_used": result.get("tokens_used", 0)}


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
