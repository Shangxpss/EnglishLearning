from typing import List, Dict, Any
from backend.app.core.logger import get_logger
from .langchain_prompts import build_story_prompt

logger = get_logger(__name__)

class LangChainAgent:
    """Wrapper for LangChain-based story generation.

    Replace the placeholder generate_story implementation with a real LangChain
    orchestration that uses prompt templates and client bindings.
    """
    def __init__(self):
        # Initialize LangChain client or config here (placeholder)
        # Example: read OPENAI_API_KEY from env and configure client
        self.client = None

    async def start(self) -> None:
        logger.info("LangChainAgent started")

    async def stop(self) -> None:
        logger.info("LangChainAgent stopped")

    async def generate_story(self, words: List[str], tone: str = "fantastic", length: str = "short") -> Dict[str, Any]:
        """Generate a story containing the given words.

        Returns a dict: {"text": str, "tokens_used": int}
        """
        # Build prompt using templates
        prompt = build_story_prompt(words, tone=tone, length=length)

        # Placeholder: when LangChain/OpenAI integration is added, send the prompt to the model.
        # For now, return a simple mocked story so front-end integration can be developed and tested.
        story = f"[MOCKED STORY based on prompt]\n{prompt}"
        return {"text": story, "tokens_used": 0}

# Singleton instance for simple import from main
agent = LangChainAgent()

def get_agent():
    return agent
