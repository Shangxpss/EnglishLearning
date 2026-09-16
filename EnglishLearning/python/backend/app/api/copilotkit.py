"""CopilotKit LangGraph AG-UI endpoint for the English Learning backend.

This module mirrors AI-Demo/backend/main.py: it builds a FastAPI sub-app that
mounts the LangGraph agent as an AG-UI endpoint, then exposes it via the
`copilotkit_router` that the main app includes.

The A2UIFixedAgent override fixes the ag-ui/ag_ui key mismatch between
ag-ui-langgraph (writes "ag-ui") and LangGraph TypedDict fields (use
"ag_ui"). Without this fix, the inject_a2ui_tool flag never reaches
CopilotKitMiddleware and A2UI surfaces never render.

Data flow:
    Frontend (CopilotChat)
      → agent-runtime (Bun, /copilotkit)
        → this endpoint (FastAPI, /copilotkit-agent)
          → LangGraph agent (DeepSeek + tools + A2UI)
"""

from __future__ import annotations

import logging

from ag_ui_langgraph import add_langgraph_fastapi_endpoint
from copilotkit import LangGraphAGUIAgent
from fastapi import APIRouter, FastAPI
from fastapi.middleware.cors import CORSMiddleware

from app.services.copilotkit_agent import agent, CATALOG_ID

logger = logging.getLogger(__name__)

# A dedicated FastAPI sub-app is used because add_langgraph_fastapi_endpoint
# mounts a route at a given path on an ASGI app. Using a sub-app keeps the
# CopilotKit routes (and their CORS handling) isolated from the rest of the
# English Learning API.
copilotkit_app = FastAPI(
    title="EnglishPro CopilotKit Agent",
    description="LangGraph AG-UI endpoint powering the EnglishPro AI assistant.",
    version="1.0.0",
)

copilotkit_app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


class A2UIFixedAgent(LangGraphAGUIAgent):
    """LangGraphAGUIAgent that fixes the ag-ui/ag_ui key mismatch.

    ag-ui-langgraph writes state under the 'ag-ui' key (with hyphen), but
    Python TypedDict fields use underscores (ag_ui). LangGraph silently drops
    keys that don't match declared channels, so the inject_a2ui_tool flag
    never reaches CopilotKitMiddleware.

    This override:
    1. Adds 'ag_ui' to constant_schema_keys so get_stream_payload_input
       doesn't filter it out.
    2. Remaps 'ag-ui' -> 'ag_ui' in langgraph_default_merge_state so the
       data lands in the correct LangGraph channel.
    """

    def __init__(self, *, name, graph, description=None, config=None):
        super().__init__(name=name, graph=graph, description=description, config=config)
        # Add 'ag_ui' alongside the inherited 'copilotkit' so the key
        # survives get_stream_payload_input filtering.
        self.constant_schema_keys = self.constant_schema_keys + ["ag_ui"]

    def langgraph_default_merge_state(self, state, messages, input):
        result = super().langgraph_default_merge_state(state, messages, input)
        # Copy 'ag-ui' (hyphen) to 'ag_ui' (underscore) for LangGraph channels.
        # We must KEEP 'ag-ui' because CopilotKitMiddleware reads from
        # state.get("ag-ui") to find inject_a2ui_tool and a2ui_schema.
        if "ag-ui" in result:
            result["ag_ui"] = result["ag-ui"]
        return result


# Mount the LangGraph agent at the sub-app root. The agent-runtime proxies
# /copilotkit requests here (AGENT_URL should point at this FastAPI server).
add_langgraph_fastapi_endpoint(
    app=copilotkit_app,
    agent=A2UIFixedAgent(
        name="english_pro_agent",
        description=(
            "EnglishPro Assistant — an AI tutor that generates stories from "
            "vocabulary, explains words and grammar, and renders rich UI "
            "dashboards for English learners."
        ),
        graph=agent,
        config={
            "a2ui": {
                "default_catalog_id": CATALOG_ID,
            },
        },
    ),
    path="/",
)


@copilotkit_app.get("/health")
async def copilotkit_health():
    """Health check for the CopilotKit agent sub-app."""
    return {"status": "ok", "agent": "english_pro_agent", "catalog": CATALOG_ID}


# A router that the main English Learning FastAPI app can include. Mounting
# the sub-app under /copilotkit-agent keeps it separate from /api routes.
copilotkit_router = APIRouter()


@copilotkit_router.get("/copilotkit-agent/info")
async def copilotkit_info():
    """Return metadata about the CopilotKit agent (used for diagnostics)."""
    return {
        "agent_name": "english_pro_agent",
        "catalog_id": CATALOG_ID,
        "description": (
            "EnglishPro Assistant — AI tutor for English learners, powered by "
            "DeepSeek + LangGraph + CopilotKit + A2UI."
        ),
    }
