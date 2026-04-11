# Agents — Developer & CLI Guide

This file defines the agents (backend services) used by the EnglishLearning project and provides clear templates, naming conventions, and integration patterns so a CLI code generator can scaffold new agents reliably.

Goals:
- Describe agent file layout and required functions/classes
- Provide a minimal agent template and example for code generation
- Explain how to register agents with FastAPI and tests structure
- Document naming and configuration conventions for predictable scaffolding

## Project layout (important paths)
- backend/app/ — FastAPI backend code
  - backend/app/services/ — agent implementations (one file per agent)
  - backend/app/api/ — routers and route registration helpers
  - backend/app/core/ — configuration, settings, logger
  - backend/app/models/ — pydantic models and DTOs
- frontend/ — React / React Native apps (separate workspaces)
- AGENTS.md — this file (generator source of truth)

When generating code, place new agent files under: backend/app/services/<agent_name>_agent.py

## Agent conventions (required)
- Filename: snake_case: <agent_name>_agent.py (e.g., audio_processor_agent.py)
- Class: PascalCase with Agent suffix: <AgentName>Agent (e.g., AudioProcessorAgent)
- Export: module must expose a `get_agent()` factory function that returns a singleton agent instance
- Router registration: module-level `router` (FastAPI APIRouter) or a `register_routes(app)` function
- Logging: use app core logger (from backend.app.core.logger import get_logger())
- Settings: read env via backend.app.core.settings

Required methods on agent class (minimal interface):
- async def start(self) -> None: optional init (startup tasks)
- async def stop(self) -> None: optional cleanup (shutdown tasks)
- core processing method(s) documented via docstring (e.g., process_audio(self, audio_path: str) -> dict)

Example minimal template (for generator to emit)

```python
# backend/app/services/<agent_name>_agent.py
from fastapi import APIRouter, Depends
from backend.app.core.logger import get_logger
from backend.app.core.settings import settings

logger = get_logger(__name__)
router = APIRouter(prefix="/agents/<agent-name>")

class <AgentName>Agent:
    """Agent responsibilities described here."""
    def __init__(self):
        pass

    async def start(self):
        logger.info("<AgentName>Agent started")

    async def stop(self):
        logger.info("<AgentName>Agent stopped")

    async def process(self, payload: dict) -> dict:
        """Process a payload and return structured result."""
        return {"status": "ok"}

agent = <AgentName>Agent()

@router.post("/process")
async def process_endpoint(body: dict):
    return await agent.process(body)

def get_agent():
    return agent
```

Generator responsibilities
- Create file with name and class following conventions
- Add router prefix and endpoint stubs matching agent name
- Add import to backend/app/api/__init__.py or instructions for manual registration
- Optionally add a test skeleton at tests/agents/test_<agent_name>.py

## FastAPI integration
Recommended pattern in backend/app/main.py (generator-friendly):
- Import agent routers dynamically from backend.app.services
- Expose a registration helper in backend/app/api/register_agents.py that the generator can update

Example register helper (generator may append imports):

```python
# backend/app/api/register_agents.py
from fastapi import FastAPI

def register_agents(app: FastAPI, routers: list):
    for r in routers:
        app.include_router(r)
```

Generator should add the new agent's router to a central list or provide a single-line import the main app imports.

## Pydantic models & DTOs
- Keep models in backend/app/models/
- Generator should create request/response models for endpoints (e.g., <AgentName>Request, <AgentName>Response)
- Import models in the agent module to keep contracts explicit

## Testing conventions
- tests/agents/test_<agent_name>.py — basic unit tests
- Tests should instantiate the agent, call core methods, and assert outputs
- Integration tests: use TestClient against the agent router

Example test template:

```python
from backend.app.services.<agent_name>_agent import <AgentName>Agent

async def test_process_returns_ok():
    agent = <AgentName>Agent()
    result = await agent.process({})
    assert result.get("status") == "ok"
```

## CLI scaffolding commands (suggested)
Provide these commands to generate an agent (the repository may not include them; generator tools should implement):
- generate agent: `python scripts/generate_agent.py <agent-name> --with-router --with-tests`
- or pnpm script for JS-based generator: `pnpm run gen:agent -- <agent-name>`

Generator behavior:
- Ask for agent display name and description
- Emit: service file, tests file, pydantic models file (optional), and an import line for register_agents
- Respect existing files (do not overwrite unless --force)

## Naming & style rules (for deterministic generation)
- agent-name (CLI arg): kebab-case or snake_case accepted; generator normalizes to snake_case file and PascalCase class
- router prefix: `/agents/<kebab-agent-name>`
- test filename: tests/agents/test_<snake_agent_name>.py
- model filename: backend/app/models/<snake_agent_name>_models.py

## Example: Audio Processor (reference)
- File: backend/app/services/audio_processor_agent.py
- Class: AudioProcessorAgent
- Router: /agents/audio-processor
- Core method: async def analyze_pronunciation(self, audio_path: str) -> dict

## Checklist for generated agents (automated verification)
- [ ] File created at backend/app/services/<agent>_agent.py
- [ ] Class <AgentName>Agent present and exported via get_agent()
- [ ] APIRouter or register_routes present
- [ ] Pydantic models created (if endpoints accept structured input)
- [ ] Unit test skeleton exists at tests/agents/test_<agent>.py
- [ ] README snippet or docstring describing responsibilities

## Troubleshooting generator issues
- Ensure package imports use absolute imports (backend.app.services...) so main.py can import dynamically
- Avoid circular imports between services and core; use get_agent factories and import weak references

## Extending this file
When adding new conventions or generator flags, update this file and the generator scripts. Keep examples minimal and machine-parsable (templates should match the exact strings used here for prefix, class suffix, and factory function name).

-- End of document
