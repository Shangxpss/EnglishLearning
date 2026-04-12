import os
import logging
import random
from typing import List, Dict, Any
from langchain_openai import ChatOpenAI
from langchain_core.prompts import ChatPromptTemplate
from langchain_core.output_parsers import StrOutputParser
from app.core.config import settings
from .langchain_prompts import build_story_prompt

# Updated to fix import issue

logger = logging.getLogger(__name__)


class LangChainAgent:
    """Wrapper for LangChain-based story generation using DeepSeek API.

    This agent uses LangChain orchestration with DeepSeek's OpenAI-compatible API
    to generate creative stories from unfamiliar words.
    """

    def __init__(self):
        self.llm = None
        self.initialized = False

    async def start(self) -> None:
        """Initialize the LLM client with DeepSeek API configuration."""
        api_key = settings.DEEPSEEK_API_KEY or os.getenv("DEEPSEEK_API_KEY")
        base_url = settings.DEEPSEEK_BASE_URL or os.getenv(
            "DEEPSEEK_BASE_URL", "https://api.deepseek.com")

        if not api_key:
            logger.warning(
                "DEEPSEEK_API_KEY not configured. Story generation will use mocked responses.")
            self.initialized = False
            return

        try:
            self.llm = ChatOpenAI(
                model="deepseek-chat",
                api_key=api_key,
                base_url=base_url,
                temperature=0.7,
                max_completion_tokens=2000,
            )
            self.initialized = True
            logger.info("LangChainAgent initialized with DeepSeek API")
        except Exception as e:
            logger.error(f"Failed to initialize LangChainAgent: {e}")
            self.initialized = False

    async def stop(self) -> None:
        """Cleanup resources."""
        self.llm = None
        self.initialized = False
        logger.info("LangChainAgent stopped")

    async def generate_story(
        self,
        words: List[str],
        tone: str = "fantastic",
        length: str = "short"
    ) -> Dict[str, Any]:
        """Generate a story containing the given words using DeepSeek API.

        Args:
            words: List of unfamiliar words to include in the story
            tone: Story tone (fantastic, educational, humorous, etc.)
            length: Story length (short, medium, long)

        Returns:
            Dict with 'text' (story content) and 'tokens_used' (approximate token count)
        """
        # Build prompt using templates
        prompt_text = build_story_prompt(words, tone=tone, length=length)

        # If not initialized (no API key), return mocked response
        if not self.initialized or not self.llm:
            logger.warning(
                "Using mocked story generation (API not configured)")
            story = self._generate_mocked_story(words, tone, length)
            return {"text": story, "tokens_used": 0}

        try:
            # Create LangChain prompt template
            prompt = ChatPromptTemplate.from_messages([
                ("system", "You are a creative writing assistant. Generate engaging, educational stories for English learners."),
                ("user", "{prompt}")
            ])

            # Create the chain
            chain = prompt | self.llm | StrOutputParser()

            # Invoke the chain
            story = await chain.ainvoke({"prompt": prompt_text})

            # Estimate token count (rough approximation)
            tokens_used = len(story.split()) * 1.3  # Rough estimate

            logger.info(
                f"Generated story with {len(words)} words, tone={tone}, length={length}")
            return {"text": story, "tokens_used": int(tokens_used)}

        except Exception as e:
            logger.error(f"Error generating story: {e}")
            # Fallback to mocked story on error
            story = self._generate_mocked_story(words, tone, length)
            return {"text": story, "tokens_used": 0}

    def _generate_mocked_story(self, words: List[str], tone: str, length: str) -> str:
        """Generate a mocked story for testing/fallback purposes."""
        word_list = ", ".join(words[:5]) if words else "interesting words"

        # Different story templates to create variety
        story_templates = [
            f"""Once upon a time, there was a curious learner who encountered some fascinating words: {word_list}.

In a {tone} adventure, these words came to life and taught valuable lessons about language and creativity. 
The story unfolded with wonder and discovery, helping the learner understand each word in context.

[Note: This is a mocked story. Configure DEEPSEEK_API_KEY to enable real AI-generated stories.]

The end.""",
            f"""In a far-off land, a young explorer stumbled upon a magical book containing the words: {word_list}.

Each word had a special power, and as the explorer learned their meanings, they embarked on a {tone} journey filled with excitement and learning.

The explorer discovered that understanding these words opened up new worlds of possibility.

[Note: This is a mocked story. Configure DEEPSEEK_API_KEY to enable real AI-generated stories.]

The end.""",
            f"""Once upon a time, in a village of words, {word_list} were the most mysterious and powerful of all.

A brave young linguist set out to understand their secrets, and along the way, they experienced a {tone} adventure that changed their life.

Through challenges and triumphs, the linguist learned the true power of language.

[Note: This is a mocked story. Configure DEEPSEEK_API_KEY to enable real AI-generated stories.]

The end."""
        ]

        # Select a random story template
        selected_template = random.choice(story_templates)
        return selected_template


# Singleton instance for simple import from main
agent = LangChainAgent()


def get_agent() -> LangChainAgent:
    """Factory function to get the LangChainAgent singleton."""
    return agent
