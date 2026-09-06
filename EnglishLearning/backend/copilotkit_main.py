"""Standalone entry point for the EnglishPro CopilotKit LangGraph agent.

This mirrors AI-Demo/backend/main.py and lets you run the CopilotKit agent
in isolation (without the rest of the English Learning API) for debugging:

    cd backend
    uv run python copilotkit_main.py

The agent is served at http://localhost:8001/ and is intended to be
proxied by the agent-runtime (Bun) which exposes /copilotkit to the frontend.
"""

from __future__ import annotations

import logging

from ag_ui_langgraph import add_langgraph_fastapi_endpoint
from copilotkit import LangGraphAGUIAgent
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

# Allow running this file directly (python copilotkit_main.py) by importing
# from the package when present, and falling back to a relative import.
try:
    from app.api.copilotkit import A2UIFixedAgent
    from app.services.copilotkit_agent import agent, CATALOG_ID
except ImportError:  # pragma: no cover - direct execution fallback
    from api.copilotkit import A2UIFixedAgent  # type: ignore
    from services.copilotkit_agent import agent, CATALOG_ID  # type: ignore

logging.basicConfig(level=logging.INFO, force=True)

app = FastAPI(
    title="EnglishPro CopilotKit Agent (standalone)",
    version="1.0.0",
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.get("/health")
async def health():
    return {"status": "ok", "agent": "english_pro_agent", "catalog": CATALOG_ID}


add_langgraph_fastapi_endpoint(
    app=app,
    agent=A2UIFixedAgent(
        name="english_pro_agent",
        description=(
            "EnglishPro Assistant — an AI tutor that generates stories from "
            "vocabulary, explains words and grammar, and renders rich UI "
            "dashboards for English learners."
        ),
        graph=agent,
        config={"a2ui": {"default_catalog_id": CATALOG_ID}},
    ),
    path="/",
)


if __name__ == "__main__":
    import uvicorn

    uvicorn.run("copilotkit_main:app", host="0.0.0.0", port=8001, reload=True)
