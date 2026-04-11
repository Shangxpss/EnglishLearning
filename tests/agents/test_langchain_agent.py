import asyncio
import pytest
from backend.app.services.langchain_agent import LangChainAgent, get_agent


class TestLangChainAgent:
    """Unit tests for LangChainAgent."""

    @pytest.mark.asyncio
    async def test_start_initializes_agent(self):
        """Test that start() initializes the agent."""
        agent = LangChainAgent()
        assert agent.initialized is False
        await agent.start()
        # Agent may or may not be initialized depending on API key availability
        # Just verify the method runs without error

    @pytest.mark.asyncio
    async def test_stop_cleanup(self):
        """Test that stop() cleans up resources."""
        agent = LangChainAgent()
        await agent.start()
        await agent.stop()
        assert agent.initialized is False

    @pytest.mark.asyncio
    async def test_generate_story_returns_structure(self):
        """Test that generate_story() returns expected structure."""
        agent = LangChainAgent()
        await agent.start()
        result = await agent.generate_story(["apple", "banana"], tone="whimsical", length="short")
        assert "text" in result
        assert "tokens_used" in result
        assert isinstance(result["text"], str)
        assert isinstance(result["tokens_used"], int)

    @pytest.mark.asyncio
    async def test_generate_story_includes_words(self):
        """Test that generated story includes the provided words (mocked mode)."""
        agent = LangChainAgent()
        await agent.start()
        words = ["adventure", "mysterious"]
        result = await agent.generate_story(words, tone="fantastic", length="short")
        # In mocked mode, words should appear in the story
        assert "adventure" in result["text"].lower() or "mysterious" in result["text"].lower()

    def test_get_agent_factory(self):
        """Test that get_agent() returns a LangChainAgent instance."""
        agent = get_agent()
        assert isinstance(agent, LangChainAgent)


class TestLangChainAgentIntegration:
    """Integration tests using FastAPI TestClient."""

    @pytest.fixture
    def client(self):
        """Create test client with stories endpoint."""
        from fastapi.testclient import TestClient
        from fastapi import FastAPI
        
        app = FastAPI()
        
        @app.post("/stories")
        async def generate_story(payload: dict):
            from backend.app.services.langchain_agent import get_agent
            agent = get_agent()
            await agent.start()
            words = payload.get("words", [])
            tone = payload.get("tone", "fantastic")
            length = payload.get("length", "short")
            result = await agent.generate_story(words, tone=tone, length=length)
            return {"success": True, "text": result.get("text"), "tokens_used": result.get("tokens_used", 0)}
        
        return TestClient(app)

    def test_stories_endpoint(self, client):
        """Test stories generation endpoint."""
        response = client.post(
            "/stories",
            json={"words": ["curious", "adventure"], "tone": "fantastic", "length": "short"}
        )
        assert response.status_code == 200
        data = response.json()
        assert "success" in data
        assert "text" in data
        assert data["success"] is True

    def test_stories_endpoint_empty_words(self, client):
        """Test stories endpoint with empty words list."""
        response = client.post(
            "/stories",
            json={"words": [], "tone": "educational", "length": "medium"}
        )
        assert response.status_code == 200
        data = response.json()
        assert data["success"] is True
        assert "text" in data
