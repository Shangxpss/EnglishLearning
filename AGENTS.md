# Agents — Developer & CLI Guide

This file defines the agents (backend services) used by the EnglishLearning project and provides clear templates, naming conventions, and integration patterns so a CLI code generator can scaffold new agents reliably.

## Table of Contents
- [Project Layout](#project-layout)
- [Naming Rules](#naming-rules)
- [Agent Interface](#agent-interface)
- [Router Pattern & Registration](#router-pattern--registration)
- [Pydantic Contracts](#pydantic-contracts)
- [Minimal Templates](#minimal-templates)
- [CLI Hooks](#cli-hooks)
- [Tests](#tests)
- [Dev Commands](#dev-commands)
- [Idempotency Rules](#idempotency-rules)
- [Machine Manifest](#machine-manifest)

---

## Project Layout

Exact paths for services, models, tests:

```
backend/
├── app/
│   ├── main.py                 # FastAPI application entry point
│   ├── api/
│   │   └── register_agents.py  # Agent router registration hook
│   ├── services/               # Agent implementations
│   │   ├── audio_processor.py
│   │   └── subtitle_processor.py
│   ├── models/                 # Pydantic models & DTOs
│   │   ├── response_models.py
│   │   ├── user_models.py
│   │   └── auth_models.py
│   └── core/
│       ├── config.py           # Settings & environment
│       ├── database.py         # DB connection
│       └── auth.py             # Auth utilities
├── tests/                      # Test directory (to be created)
│   ├── agents/
│   │   ├── test_audio_processor.py
│   │   └── test_subtitle_processor.py
│   └── conftest.py
└── agents.json                 # Machine manifest for agent discovery
```

**Generator placement rules:**
- New agent file: `backend/app/services/{{agent_name}}_agent.py`
- New models: `backend/app/models/{{agent_name}}_models.py`
- New tests: `backend/tests/agents/test_{{agent_name}}.py`

---

## Naming Rules

| Component        | Format          | Example                    |
|------------------|-----------------|----------------------------|
| File (snake_case)| `{{agent_name}}_agent.py` | `audio_processor_agent.py` |
| Class (PascalCase + Agent) | `{{AgentName}}Agent` | `AudioProcessorAgent` |
| Router prefix    | `/agents/<kebab>` | `/agents/audio-processor` |
| Test file        | `test_{{agent_name}}.py` | `test_audio_processor.py` |
| Model file       | `{{agent_name}}_models.py` | `audio_processor_models.py` |

**Normalization rules:**
- CLI input `agent-name` (kebab-case) → normalize to `agent_name` (snake_case) for files
- Class name: capitalize each word, append `Agent` suffix
- Router prefix: keep kebab-case for URL paths

---

## Agent Interface

Required methods on every agent class:

```python
class {{AgentName}}Agent:
    """Agent responsibilities described here."""

    async def start(self) -> None:
        """Initialize agent resources (startup tasks)."""
        pass

    async def stop(self) -> None:
        """Cleanup agent resources (shutdown tasks)."""
        pass

    async def process(self, payload: dict) -> dict:
        """Core processing method. Process a payload and return structured result."""
        raise NotImplementedError
```

**Method signatures:**
- `async def start(self) -> None` — optional initialization
- `async def stop(self) -> None` — optional cleanup
- `async def process(self, payload: <RequestModel>) -> <ResponseModel>` — core business logic

**Example with typed contracts:**
```python
from backend.app.models.audio_processor_models import AudioProcessRequest, AudioProcessResponse

class AudioProcessorAgent:
    async def process(self, payload: AudioProcessRequest) -> AudioProcessResponse:
        """Analyze audio and return learning metrics."""
        ...
```

---

## Router Pattern & Registration

### Register Hook

Create `backend/app/api/register_agents.py`:

```python
# backend/app/api/register_agents.py
from fastapi import FastAPI
from typing import List
from fastapi.routing import APIRouter

# IMPORT_MARKER: Generator will inject agent router imports above this line


def register_agents(app: FastAPI, routers: List[APIRouter]) -> None:
    """Register all agent routers with the FastAPI application."""
    for router in routers:
        app.include_router(router)


def get_all_routers() -> List[APIRouter]:
    """Return list of all agent routers for registration."""
    # ROUTER_MARKER: Generator will append new routers to this list
    from backend.app.services.audio_processor_agent import router as audio_router
    from backend.app.services.subtitle_processor_agent import router as subtitle_router
    
    return [
        audio_router,      # /agents/audio-processor
        subtitle_router,   # /agents/subtitle-processor
        # GENERATOR_MARKER: New routers appended here
    ]
```

### Main.py Integration

In `backend/app/main.py`, add after service initialization:

```python
# After existing service imports
from backend.app.api.register_agents import register_agents, get_all_routers

# At end of setup, before running app
@app.on_event("startup")
async def startup_event():
    routers = get_all_routers()
    register_agents(app, routers)
    # Start all agents
    # for agent in get_all_agents(): await agent.start()

@app.on_event("shutdown")
async def shutdown_event():
    # Stop all agents
    # for agent in get_all_agents(): await agent.stop()
    pass
```

---

## Pydantic Contracts

### File Locations
- Request models: `backend/app/models/{{agent_name}}_models.py`
- Response models: `backend/app/models/response_models.py` (shared) or agent-specific

### Example Models

**File: `backend/app/models/audio_processor_models.py`**

```python
from pydantic import BaseModel, Field
from typing import Optional, List, Dict, Any


class AudioProcessRequest(BaseModel):
    """Request model for audio processing."""
    audio_path: str = Field(..., description="Path to audio file")
    language: str = Field(default="en", description="Language code")
    model_size: str = Field(default="small", description="Whisper model size")

    class Config:
        json_schema_extra = {
            "example": {
                "audio_path": "/tmp/audio.wav",
                "language": "en",
                "model_size": "small"
            }
        }


class AudioProcessResponse(BaseModel):
    """Response model for audio processing."""
    success: bool
    data: Dict[str, Any]
    message: str
    errors: Optional[List[str]] = None

    class Config:
        json_schema_extra = {
            "example": {
                "success": True,
                "data": {
                    "duration": 120.5,
                    "tempo": 120.0,
                    "speaking_speed": "normal"
                },
                "message": "Audio processed successfully",
                "errors": None
            }
        }
```

---

## Minimal Templates

### 1. Agent Template

**File: `backend/app/services/{{agent_name}}_agent.py`**

```python
# backend/app/services/{{agent_name}}_agent.py
"""{{AgentName}} agent for handling {{description}}."""

from fastapi import APIRouter, Depends, UploadFile, File, Form
from typing import Optional
from backend.app.core.logger import get_logger
from backend.app.core.config import settings

logger = get_logger(__name__)
router = APIRouter(prefix="/agents/{{kebab-agent-name}}", tags=["{{agent_name}}"])


class {{AgentName}}Agent:
    """{{AgentName}} agent handles {{description}}."""

    def __init__(self):
        self.initialized = False

    async def start(self) -> None:
        """Initialize agent resources."""
        logger.info("{{AgentName}}Agent started")
        self.initialized = True

    async def stop(self) -> None:
        """Cleanup agent resources."""
        logger.info("{{AgentName}}Agent stopped")
        self.initialized = False

    async def process(self, payload: dict) -> dict:
        """Process payload and return result."""
        if not self.initialized:
            raise RuntimeError("Agent not initialized. Call start() first.")
        # TODO: Implement processing logic
        return {"status": "ok", "data": {}}


# Singleton instance
agent = {{AgentName}}Agent()


@router.post("/process")
async def process_endpoint(payload: dict):
    """Process endpoint for {{agent_name}}."""
    result = await agent.process(payload)
    return result


@router.get("/health")
async def health_check():
    """Health check endpoint."""
    return {"status": "healthy" if agent.initialized else "uninitialized"}


def get_agent() -> {{AgentName}}Agent:
    """Factory function to get agent singleton."""
    return agent
```

### 2. Endpoint Template

```python
@router.post("/{{endpoint_name}}")
async def {{endpoint_name}}_handler(
    file: UploadFile = File(...),
    param: Optional[str] = Form(None)
):
    """Handler for {{endpoint_name}} endpoint."""
    # TODO: Implement handler logic
    return {"success": True}
```

### 3. Model Template

**File: `backend/app/models/{{agent_name}}_models.py`**

```python
from pydantic import BaseModel, Field
from typing import Optional, List, Dict, Any


class {{AgentName}}Request(BaseModel):
    """Request model for {{agent_name}}."""
    field_name: str = Field(..., description="Field description")

    class Config:
        json_schema_extra = {
            "example": {"field_name": "example_value"}
        }


class {{AgentName}}Response(BaseModel):
    """Response model for {{agent_name}}."""
    success: bool
    data: Dict[str, Any]
    message: str

    class Config:
        json_schema_extra = {
            "example": {
                "success": True,
                "data": {},
                "message": "Operation completed"
            }
        }
```

### 4. Test Skeleton

**File: `backend/tests/agents/test_{{agent_name}}.py`**

```python
import pytest
from unittest.mock import AsyncMock, patch
from backend.app.services.{{agent_name}}_agent import {{AgentName}}Agent, get_agent


class Test{{AgentName}}Agent:
    """Unit tests for {{AgentName}}Agent."""

    @pytest.mark.asyncio
    async def test_start_initializes_agent(self):
        """Test that start() initializes the agent."""
        agent = {{AgentName}}Agent()
        assert agent.initialized is False
        await agent.start()
        assert agent.initialized is True

    @pytest.mark.asyncio
    async def test_stop_cleanup(self):
        """Test that stop() cleans up resources."""
        agent = {{AgentName}}Agent()
        await agent.start()
        await agent.stop()
        assert agent.initialized is False

    @pytest.mark.asyncio
    async def test_process_returns_ok(self):
        """Test that process() returns expected structure."""
        agent = {{AgentName}}Agent()
        await agent.start()
        result = await agent.process({"test": "data"})
        assert result.get("status") == "ok"
        assert "data" in result


class Test{{AgentName}}Endpoint:
    """Integration tests using FastAPI TestClient."""

    @pytest.fixture
    def client(self):
        """Create test client with agent router."""
        from fastapi.testclient import TestClient
        from fastapi import FastAPI
        from backend.app.services.{{agent_name}}_agent import router

        app = FastAPI()
        app.include_router(router)
        return TestClient(app)

    def test_health_endpoint(self, client):
        """Test health check endpoint."""
        response = client.get("/agents/{{kebab-agent-name}}/health")
        assert response.status_code == 200
        data = response.json()
        assert "status" in data

    def test_process_endpoint(self, client):
        """Test process endpoint."""
        response = client.post(
            "/agents/{{kebab-agent-name}}/process",
            json={"test": "payload"}
        )
        assert response.status_code == 200
        data = response.json()
        assert data.get("status") == "ok"
```

---

## CLI Hooks

Lines/comments where generator should inject imports/routers:

### In `backend/app/api/register_agents.py`:
```python
# IMPORT_MARKER: Generator will inject agent router imports above this line
from backend.app.services.{{agent_name}}_agent import router as {{snake_agent_name}}_router

# ROUTER_MARKER: Generator will append new routers to this list
{{snake_agent_name}}_router,  # /agents/{{kebab-agent-name}}

# GENERATOR_MARKER: New routers appended here
```

### In `backend/app/main.py`:
```python
# AGENT_IMPORT_MARKER: Generator adds agent imports here
from backend.app.services.{{agent_name}}_agent import get_agent

# AGENT_STARTUP_MARKER: Generator adds agent startup calls here
# await {{snake_agent_name}}_agent.start()

# AGENT_SHUTDOWN_MARKER: Generator adds agent shutdown calls here
# await {{snake_agent_name}}_agent.stop()
```

---

## Tests

### Unit Test Example

```python
# tests/agents/test_audio_processor.py
import pytest
from backend.app.services.audio_processor_agent import AudioProcessorAgent


@pytest.mark.asyncio
async def test_audio_processor_start():
    agent = AudioProcessorAgent()
    await agent.start()
    assert agent.initialized is True


@pytest.mark.asyncio
async def test_audio_processor_process():
    agent = AudioProcessorAgent()
    await agent.start()
    result = await agent.process({"audio_path": "/tmp/test.wav"})
    assert result["success"] is True
```

### FastAPI TestClient Example

```python
# tests/agents/test_audio_processor_integration.py
import pytest
from fastapi.testclient import TestClient
from fastapi import FastAPI
from backend.app.services.audio_processor_agent import router


@pytest.fixture
def client():
    app = FastAPI()
    app.include_router(router)
    return TestClient(app)


def test_health_check(client):
    response = client.get("/agents/audio-processor/health")
    assert response.status_code == 200
    assert response.json()["status"] in ["healthy", "uninitialized"]


def test_process_endpoint(client):
    response = client.post(
        "/agents/audio-processor/process",
        json={"audio_path": "/tmp/test.wav"}
    )
    assert response.status_code == 200
    assert response.json()["status"] == "ok"
```

### Expected Assertions
- Status code: `assert response.status_code == 200`
- Response structure: `assert "success" in data` or `assert "status" in data`
- Data types: `assert isinstance(data["data"], dict)`
- Error handling: `assert response.json().get("errors") is None`

---

## Dev Commands

### Lint, Build, Test, Run Scripts

Add to `backend/pyproject.toml`:

```toml
[tool.uv.scripts]
lint = "ruff check backend/app && black --check backend/app"
lint-fix = "ruff check backend/app --fix && black backend/app"
build = "uv pip compile requirements.in -o requirements.txt"
test = "pytest backend/tests -v --cov=backend/app"
test-unit = "pytest backend/tests/agents -v"
test-integration = "pytest backend/tests/integration -v"
run = "uvicorn backend.app.main:app --reload --host 0.0.0.0 --port 8000"
generate-agent = "python scripts/generate_agent.py"
```

### CI Job Names (GitHub Actions)

```yaml
# .github/workflows/ci.yml
jobs:
  lint:
    name: Lint Code
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run linters
        run: uv run lint

  test:
    name: Run Tests
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run unit tests
        run: uv run test-unit
      - name: Run integration tests
        run: uv run test-integration

  build:
    name: Build & Validate
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Build dependencies
        run: uv run build

  generate-agent:
    name: Generate Agent Validation
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Validate agent generation
        run: uv run generate-agent --validate
```

---

## Idempotency Rules

### Do Not Overwrite Without `--force`

Generator behavior:
1. **Check existence**: Before creating any file, check if it exists
2. **Skip if exists**: If file exists and `--force` flag is NOT provided, skip generation
3. **Warn user**: Print warning message when skipping
4. **Merge strategy**: For registration files (`register_agents.py`), append to markers instead of overwriting

### Merge Strategy for Registration Files

When adding to `register_agents.py`:
```python
# Check if import already exists
if "from backend.app.services.{{agent_name}}_agent import router" not in content:
    # Inject at IMPORT_MARKER
    content = content.replace(
        "# IMPORT_MARKER: ...",
        f"from backend.app.services.{{agent_name}}_agent import router as {{snake}}_router\n# IMPORT_MARKER: ..."
    )

# Check if router already in list
if f"{{snake}}_router" not in content:
    # Append at GENERATOR_MARKER
    content = content.replace(
        "# GENERATOR_MARKER: ...",
        f"        {{snake}}_router,  # /agents/{{kebab}}\n        # GENERATOR_MARKER: ..."
    )
```

### Conflict Resolution
- If file exists with same content → skip silently
- If file exists with different content → warn and skip (unless `--force`)
- For marker-based files → merge intelligently, avoiding duplicates

---

## Machine Manifest

### `agents.json` Example

**File: `backend/agents.json`**

```json
{
  "version": "1.0.0",
  "agents": [
    {
      "name": "audio_processor",
      "class": "AudioProcessorAgent",
      "file": "backend/app/services/audio_processor_agent.py",
      "router_prefix": "/agents/audio-processor",
      "endpoints": [
        {
          "path": "/process",
          "method": "POST",
          "description": "Process audio file"
        },
        {
          "path": "/health",
          "method": "GET",
          "description": "Health check"
        }
      ],
      "models": {
        "request": "AudioProcessRequest",
        "response": "AudioProcessResponse"
      },
      "permissions": ["read:audio", "write:analysis"]
    },
    {
      "name": "subtitle_processor",
      "class": "SubtitleProcessorAgent",
      "file": "backend/app/services/subtitle_processor_agent.py",
      "router_prefix": "/agents/subtitle-processor",
      "endpoints": [
        {
          "path": "/generate",
          "method": "POST",
          "description": "Generate subtitles from audio"
        },
        {
          "path": "/parse",
          "method": "POST",
          "description": "Parse SRT subtitle file"
        }
      ],
      "models": {
        "request": "SubtitleProcessRequest",
        "response": "SubtitleProcessResponse"
      },
      "permissions": ["read:audio", "write:subtitle"]
    }
  ],
  "generator": {
    "import_marker": "# IMPORT_MARKER: Generator will inject agent router imports above this line",
    "router_marker": "# GENERATOR_MARKER: New routers appended here",
    "template_dir": "backend/templates"
  }
}
```

### YAML Alternative: `agents.yaml`

```yaml
version: "1.0.0"
agents:
  - name: audio_processor
    class: AudioProcessorAgent
    file: backend/app/services/audio_processor_agent.py
    router_prefix: /agents/audio-processor
    endpoints:
      - path: /process
        method: POST
        description: Process audio file
      - path: /health
        method: GET
        description: Health check
    models:
      request: AudioProcessRequest
      response: AudioProcessResponse
    permissions:
      - read:audio
      - write:analysis

generator:
  import_marker: "# IMPORT_MARKER: Generator will inject agent router imports above this line"
  router_marker: "# GENERATOR_MARKER: New routers appended here"
  template_dir: backend/templates
```

---

## Tokenized Template Example

Here is one small tokenized template for quick reference:

```python
# backend/app/services/{{agent_name}}_agent.py
from fastapi import APIRouter
from backend.app.core.logger import get_logger

logger = get_logger(__name__)
router = APIRouter(prefix="/agents/{{kebab-agent-name}}")

class {{AgentName}}Agent:
    async def start(self) -> None: pass
    async def stop(self) -> None: pass
    async def process(self, payload: dict) -> dict: return {"status": "ok"}

agent = {{AgentName}}Agent()

@router.post("/process")
async def process_endpoint(body: dict):
    return await agent.process(body)

def get_agent():
    return agent
```

**Tokens to replace:**
- `{{agent_name}}` → snake_case name (e.g., `audio_processor`)
- `{{AgentName}}` → PascalCase name (e.g., `AudioProcessor`)
- `{{kebab-agent-name}}` → kebab-case for URLs (e.g., `audio-processor`)

---

## Checklist for Generated Agents

- [ ] File created at `backend/app/services/{{agent_name}}_agent.py`
- [ ] Class `{{AgentName}}Agent` present with `start()`, `stop()`, `process()` methods
- [ ] Exported via `get_agent()` factory function
- [ ] `APIRouter` with prefix `/agents/{{kebab-agent-name}}`
- [ ] Pydantic models created in `backend/app/models/{{agent_name}}_models.py`
- [ ] Unit test skeleton at `backend/tests/agents/test_{{agent_name}}.py`
- [ ] Import added to `backend/app/api/register_agents.py` at `IMPORT_MARKER`
- [ ] Router added to `get_all_routers()` at `GENERATOR_MARKER`
- [ ] Entry added to `backend/agents.json`
- [ ] Docstring describing agent responsibilities

---

## Troubleshooting

**Circular imports**: Use `get_agent()` factories and import at function level if needed.

**Missing markers**: Ensure `IMPORT_MARKER` and `GENERATOR_MARKER` comments exist in `register_agents.py`.

**Duplicate routers**: Check `agents.json` and `register_agents.py` for existing entries before generating.

**Test failures**: Verify agent is initialized with `await agent.start()` before calling `process()`.

---

*End of document — Last updated for generator v1.0.0*
