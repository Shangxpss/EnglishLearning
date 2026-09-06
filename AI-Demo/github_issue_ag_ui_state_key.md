# Bug: AG-UI State Key `ag-ui` vs `ag_ui` Inconsistency Causes A2UI Rendering Failures

## Description

When using CopilotKit with LangGraph, the AG-UI state key is inconsistently accessed as `ag-ui` (hyphenated) in the CopilotKit SDK, but in some scenarios, the state is stored with the key `ag_ui` (underscore). This mismatch causes A2UI components to fail rendering because the `a2ui_schema` and other AG-UI properties cannot be properly read from the state.

**Root Cause**: The LangGraph JSON schema validation and state merging logic converts hyphenated keys to underscore-separated keys in certain scenarios, but the CopilotKit middleware code only checks for `state["ag-ui"]` and never falls back to `state["ag_ui"]`.

## Reproduction Steps

### Prerequisites

- Python 3.10+
- LangGraph >= 1.1.0
- `ag-ui-langgraph` >= 0.0.40
- `copilotkit` >= 0.1.94

### Step 1: Set up a basic LangGraph agent with A2UI

```python
# agent.py
from langgraph.graph import StateGraph, END
from copilotkit import CopilotKitMiddleware
from langchain_openai import ChatOpenAI
from typing import TypedDict, Annotated, List
from langchain_core.messages import BaseMessage

class AgentState(TypedDict):
    messages: Annotated[List[BaseMessage], "Messages in the conversation"]
    # Note: ag-ui state is injected by middleware

async def chat_node(state: AgentState):
    model = ChatOpenAI(model="gpt-4o-mini")
    response = await model.ainvoke(state["messages"])
    return {"messages": [response]}

workflow = StateGraph(AgentState)
workflow.add_node("chat", chat_node)
workflow.add_edge("START", "chat")
workflow.add_edge("chat", END)

agent = workflow.compile(
    checkpointer=MemorySaver(),
    middleware=[CopilotKitMiddleware()]
)
```

### Step 2: Check the state during execution

Add debug logging to observe the actual state keys:

```python
async def chat_node(state: AgentState):
    # Debug: Print all state keys
    print("=== Current State Keys ===")
    print(f"All keys: {list(state.keys())}")
    print(f"Has 'ag-ui': {'ag-ui' in state}")
    print(f"Has 'ag_ui': {'ag_ui' in state}")
    print(f"ag-ui value: {state.get('ag-ui', 'NOT FOUND')}")
    print(f"ag_ui value: {state.get('ag_ui', 'NOT FOUND')}")

    model = ChatOpenAI(model="gpt-4o-mini")
    response = await model.ainvoke(state["messages"])
    return {"messages": [response]}
```

### Step 3: Observe the issue

When running the agent through a CopilotKit endpoint, you may see output like:

```
=== Current State Keys ===
All keys: ['messages', 'ag_ui', 'copilotkit']
Has 'ag-ui': False
Has 'ag_ui': True
ag-ui value: NOT FOUND
ag_ui value: {'a2ui_schema': '{"catalogId": "...", "components": [...]}', 'inject_a2ui_tool': True, ...}
```

Notice that the state contains `ag_ui` (underscore) but CopilotKit middleware code looks for `ag-ui` (hyphen).

### Step 4: Verify A2UI rendering failure

The `generate_a2ui` tool will fail to find the catalog because:

- `_resolve_a2ui_catalog()` looks for `state["ag-ui"]["a2ui_schema"]` → returns `None`
- `_a2ui_inject_decision()` looks for `state["ag-ui"]["inject_a2ui_tool"]` → returns `None`

This results in:

1. A2UI tool not being injected
2. "Unknown component" errors when trying to render A2UI surfaces
3. `a2ui_schema` not passed to sub-agents

## Expected Behavior

The CopilotKit middleware should handle both `ag-ui` (hyphen) and `ag_ui` (underscore) state keys interchangeably, since LangGraph may use either format depending on the context.

## Actual Behavior

The middleware only reads `state["ag-ui"]`, causing failures when the state is stored with `ag_ui` key.

## Code Locations to Fix

### 1. `sdk-python/copilotkit/langgraph_agui_agent.py`

**Line ~280**:

```python
# Current code:
agui_properties = merged_state.get("ag-ui", {}) or merged_state

# Should be:
agui_properties = merged_state.get("ag-ui", {}) or merged_state.get("ag_ui", {}) or merged_state
```

### 2. `sdk-python/copilotkit/copilotkit_lg_middleware.py`

**`_resolve_a2ui_catalog()` method**:

```python
# Current code:
ag_ui = state.get("ag-ui") or {}

# Should be:
ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
```

**`_a2ui_inject_decision()` method**:

```python
# Current code:
return (state.get("ag-ui") or {}).get("inject_a2ui_tool")

# Should be:
ag_ui = state.get("ag-ui") or state.get("ag-ui") or {}
return ag_ui.get("inject_a2ui_tool")
```

## Workaround

In your agent code, manually copy the state from `ag_ui` to `ag-ui`:

```python
async def chat_node(state: AgentState):
    # Workaround: Ensure both key formats are available
    if "ag_ui" in state and "ag-ui" not in state:
        state["ag-ui"] = state["ag_ui"]

    model = ChatOpenAI(model="gpt-4o-mini")
    response = await model.ainvoke(state["messages"])
    return {"messages": [response]}
```

## Environment

- **CopilotKit Version**: 0.1.94+
- **AG-UI LangGraph Version**: 0.0.40+
- **LangGraph Version**: 1.1.0+
- **Python Version**: 3.10+
- **OS**: [Your OS]

## Additional Context

This issue appears to be related to how LangGraph handles JSON schema property names during state merging. Hyphenated property names in JSON schemas may be converted to underscores internally by LangGraph, but the CopilotKit middleware expects the original hyphenated format.

The issue affects:

1. A2UI tool injection (`inject_a2ui_tool` flag)
2. A2UI catalog resolution (`a2ui_schema`)
3. Frontend tool forwarding (`tools`)
4. Context propagation (`context`)

All of these properties live under the `ag-ui`/`ag_ui` state key and are affected by this inconsistency.

---

## How to Submit a Pull Request (PR) to Fix This Issue

If you want to contribute the fix yourself, follow these step-by-step instructions:

### Prerequisites

- A GitHub account
- Git installed on your computer
- Python 3.10+ installed

### Step 1: Fork the Repository

1. Go to the CopilotKit GitHub page: https://github.com/CopilotKit/CopilotKit
2. Click the **Fork** button in the top-right corner (creates a copy under your GitHub account)

### Step 2: Clone Your Fork to Local

Open your terminal/command prompt and run:

```bash
git clone https://github.com/[YourGitHubUsername]/CopilotKit.git
cd CopilotKit
```

Replace `[YourGitHubUsername]` with your actual GitHub username.

### Step 3: Create a New Branch

Create a dedicated branch for this fix:

```bash
git checkout -b fix/ag-ui-state-key-inconsistency
```

### Step 4: Make the Code Changes

Edit the following files:

#### File 1: `sdk-python/copilotkit/copilotkit_lg_middleware.py`

**Find `_RESERVED_STATE_KEYS`** (around line ~30) and add `ag_ui`:

```python
_RESERVED_STATE_KEYS = {"ag-ui", "ag_ui", "copilotkit", ...}
```

**Find `_resolve_a2ui_catalog()` method** and update:

```python
# Before:
ag_ui = state.get("ag-ui") or {}

# After:
ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
```

**Find `_a2ui_inject_decision()` method** and update:

```python
# Before:
return (state.get("ag-ui") or {}).get("inject_a2ui_tool")

# After:
ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
return ag_ui.get("inject_a2ui_tool")
```

#### File 2: `sdk-python/copilotkit/langgraph_agui_agent.py`

**Find `langgraph_default_merge_state()`** (around line ~280) and update:

```python
# Before:
agui_properties = merged_state.get("ag-ui", {}) or merged_state

# After:
agui_properties = merged_state.get("ag-ui", {}) or merged_state.get("ag_ui", {}) or merged_state
```

### Step 5: Verify the Changes

Run the tests to make sure your changes don't break anything:

```bash
cd sdk-python
python -m pytest tests/ -v
```

If tests pass, proceed. If not, fix the issues.

### Step 6: Commit Your Changes

```bash
git add sdk-python/copilotkit/copilotkit_lg_middleware.py
git add sdk-python/copilotkit/langgraph_agui_agent.py
git commit -m "fix: handle both 'ag-ui' and 'ag_ui' state keys for LangGraph compatibility"
```

### Step 7: Push to Your Fork

```bash
git push origin fix/ag-ui-state-key-inconsistency
```

### Step 8: Create a Pull Request

1. Go to your fork: https://github.com/[YourGitHubUsername]/CopilotKit
2. You should see a banner: "Compare & pull request"
3. Click **Compare & pull request**
4. Fill in the PR details:
   - **Title**: `fix: handle both 'ag-ui' and 'ag_ui' state keys for LangGraph compatibility`
   - **Description**: Reference this issue (#5463) and explain what you changed
5. Click **Create pull request**

### Step 9: Wait for Review

The CopilotKit team will review your PR. They may:

- Approve and merge it
- Ask for changes
- Provide feedback

### After Your PR is Merged

Once merged, the fix will be included in the next release of CopilotKit. You can then:

- Update your project's `copilotkit` dependency to the new version
- Remove the workaround from your code
- Verify the issue is resolved

---

**Tip**: If you're unsure about any step, feel free to ask for help in the issue comments! The open source community is usually happy to help new contributors.
