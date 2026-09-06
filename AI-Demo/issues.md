# CopilotKit + A2UI + LangGraph Integration Bugs

## Issue 1: A2UI Schema Not Passed to Sub-Agent

### Symptom

When requesting "list a coffee list with beautiful UI", the frontend displays:

```
Unknown component: Title
Coffee
**Espresso**
Rich & bold single shot
$3.50
...
```

LLM generates `Title` component which doesn't exist in the catalog.

### Root Cause

Three interconnected bugs prevent the A2UI schema from reaching the sub-agent:

#### Bug 1.1: `ag-ui` State Key Filtered by LangGraph

**Package**: `ag-ui-langgraph`

**Source**: `ag_ui_langgraph/agent.py:128` / `ag_ui_langgraph/utils.py:49`

**Problem**: `state["ag-ui"]` (with hyphen) is not declared in LangGraph's TypedDict schema keys, so it gets filtered out by `filter_object_by_schema_keys()`.

```python
# constant_schema_keys doesn't include "ag-ui"
self.constant_schema_keys = ['messages', 'tools']

# filter_object_by_schema_keys removes keys not in schema
def filter_object_by_schema_keys(obj, schema_keys):
    return {k: v for k, v in obj.items() if k in schema_keys}
```

#### Bug 1.2: AG-UI Native Path Returns `None` for `component_schema`

**Package**: `copilotkit`

**Source**: `copilotkit/copilotkit_lg_middleware.py:356-363`

**Problem**: The AG-UI native path assumes `build_context_prompt(state)` reads schema from state, but due to Bug 1.1, `state["ag-ui"]` is empty.

```python
# Returns None as component_schema
if a2ui_schema:
    ...
    return None, catalog_id  # ← Bug: should return schema_text, catalog_id
```

#### Bug 1.3: Missing `catalog` and `recovery` Parameters

**Package**: `copilotkit`

**Source**: `copilotkit/copilotkit_lg_middleware.py:413-419`

**Problem**: `get_a2ui_tools()` is called without `catalog` (needed for component validation) and `recovery` (needed for auto-retry).

```python
kwargs: dict[str, Any] = {}
if catalog_id:
    kwargs["default_catalog_id"] = catalog_id
if component_schema:
    kwargs["composition_guide"] = component_schema
# Missing: kwargs["catalog"] = parsed_schema
# Missing: kwargs["recovery"] = {"maxAttempts": 3}
tool = get_a2ui_tools(request.model, **kwargs)
```

### Impact Chain

```
Bug 1 → state["ag-ui"] empty → Bug 2 → component_schema=None → Bug 3 → no validation/recovery
                                                                      ↓
                                                    Sub-agent generates "Title" → no validation passes it → Frontend "Unknown component: Title"
```

### render_a2ui vs generate_a2ui Comparison

| Feature | render_a2ui (Direct) | generate_a2ui (Sub-agent) |
|---------|----------------------|---------------------------|
| **Injector** | Runtime A2UIMiddleware | Python CopilotKitMiddleware |
| **Caller** | Main LLM directly | Main LLM → triggers sub-agent |
| **Schema Constraint** | None - LLM invents names | Yes - sub-agent prompt includes schema |
| **Validation** | None | `validate_a2ui_components()` + recovery |
| **Reliability** | Low | High |

### Fix Summary

1. **Bug 1**: Read both `state["ag-ui"]` and `state["ag_ui"]` (A2UIFixedAgent copies to underscore variant)
2. **Bug 2**: Return `schema_text` instead of `None` in AG-UI native path
3. **Bug 3**: Pass `catalog` and `recovery` parameters to `get_a2ui_tools()`

---

## Issue 2: `ag-ui-langgraph` State Filtering

### Symptom

`state["ag-ui"]` data disappears between `langgraph_default_merge_state()` writing it and `CopilotKitMiddleware` reading it.

### Root Cause

**Source**: `ag_ui_langgraph/agent.py:923` / `ag_ui_langgraph/agent.py:128`

```python
# Data is written with hyphen
return {
    ...,
    "ag-ui": ag_ui_state,
}

# But "ag-ui" is not in the schema
self.constant_schema_keys = ['messages', 'tools']  # No 'ag-ui'!
```

**Why**: Python TypedDict/Pydantic doesn't allow hyphens in attribute names (`ag-ui` is not a valid Python identifier), so LangGraph cannot declare it as a channel.

### Fix

Read both `state["ag-ui"]` and `state["ag_ui"]` (A2UIFixedAgent copies data to underscore variant).

---

## Issue 3: Sub-Agent Prompt Missing Component Schema

### Symptom

Sub-agent LLM generates invalid component names because its prompt doesn't include the A2UI component catalog schema.

### Root Cause

**Source**: `ag_ui_a2ui_toolkit/__init__.py:159-170`

```python
def build_context_prompt(state: dict) -> str:
    ag_ui = state.get("ag-ui", {}) or {}  # ← Only reads hyphenated key
    a2ui_schema = ag_ui.get("a2ui_schema")  # ← Empty due to Issue 2
    if a2ui_schema:
        parts.append(f"## Available Components\n{a2ui_schema}\n")
    return "\n".join(parts)  # ← Returns empty string
```

### Fix

Read both `state["ag-ui"]` and `state["ag_ui"]` for the A2UI schema.

---

## Issue 4: Component Validation Disabled

### Symptom

Invalid component names like `Title` pass validation and reach the frontend.

### Root Cause

**Source**: `ag_ui_a2ui_toolkit/recovery.py:72-99`

```python
def run_a2ui_generation_with_recovery(*, catalog=None, ...):
    ...
    result = validate_a2ui_components(components=components, catalog=catalog)
    # catalog=None → validate_a2ui_components() skips component name checks
    if result["valid"]:  # ← Always True when catalog is None
        return {"envelope": build_envelope(args), "ok": True}
```

### Fix

Pass `catalog` parameter to `get_a2ui_tools()` so validation can check component names against the catalog.

---

## Issue 5: Recovery Mechanism Disabled

### Symptom

When LLM generates invalid components, there's no automatic retry.

### Root Cause

**Source**: `copilotkit/copilotkit_lg_middleware.py:413-419`

```python
# recovery parameter is never passed
kwargs: dict[str, Any] = {}
# ... no recovery kwarg ...
tool = get_a2ui_tools(request.model, **kwargs)
```

### Fix

Pass `recovery={"maxAttempts": 3}` to `get_a2ui_tools()` to enable auto-retry when validation fails.

---

## Summary of Affected Packages

| Package | Version | File | Bug | Fix |
|---------|---------|------|-----|-----|
| `copilotkit` | 0.1.94 | `copilotkit_lg_middleware.py` | 1.2, 1.3 | Read both keys, pass catalog/recovery |
| `ag-ui-a2ui-toolkit` | 0.0.2 | `__init__.py` | 3 | Read both keys |
| `ag-ui-langgraph` | 0.0.40 | `agent.py` | 2 | Add 'ag_ui' to schema keys |

---

## Recommended Solution

**Better than patching third-party packages**: Manually construct `generate_a2ui` tool in `agent.py`:

```python
from ag_ui_langgraph import get_a2ui_tools
from langchain_openai import ChatOpenAI

a2ui_tool = get_a2ui_tools(
    model=ChatOpenAI(model="deepseek-chat", base_url="...", api_key="..."),
    default_catalog_id="generative-agent-catalog",
)

graph = create_agent(
    model,
    tools=[get_weather, calculate, a2ui_tool],
)
```

**Advantages**:
- Survives package reinstallations
- No `.pyc` cache issues
- No conflicts during package upgrades
- Better maintainability

**Supplement**: Add component constraint to SYSTEM_PROMPT:

```python
SYSTEM_PROMPT = """\
...
Available A2UI components (ONLY use these):
- Layout: Row, Column, List, Card
- Display: Text (variant: h1/h2/h3/h4/h5/caption/body), Image, Icon, Divider
- Interactive: Button, TextField, CheckBox, ChoicePicker, Slider, DateTimeInput
- Container: Tabs, Modal
- Media: Video, AudioPlayer
- Custom: Heading, Grid, Table, KeyValueList, StatusBadge, Metric, InfoRow

For titles, use Text with variant="h1"/"h2"/"h3" or the Heading component.
NEVER use "Title" or "Header" — they do not exist.
...
"""
```

---

## References

- [CopilotKit Dynamic Schema A2UI](https://docs.copilotkit.ai/langgraph-typescript/generative-ui/a2ui/dynamic-schema)
- [CopilotKit Voice Showcase](https://docs.copilotkit.ai/langgraph-typescript/voice)
- [A2UI Official Components](https://a2ui.org/reference/components/)
- [A2UI Renderer Guide](https://a2ui.org/guides/renderer-development/)