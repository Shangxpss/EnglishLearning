import asyncio
from backend.app.services.langchain_agent import LangChainAgent

async def _async_test():
    agent = LangChainAgent()
    result = await agent.generate_story(["apple", "banana"], tone="whimsical", length="short")
    assert "apple" in result["text"]

def test_generate_story():
    asyncio.run(_async_test())
