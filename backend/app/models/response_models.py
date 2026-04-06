from pydantic import BaseModel
from typing import List, Dict, Any, Optional


class SubtitleItem(BaseModel):
    id: int
    start: float
    end: float
    text: str
    confidence: float


class SubtitleResponse(BaseModel):
    success: bool
    subtitles: List[SubtitleItem]
    message: str


class AudioAnalysisResponse(BaseModel):
    success: bool
    analysis: Dict[str, Any]
    message: str


class WordExtractionResponse(BaseModel):
    success: bool
    words: List[str]
    message: str
