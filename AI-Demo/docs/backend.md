# Backend Repository Documentation

## Overview

The `backend` repository is the **agent layer** of the three-tier architecture. It hosts a LangGraph-based AI agent with A2UI integration, exposed via a FastAPI endpoint.

```
Agent Runtime  ←── HTTP/SSE ──→  Backend (Python/FastAPI/LangGraph)
```

## Repository Structure

```
backend/
├── main.py              # FastAPI app + AG-UI endpoint registration
├── agent.py             # LangGraph agent definition + tools
├── test_bug3.py         # Test file for Bug 3 fix verification
└── .env                 # Environment variables (not committed)
```

## Core Components

### 1. FastAPI Application (`main.py:7`)

The main FastAPI app with CORS middleware:

```python
app = FastAPI()

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)
```

**⚠️ Production Note**: Replace `allow_origins=["*"]` with specific origins.

### 2. A2UIFixedAgent (`main.py:23-51`)

A custom `LangGraphAGUIAgent` that fixes the `ag-ui`/`ag_ui` key mismatch:

```python
class A2UIFixedAgent(LangGraphAGUIAgent):
    def __init__(self, *, name, graph, description=None, config=None):
        super().__init__(name=name, graph=graph, description=description, config=config)
        # Add 'ag_ui' to constant_schema_keys so it survives filtering
        self.constant_schema_keys = self.constant_schema_keys + ["ag_ui"]

    def langgraph_default_merge_state(self, state, messages, input):
        result = super().langgraph_default_merge_state(state, messages, input)
        # Copy 'ag-ui' (hyphen) to 'ag_ui' (underscore)
        if "ag-ui" in result:
            result["ag_ui"] = result["ag-ui"]
        return result
```

**Why this fix is needed**:

- `ag-ui-langgraph` writes state under the `ag-ui` key (with hyphen)
- Python TypedDict fields use underscores (`ag_ui`)
- LangGraph silently drops keys that don't match declared channels
- `CopilotKitMiddleware` reads from `state.get("ag-ui")`

### 3. LangGraph Agent (`agent.py:190-197`)

The LangGraph agent created with `create_agent`:

```python
agent = create_agent(
    model=llm,
    tools=[get_weather, calculate, register_user, display_register_form],
    middleware=[A2UIAwareMiddleware()],
    system_prompt=SYSTEM_PROMPT,
    checkpointer=MemorySaver(),
)
```

### 4. A2UIAwareMiddleware (`agent.py:28-29`)

Custom middleware that extends `CopilotKitMiddleware`:

```python
class A2UIAwareMiddleware(CopilotKitMiddleware):
    state_schema = AgentStateSchema
```

### 5. AgentStateSchema (`agent.py:23-26`)

Custom state schema with `ag_ui` channel:

```python
class AgentStateSchema(StateSchema):
    ag_ui: NotRequired[Annotated[dict, LastValue]]
    tools: NotRequired[Annotated[list, LastValue]]
```

## Agent Tools

### Basic Tools

| Tool            | Description                | Parameters                   |
| --------------- | -------------------------- | ---------------------------- |
| `get_weather`   | Get weather data as text   | `location: str`              |
| `calculate`     | Calculate math expressions | `expression: str`            |
| `register_user` | Register a new user        | `name: str`, `password: str` |

### A2UI Tools

| Tool                    | Description                                          |
| ----------------------- | ---------------------------------------------------- |
| `display_register_form` | Shows a registration form with name, password fields |

### display_register_form (`agent.py:136-150`)

```python
@tool
def display_register_form() -> str:
    return a2ui.render(
        operations=[
            a2ui.create_surface(REGISTER_SURFACE_ID, catalog_id=CATALOG_ID),
            a2ui.update_components(REGISTER_SURFACE_ID, REGISTER_FORM_COMPONENTS),
            a2ui.update_data_model(REGISTER_SURFACE_ID, REGISTER_FORM_DATA),
        ],
    )
```

#### Registration Form Components (`agent.py:68-132`)

| Component     | ID               | Purpose                     |
| ------------- | ---------------- | --------------------------- |
| `Card`        | `root`           | Container                   |
| `Column`      | `form-col`       | Vertical layout             |
| `Text`        | `title`          | Form title                  |
| `TextField`   | `name-field`     | Name input                  |
| `TextField`   | `password-field` | Password input (obscured)   |
| `Row`         | `btn-row`        | Button layout               |
| `Button`      | `submit-btn`     | Register button with action |
| `Text`        | `btn-label`      | Button text                 |
| `ResetButton` | `reset-btn`      | Reset form locally          |

## A2UI Integration

### a2ui.render()

Generates A2UI operations:

```python
from copilotkit import a2ui

a2ui.render(
    operations=[
        a2ui.create_surface(surface_id, catalog_id=CATALOG_ID),
        a2ui.update_components(surface_id, components),
        a2ui.update_data_model(surface_id, data),
    ],
)
```

### A2UI Operations

| Operation           | Purpose                  |
| ------------------- | ------------------------ |
| `create_surface`    | Create a new UI surface  |
| `update_components` | Add/update components    |
| `update_data_model` | Update application state |
| `delete_surface`    | Remove a surface         |

## CopilotKitMiddleware Internals

The middleware hooks into the model call lifecycle to handle A2UI:

1. **wrap_model_call**: Called before each LLM invocation
   - Extracts forwarded headers
   - Builds state note (if enabled)
   - Decides whether to inject `generate_a2ui` tool
   - Replaces `render_a2ui` with `generate_a2ui` in tools list

2. **wrap_tool_call**: Called when agent executes a tool
   - Handles `generate_a2ui` execution
   - Spawns sub-agent for UI generation

### generate_a2ui vs render_a2ui

| Aspect            | render_a2ui          | generate_a2ui               |
| ----------------- | -------------------- | --------------------------- |
| Injection         | Runtime (TypeScript) | Python middleware           |
| Caller            | Main agent directly  | Sub-agent                   |
| Schema constraint | None                 | Yes (full component schema) |
| Validation        | None                 | Yes                         |
| Retry             | None                 | Yes (up to 3 attempts)      |

## Development Workflow

### Installation

```bash
pip install -r requirements.txt
```

Or using uv (recommended):

```bash
uv sync
```

### Environment Variables

Create a `.env` file:

```env
DEEPSEEK_API_KEY=your-api-key
```

### Running

```bash
python main.py
```

Or with uvicorn:

```bash
uvicorn main:app --host 0.0.0.0 --port 8000 --reload
```

### Testing

```bash
python test_bug3.py
```

## Key Dependencies

| Package              | Purpose                     |
| -------------------- | --------------------------- |
| `copilotkit`         | CopilotKit Python SDK       |
| `ag-ui-langgraph`    | AG-UI LangGraph integration |
| `langchain`          | LLM orchestration           |
| `langchain-deepseek` | DeepSeek LLM integration    |
| `langgraph`          | State machine for agents    |
| `fastapi`            | HTTP framework              |
| `uvicorn`            | ASGI server                 |
| `python-dotenv`      | Environment variables       |

## Adding a New Tool

1. Define the tool with `@tool` decorator:

   ```python
   @tool
   def my_tool(param1: str) -> str:
       """Tool description for LLM."""
       return "result"
   ```

2. Add it to the tools list in `create_agent`:

   ```python
   agent = create_agent(
       tools=[get_weather, calculate, my_tool],
       # ...
   )
   ```

3. Update `SYSTEM_PROMPT` to describe how to use the tool.

## Adding A2UI Components

1. Define the component in the frontend `definitions.ts`
2. Implement the renderer in `renderers.tsx`
3. Create a tool that calls `a2ui.render()` with the component
4. Update the system prompt to describe when to use the component

## Debugging

### Logging

Debug logging is enabled by default in `agent.py`:

```python
logging.basicConfig(level=logging.DEBUG, force=True)
for _name in ["ag_ui_langgraph.a2ui_tool", "ag_ui_a2ui_toolkit.recovery", "copilotkit.middleware.a2ui"]:
    logging.getLogger(_name).setLevel(logging.DEBUG)
```

### Common Issues

1. **"Unknown component" error**: Component name doesn't exist in catalog
   - Verify component is defined in frontend `definitions.ts`
   - Check catalog ID matches across all layers

2. **`generate_a2ui` not injected**: `ag-ui` state key was filtered
   - Check `A2UIFixedAgent` is being used
   - Verify `ag_ui` is in `constant_schema_keys`

3. **A2UI surfaces not rendering**: Schema not reaching agent
   - Enable debug logging for `copilotkit.middleware.a2ui`
   - Check `includeSchema: true` in frontend `CopilotKit`

## Maintenance Notes

### Updating LLM Model

Change the `ChatDeepSeek` configuration in `agent.py`:

```python
llm = ChatDeepSeek(
    model="deepseek-chat",
    temperature=0.7,  # Adjust creativity
)
```

### Changing State Persistence

Replace `MemorySaver` with a persistent checkpointer:

```python
from langgraph.checkpoint.sqlite import SqliteSaver

agent = create_agent(
    # ...
    checkpointer=SqliteSaver.from_conn_string("sqlite:///agent.db"),
)
```

### Handling Actions

When users interact with A2UI components, action events are sent back to the agent. Handle them in the system prompt or tool logic:

```python
# In SYSTEM_PROMPT
"When you receive an action event named 'register' with context containing name and password, call register_user(name, password)."
```

## Known Issues

1. **CORS security**: Current configuration allows all origins. Restrict in production.
2. **MemorySaver**: State is lost on restart. Use `SqliteSaver` for production.
3. **Hardcoded catalog ID**: `CATALOG_ID` is hardcoded. Consider making it configurable.
4. **Missing authentication**: No API key validation on the FastAPI endpoint.
