# Technical Architecture Document: CopilotKit + LangGraph + A2UI Integration

## 1. System Overview

### 1.1 Three-Layer Architecture

```
Frontend (React/Vite)          →  Agent Runtime (Bun/TypeScript)  →  Backend (Python/FastAPI/LangGraph)
         ↓                              ↓                              ↓
    CopilotKit Core              CopilotRuntime Handler            LangGraph Agent
    A2UI Catalog                 HttpAgent + A2UIMiddleware        CopilotKitMiddleware
    React Components             InMemoryAgentRunner               A2UI Tool Integration
```

### 1.2 Repository Structure

This system consists of three separate repositories:

| Repository      | Language         | Role                                               | Documentation                                  |
| --------------- | ---------------- | -------------------------------------------------- | ---------------------------------------------- |
| `front`         | TypeScript/React | Client-side UI with A2UI rendering                 | [docs/front.md](docs/front.md)                 |
| `agent-runtime` | TypeScript/Bun   | Middle layer for request routing and SSE streaming | [docs/agent-runtime.md](docs/agent-runtime.md) |
| `backend`       | Python/FastAPI   | LangGraph agent with tool capabilities             | [docs/backend.md](docs/backend.md)             |

## 1.4 Open-Source Repository Dependencies

This system is built on three core open-source repositories maintained by CopilotKit:

| Repository     | GitHub                                                            | Role in This System                                            | Maintainer Documentation                                         |
| -------------- | ----------------------------------------------------------------- | -------------------------------------------------------------- | ---------------------------------------------------------------- |
| **CopilotKit** | [CopilotKit/CopilotKit](https://github.com/CopilotKit/CopilotKit) | Frontend React SDK, Python LangGraph middleware, A2UI renderer | [docs/open-source/copilotkit.md](docs/open-source/copilotkit.md) |
| **ag-ui**      | [ag-ui-protocol/ag-ui](https://github.com/ag-ui-protocol/ag-ui)   | Event-based protocol for agent↔UI communication                | [docs/open-source/ag-ui.md](docs/open-source/ag-ui.md)           |
| **a2ui**       | [CopilotKit/A2UI](https://github.com/CopilotKit/A2UI)             | Declarative JSON format for agent-generated UIs                | [docs/open-source/a2ui.md](docs/open-source/a2ui.md)             |

### 1.3 Communication Protocols

- **Frontend ↔ Runtime**: HTTP/HTTPS with Server-Sent Events (SSE)
- **Runtime ↔ Agent**: HTTP/HTTPS with SSE (AG-UI protocol)
- **Agent ↔ UI**: A2UI protocol via AG-UI `ACTIVITY_SNAPSHOT` events
- **All layers**: Event-based communication via AG-UI protocol

## 2. Layer Deep Dive

### 2.1 Frontend Layer (`front` repository)

**Detailed documentation**: [docs/front.md](docs/front.md)

#### 2.1.1 Core Components

| Component           | File                      | Purpose                             |
| ------------------- | ------------------------- | ----------------------------------- |
| `CopilotKit`        | `src/Chat.tsx`            | Root provider connecting to runtime |
| `CopilotChat`       | `src/Chat.tsx`            | Chat interface component            |
| `myCatalog`         | `src/a2ui/catalog.ts`     | A2UI component catalog registration |
| `customDefinitions` | `src/a2ui/definitions.ts` | Custom component schemas            |
| `myRenderers`       | `src/a2ui/renderers.tsx`  | React component implementations     |

#### 2.1.2 A2UI Catalog

The catalog merges 18 built-in components with 13 custom components:

```typescript
// src/a2ui/catalog.ts
export const myCatalog = createCatalog(customDefinitions, myRenderers, {
  catalogId: "generative-agent-catalog",
  includeBasicCatalog: true,
});
```

**Built-in components**: Text, Row, Column, Card, List, Button, Divider, Tabs, Modal, Image, Icon, Video, AudioPlayer, TextField, CheckBox, ChoicePicker, Slider, DateTimeInput

**Custom components**: Heading, Grid, Table, KeyValueList, StatusBadge, Metric, InfoRow, BarChart, PieChart, UserCard, ResetButton, Spacer, WeatherCard

#### 2.1.3 Key Configuration

```tsx
// src/Chat.tsx
<CopilotKit
  runtimeUrl="/copilotkit"
  agent="sample_agent"
  useSingleEndpoint={false}
  a2ui={{ catalog: myCatalog, includeSchema: true }}
  renderToolCalls={cleanToolCallRenderers}
>
  <CopilotChat agentId="sample_agent" />
</CopilotKit>
```

**Critical**: `includeSchema: true` sends full component schemas to the agent, enabling `generate_a2ui` sub-agent mode.

---

### 2.2 Runtime Layer (`agent-runtime` repository)

**Detailed documentation**: [docs/agent-runtime.md](docs/agent-runtime.md)

#### 2.2.1 Core Components

| Component               | File       | Purpose                                   |
| ----------------------- | ---------- | ----------------------------------------- |
| `CopilotRuntime`        | `index.ts` | Central orchestrator for agent management |
| `HttpAgent`             | `index.ts` | HTTP client connecting to Python backend  |
| `CopilotRuntimeHandler` | `index.ts` | HTTP handler for runtime endpoints        |
| `Bun.serve`             | `index.ts` | HTTP server with CORS                     |

#### 2.2.2 Runtime Configuration

```typescript
// index.ts
const runtime = new CopilotRuntime({
  agents: { sample_agent: agent },
  a2ui: {
    injectA2UITool: true,
    defaultCatalogId: "generative-agent-catalog",
  },
  runner: new InMemoryAgentRunner(),
});
```

#### 2.2.3 A2UIMiddleware Pipeline

When `injectA2UITool: true`, the middleware automatically applies:

1. **processUserAction**: Handles button clicks from A2UI components
2. **injectSchemaContext**: Adds server-side schema to request context
3. **injectToolAndFlag**: Adds `render_a2ui` tool + `injectA2UITool` flag to `forwardedProps`
4. **injectToolGuidelines**: Adds `render_a2ui` usage instructions to LLM context
5. **processStream**: Intercepts tool calls, extracts A2UI operations, emits `ACTIVITY_SNAPSHOT` events

#### 2.2.4 Endpoints

| Method     | Path                                        | Purpose           |
| ---------- | ------------------------------------------- | ----------------- |
| `GET`      | `/copilotkit/info`                          | Agent information |
| `POST`     | `/copilotkit/agent/:agentId/run`            | Execute agent     |
| `GET`      | `/copilotkit/agent/:agentId/connect`        | SSE connection    |
| `POST`     | `/copilotkit/agent/:agentId/stop/:threadId` | Stop execution    |
| `GET/POST` | `/copilotkit/threads`                       | Thread management |

---

### 2.3 Backend Layer (`backend` repository)

**Detailed documentation**: [docs/backend.md](docs/backend.md)

#### 2.3.1 Core Components

| Component             | File       | Purpose                                          |
| --------------------- | ---------- | ------------------------------------------------ |
| `FastAPI`             | `main.py`  | HTTP server                                      |
| `A2UIFixedAgent`      | `main.py`  | Custom agent fixing `ag-ui`/`ag_ui` key mismatch |
| `AgentStateSchema`    | `agent.py` | State schema with `ag_ui` channel                |
| `A2UIAwareMiddleware` | `agent.py` | CopilotKit middleware for A2UI                   |
| `LangGraph Agent`     | `agent.py` | AI agent with tools                              |

#### 2.3.2 Agent Configuration

```python
# agent.py
agent = create_agent(
    model=llm,
    tools=[get_weather, calculate, register_user, display_register_form],
    middleware=[A2UIAwareMiddleware()],
    system_prompt=SYSTEM_PROMPT,
    checkpointer=MemorySaver(),
)
```

#### 2.3.3 Tools

| Tool                    | Type  | Description                |
| ----------------------- | ----- | -------------------------- |
| `get_weather`           | Basic | Returns weather as text    |
| `calculate`             | Basic | Evaluates math expressions |
| `register_user`         | Basic | Registers a user           |
| `display_register_form` | A2UI  | Shows registration form    |

#### 2.3.4 A2UIFixedAgent: The Key Fix

```python
# main.py
class A2UIFixedAgent(LangGraphAGUIAgent):
    def __init__(self, *, name, graph, description=None, config=None):
        super().__init__(name=name, graph=graph, description=description, config=config)
        self.constant_schema_keys = self.constant_schema_keys + ["ag_ui"]

    def langgraph_default_merge_state(self, state, messages, input):
        result = super().langgraph_default_merge_state(state, messages, input)
        if "ag-ui" in result:
            result["ag_ui"] = result["ag-ui"]
        return result
```

**Problem solved**: LangGraph filters state keys not in the schema. By adding `ag_ui` to `constant_schema_keys` and copying `ag-ui` data to `ag_ui`, the A2UI state survives the filtering process.

---

## 3. Message Flow

### 3.1 Complete Request Lifecycle

```
1. User sends message in CopilotChat component
   ↓
2. CopilotKitCore creates message and calls agent.run()
   ↓
3. HTTP POST to /copilotkit/agent/sample_agent/run
   ↓
4. Runtime handler validates request
   ↓
5. CopilotRuntime resolves agent and applies A2UIMiddleware
   ↓
6. HTTP request to Python backend (http://localhost:8000/)
   ↓
7. FastAPI endpoint receives request via LangGraph middleware
   ↓
8. A2UIFixedAgent.langgraph_default_merge_state() merges state
   ↓
9. LangGraph agent processes message with LLM
   ↓
10. CopilotKitMiddleware decides whether to inject generate_a2ui
   ↓
11. Agent executes tools (or generate_a2ui spawns sub-agent)
   ↓
12. Agent emits AG-UI events (RUN_STARTED, STEP_STARTED, ACTIVITY_SNAPSHOT, etc.)
   ↓
13. SSE stream back to frontend with events
   ↓
14. CopilotKitCore processes events and updates message store
   ↓
15. A2UI renderer renders components in UI surfaces
   ↓
16. React components re-render with new state
   ↓
17. User interacts with A2UI components → log_a2ui_event sent to agent
   ↓
18. Agent processes action and responds with updated UI
   ↓
19. Agent emits RUN_FINISHED event
   ↓
20. SSE stream closes, frontend displays final state
```

### 3.2 A2UI Schema Injection Flow

This is the critical path that ensures the LLM only generates valid components:

```
┌─────────────────────────────────────────────────────────────────────┐
│  Frontend (src/Chat.tsx)                                           │
│                                                                     │
│  <CopilotKit a2ui={{ catalog: myCatalog, includeSchema: true }}>    │
│       │                                                             │
│       ├── A2UICatalogContext injects:                               │
│       │    ├── catalog ID list                                      │
│       │    ├── full component schemas                               │
│       │    ├── generation guidelines                                │
│       │    └── design guidelines                                    │
│       │                                                             │
│       └── HTTP POST with context + forwardedProps:                  │
│            { injectA2UITool: true }                                 │
└──────────────────────────┬──────────────────────────────────────────┘
                           │ HTTP POST
                           ▼
┌─────────────────────────────────────────────────────────────────────┐
│  Agent Runtime (index.ts)                                           │
│                                                                     │
│  A2UIMiddleware.run():                                              │
│    1. processUserAction() — handle button clicks                    │
│    2. injectSchemaContext() — add server-side schema               │
│    3. injectToolAndFlag() — add render_a2ui tool + flag            │
│    4. injectToolGuidelines() — add usage guide                     │
│    5. processStream() — intercept tool calls, emit ACTIVITY_SNAPSHOT│
└──────────────────────────┬──────────────────────────────────────────┘
                           │ HTTP (via HttpAgent)
                           ▼
┌─────────────────────────────────────────────────────────────────────┐
│  Backend (main.py → agent.py)                                       │
│                                                                     │
│  A2UIFixedAgent.langgraph_default_merge_state():                    │
│    1. Extracts A2UI schema from context → state["ag-ui"]["a2ui_schema"]│
│    2. Extracts injectA2UITool → state["ag-ui"]["inject_a2ui_tool"]  │
│    3. Copies "ag-ui" → "ag_ui" for LangGraph channels               │
│                                                                     │
│  CopilotKitMiddleware.wrap_model_call():                            │
│    1. _a2ui_inject_decision(state) — check inject_a2ui_tool        │
│    2. _resolve_a2ui_catalog(state) — extract catalog_id + schema   │
│    3. _maybe_build_a2ui_tool() — call get_a2ui_tools(model, ...)   │
│    4. If built: replace render_a2ui with generate_a2ui             │
│                                                                     │
│  Agent calls generate_a2ui(prompt="...") → sub-agent with full schema│
│  → validate_a2ui_components() → recovery retry loop                 │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.3 generate_a2ui Sub-Agent Flow

When the main agent calls `generate_a2ui`:

```
Main Agent (LLM)
  │
  ├─ Sees tools: [generate_a2ui, get_weather, calculate, ...]
  │   (render_a2ui removed by CopilotKitMiddleware)
  │
  └─ Calls generate_a2ui(intent="create dashboard")
       │
       ├─ prepare_a2ui_request() → build_context_prompt(state)
       │    └─ reads state["ag-ui"]["a2ui_schema"]
       │
       ├─ model.bind_tools([RENDER_A2UI_TOOL_DEF], tool_choice="render_a2ui")
       │    └─ Sub-agent LLM invoked with forced tool call
       │         ├─ Input: [SystemMessage(prompt with full schema), *messages]
       │         └─ Output: {surfaceId, components, data}
       │
       ├─ run_a2ui_generation_with_recovery()
       │    └─ validate_a2ui_components(catalog=parsed_schema)
       │         └─ validates component names against catalog
       │         └─ retries on failure (up to 3 attempts)
       │
       └─ Returns valid A2UI operations → A2UIMiddleware emits ACTIVITY_SNAPSHOT
```

---

## 4. Protocol Details

### 4.1 AG-UI Protocol

**Purpose**: Event-based communication protocol for agent↔UI interaction

**Transport**: Server-Sent Events (SSE) over HTTP/HTTPS

**Event Types**:

| Type                 | Purpose                   |
| -------------------- | ------------------------- |
| `RUN_STARTED`        | Agent execution begins    |
| `STEP_STARTED`       | Processing step begins    |
| `STEP_FINISHED`      | Processing step completes |
| `RUN_FINISHED`       | Agent execution completes |
| `TEXT_MESSAGE_CHUNK` | Streaming text response   |
| `TOOL_CALL_STARTED`  | Tool execution begins     |
| `TOOL_CALL_RESULT`   | Tool execution result     |
| `ACTIVITY_SNAPSHOT`  | A2UI surface updates      |

### 4.2 A2UI Protocol

**Purpose**: Declarative UI definition protocol for agent-driven interfaces

**Transport**: Embedded in AG-UI `ACTIVITY_SNAPSHOT` events

**Message Types (v0.9)**:

| Operation          | Purpose                            |
| ------------------ | ---------------------------------- |
| `createSurface`    | Create new UI surface with catalog |
| `updateComponents` | Add or update UI components        |
| `updateDataModel`  | Update application state           |
| `deleteSurface`    | Remove UI surface                  |

**Component Structure**:

```json
{
  "id": "weather-card",
  "component": "WeatherCard",
  "location": "Beijing",
  "temperature": "22°C",
  "condition": "Sunny"
}
```

**Data Binding**:

```json
{
  "id": "input-field",
  "component": "TextField",
  "value": { "path": "/user/name" }
}
```

---

## 5. Critical Configuration Synchronization

### 5.1 Agent ID

Must match across all three layers:

| Layer    | Location                           | Value            |
| -------- | ---------------------------------- | ---------------- |
| Frontend | `CopilotKit agent prop`            | `"sample_agent"` |
| Frontend | `CopilotChat agentId prop`         | `"sample_agent"` |
| Runtime  | `CopilotRuntime agents config key` | `"sample_agent"` |

### 5.2 Catalog ID

Must match across all three layers:

| Layer    | Location                          | Value                        |
| -------- | --------------------------------- | ---------------------------- |
| Frontend | `createCatalog catalogId`         | `"generative-agent-catalog"` |
| Runtime  | `CopilotRuntime defaultCatalogId` | `"generative-agent-catalog"` |
| Backend  | `a2ui.create_surface catalog_id`  | `"generative-agent-catalog"` |

### 5.3 A2UI Enablement

| Layer    | Setting                | Required                  |
| -------- | ---------------------- | ------------------------- |
| Frontend | `includeSchema: true`  | Yes                       |
| Runtime  | `injectA2UITool: true` | Yes                       |
| Backend  | `A2UIFixedAgent`       | Yes (for `generate_a2ui`) |

---

## 6. Running the System

### 6.1 Backend (Python)

```bash
cd backend
uv sync
python main.py
# Runs on http://localhost:8000
```

### 6.2 Agent Runtime (Bun)

```bash
cd agent-runtime
bun install
bun run index.ts
# Runs on http://localhost:4000
```

### 6.3 Frontend (React)

```bash
cd front
pnpm install
pnpm run dev
# Runs on http://localhost:5173
```

---

## 7. Known Issues and Workarounds

### 7.1 ag-ui/ag_ui Key Mismatch

**Problem**: LangGraph filters hyphenated keys (`ag-ui`), but `CopilotKitMiddleware` reads from `state.get("ag-ui")`.

**Solution**: `A2UIFixedAgent` in `main.py` adds `ag_ui` to `constant_schema_keys` and copies data from `ag-ui` to `ag_ui`.

### 7.2 CORS Configuration

**Problem**: All three layers use permissive CORS (`allow_origins=["*"]`).

**Solution**: Replace with specific origins in production.

### 7.3 State Persistence

**Problem**: `InMemoryAgentRunner` and `MemorySaver` lose state on restart.

**Solution**: Use `SqliteAgentRunner` (runtime) and `SqliteSaver` (backend) for production.

---

## 8. Maintenance Guide

### 8.1 Adding a New A2UI Component

1. **Frontend**: Define schema in `src/a2ui/definitions.ts`
2. **Frontend**: Implement renderer in `src/a2ui/renderers.tsx`
3. **Backend**: Create a tool that calls `a2ui.render()` with the component
4. **Backend**: Update `SYSTEM_PROMPT` to describe when to use the component

### 8.2 Adding a New Agent

1. **Backend**: Create new LangGraph agent
2. **Runtime**: Add new `HttpAgent` to `agents` config
3. **Frontend**: Update `agent` prop in `CopilotKit`

### 8.3 Updating Component Schema

1. **Frontend**: Update Zod schema in `definitions.ts`
2. **Frontend**: Update renderer in `renderers.tsx`
3. **Backend**: Update any tools using the component
4. The catalog automatically regenerates on next build

### 8.4 Debugging Checklist

- **A2UI not rendering**: Check `includeSchema: true` in frontend, `injectA2UITool: true` in runtime
- **Unknown component**: Verify component is defined in `definitions.ts`, check catalog ID matches
- **generate_a2ui not injected**: Enable debug logging for `copilotkit.middleware.a2ui`, check `A2UIFixedAgent` is used
- **SSE issues**: Monitor Network tab in browser DevTools

---

## 9. References

### Local Project Documentation

- [Frontend Repository Documentation](docs/front.md)
- [Agent Runtime Repository Documentation](docs/agent-runtime.md)
- [Backend Repository Documentation](docs/backend.md)

### Open-Source Repository Maintainer Guides

- [CopilotKit Repository Guide](docs/open-source/copilotkit.md) - Frontend SDK, Python middleware, A2UI renderer
- [AG-UI Repository Guide](docs/open-source/ag-ui.md) - Event protocol, SDKs, conformance
- [A2UI Repository Guide](docs/open-source/a2ui.md) - Declarative UI format, renderers, catalogs

### External Documentation

- [CopilotKit Documentation](https://docs.copilotkit.ai/)
- [AG-UI Documentation](https://docs.ag-ui.ai/)
- [A2UI Documentation](https://docs.a2ui.ai/)
- [LangGraph Documentation](https://langchain-ai.github.io/langgraph/)
