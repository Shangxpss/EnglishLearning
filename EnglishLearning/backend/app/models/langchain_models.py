from pydantic import BaseModel
from typing import List

class StoryRequest(BaseModel):
    words: List[str]
    tone: str = "fantastic"
    length: str = "short"

class StoryResponse(BaseModel):
    success: bool
    text: str
    tokens_used: int = 0
