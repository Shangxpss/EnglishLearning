# Issue: "Unknown component: Title" — A2UI Schema 未传递给子 Agent

## 现象

用户请求 "list a coffee list with beautiful UI" 时，前端显示：

```
Unknown component: Title
Coffee
**Espresso**
Rich & bold single shot
$3.50
...
```

LLM（DeepSeek）生成了 catalog 中不存在的 `Title` 组件，导致前端渲染器显示红色 "Unknown component: Title"。

## 根因分析

### 问题链路

```
前端 CopilotKit (includeSchema: true)
  → A2UICatalogContext 注入 schema 到 agent context
  → 通过 AG-UI 协议发送到 runtime
  → A2UIMiddleware 注入 render_a2ui 工具 + forwardedProps.injectA2UITool=true
  → ag-ui-langgraph 把 schema 放入 state["ag-ui"]["a2ui_schema"]
  → ag-ui-langgraph 把标志放入 state["ag-ui"]["inject_a2ui_tool"]

CopilotKitMiddleware._a2ui_inject_decision(state)
  → 读 state.get("ag-ui")  ← BUG: LangGraph 过滤了带连字符的 key
  → 返回 None              ← generate_a2ui 不会被注入

结果: 主 agent 只有 render_a2ui（无约束），没有 generate_a2ui（有约束）
  → 主 agent 直接调用 render_a2ui，LLM 自由发挥
  → 生成 "Title" 组件 → 前端 catalog 中无 Title → "Unknown component: Title"
```

### render_a2ui vs generate_a2ui — 为什么直接模式是问题

|                 | render_a2ui（直接模式）                                 | generate_a2ui（子 agent 模式）                        |
| --------------- | ------------------------------------------------------- | ----------------------------------------------------- |
| **谁注入**      | Runtime 的 A2UIMiddleware（TypeScript）                 | Python 的 CopilotKitMiddleware                        |
| **谁调用**      | 主 agent（LLM）直接调用                                 | 主 agent 调用 → 触发子 agent                          |
| **参数**        | `{ surfaceId, components, data }` — LLM 直接写组件 JSON | `{ prompt }` — LLM 只描述意图                         |
| **谁写组件**    | 主 agent 的 LLM 自己写                                  | 子 agent 的 LLM 写（有 schema 约束 + recovery）       |
| **schema 约束** | 无 — LLM 可以写任何组件名                               | 有 — 子 agent prompt 包含完整组件 schema              |
| **验证重试**    | 无 — 写错就错了                                         | 有 — `validate_a2ui_components` + `recovery` 自动重试 |
| **可靠性**      | 低（LLM 可能发明组件名）                                | 高（schema + 验证 + 重试）                            |

**核心问题**：`render_a2ui` 让主 agent 的 LLM **直接写组件 JSON**。LLM 没有严格的 schema 约束，可能会发明不存在的组件名（如 `Title`、`Header`、`Paragraph`）。`generate_a2ui` 通过子 agent 模式解决这个问题：子 agent 的 prompt 包含完整组件 schema，且有验证重试机制。

### 子 agent 的工作机制（源码级调用链）

```
主 agent（LLM）
  → 调用 generate_a2ui(prompt="展示咖啡列表")
    → CopilotKitMiddleware 的 wrap_tool_call 拦截
    → 从 _a2ui_tools_by_thread 取出预构建的工具
    → 执行 generate_a2ui 工具：
      → build_context_prompt(state) 读取 state["ag-ui"]["a2ui_schema"]
      → 构建子 agent 的 system prompt（包含完整组件 schema）
      → 子 agent LLM 生成组件 JSON
      → validate_a2ui_components() 检查组件名是否在 catalog 中
      → 如果无效：recovery 机制自动重试（最多 maxAttempts 次）
      → 返回有效的 A2UI 操作
    → 主 agent 收到工具结果，继续执行
  → A2UIMiddleware 拦截 render_a2ui 调用，提取 A2UI 操作
  → 发送 ACTIVITY_SNAPSHOT 事件到前端
  → 前端渲染正确的组件
```

#### 详细调用链（7 步，含源码引用）

**Step 1: Middleware 决定是否注入 `generate_a2ui`**

`CopilotKitMiddleware._a2ui_inject_decision(state)` 读取 `state["ag-ui"]["inject_a2ui_tool"]`：

```python
# copilotkit_lg_middleware.py:377-387
@staticmethod
def _a2ui_inject_decision(state: dict) -> "bool | str | None":
    return (state.get("ag-ui") or {}).get("inject_a2ui_tool")
```

此值由 Runtime 的 `A2UIMiddleware` 设置（`forwardedProps.injectA2UITool = true`），经 `LangGraphAGUIAgent` 转换为 `state["ag-ui"]["inject_a2ui_tool"]`（camelCase → snake_case）。

**Step 2: Middleware 构建 `generate_a2ui` 工具**

`CopilotKitMiddleware._maybe_build_a2ui_tool(request)` 构建工具：

```python
# copilotkit_lg_middleware.py:390-431
if not self._a2ui_inject_decision(state):
    return None

resolved = self._resolve_a2ui_catalog(state)
component_schema, catalog_id = resolved if resolved else (None, None)

kwargs: dict[str, Any] = {}
if catalog_id:
    kwargs["default_catalog_id"] = catalog_id
if component_schema:
    kwargs["composition_guide"] = component_schema
# ← Bug 3: 缺少 kwargs["catalog"] 和 kwargs["recovery"]

tool = get_a2ui_tools(request.model, **kwargs)
```

**Step 3: Middleware 替换 `render_a2ui` → `generate_a2ui`**

主 agent 的工具列表被修改，只能看到 `generate_a2ui`，看不到 `render_a2ui`：

```python
# copilotkit_lg_middleware.py:454-456
if a2ui_tool is not None:
    drop = decision if isinstance(decision, str) else "render_a2ui"
    frontend_tools = [t for t in frontend_tools
        if ((t.get("function") or {}).get("name") or t.get("name")) != drop]
```

**Step 4: 主 agent 调用 `generate_a2ui`**

`generate_a2ui` 工具定义在 `ag_ui_langgraph/a2ui_tool.py`，接受 `intent`、`target_surface_id`、`changes`：

```python
# ag_ui_langgraph/a2ui_tool.py:88-100
@tool(tool_name, description=description)
def generate_a2ui(runtime: ToolRuntime[Any], intent, target_surface_id, changes):
    messages = runtime.state.get("messages", [])[:-1]
    prep = prepare_a2ui_request(
        intent=intent, target_surface_id=target_surface_id,
        changes=changes, messages=messages, state=runtime.state,
        composition_guide=composition_guide,
    )
```

**Step 5: 构建子 agent prompt**

`prepare_a2ui_request()` 调用 `build_context_prompt(state)` 组装 prompt：

```python
# ag_ui_a2ui_toolkit/__init__.py:279-296
prompt = build_subagent_prompt(
    context_prompt=build_context_prompt(state),  # 读取 state["ag-ui"]["a2ui_schema"]
    composition_guide=composition_guide,
    edit_context=...,
)
```

`build_context_prompt(state)` 从 `state["ag-ui"]` 读取：

```python
# ag_ui_a2ui_toolkit/__init__.py:159-170
def build_context_prompt(state: dict) -> str:
    ag_ui = state.get("ag-ui", {}) or {}
    parts: list[str] = []
    a2ui_schema = ag_ui.get("a2ui_schema")  # ← Bug 1+2: 可能为空
    if a2ui_schema:
        parts.append(f"## Available Components\n{a2ui_schema}\n")
    return "\n".join(parts)
```

如果 `state["ag-ui"]` 被 LangGraph 过滤（Bug 1），这里返回空字符串 — 子 agent 没有组件定义。

**Step 6: 子 agent LLM 被调用**

子 agent 是第二个 LLM 实例，绑定 `render_a2ui` 作为强制工具调用：

```python
# ag_ui_langgraph/a2ui_tool.py:103-110
model_with_tool = model.bind_tools(
    [RENDER_A2UI_TOOL_DEF], tool_choice="render_a2ui"  # 强制子 agent 调用 render_a2ui
)

def _invoke_subagent(prompt, _attempt):
    response = model_with_tool.invoke(
        [SystemMessage(content=prompt), *messages]  # 真正的 LLM 调用
    )
    if not response.tool_calls:
        return None
    return response.tool_calls[0]["args"]  # {surfaceId, components, data}
```

`RENDER_A2UI_TOOL_DEF` 定义子 agent 必须遵循的结构化输出 schema：

```python
# ag_ui_a2ui_toolkit/__init__.py:87-115
RENDER_A2UI_TOOL_DEF = {
    "type": "function",
    "function": {
        "name": "render_a2ui",
        "parameters": {
            "type": "object",
            "properties": {
                "surfaceId": {"type": "string"},
                "components": {"type": "array", "items": {"type": "object"}},
                "data": {"type": "object"},
            },
            "required": ["surfaceId", "components"],
        },
    },
}
```

关键：`tool_choice="render_a2ui"` 强制子 agent 必须调用 `render_a2ui`，返回结构化的 `{surfaceId, components, data}`。

**Step 7: 验证 + 重试循环**

`run_a2ui_generation_with_recovery()` 驱动验证→重试循环：

```python
# ag_ui_a2ui_toolkit/recovery.py:72-99
def run_a2ui_generation_with_recovery(*, base_prompt, invoke_subagent,
                                       build_envelope, catalog=None, config=None, ...):
    max_attempts = (config or {}).get("maxAttempts", MAX_A2UI_ATTEMPTS)  # 默认 3
    for attempt in range(1, max_attempts + 1):
        prompt = augment_prompt_with_validation_errors(base_prompt, last_errors)
        args = invoke_subagent(prompt, attempt)  # 调用子 agent LLM

        if not args:
            last_errors = [{"code": "empty_components", ...}]
            continue

        components = args.get("components", [])
        result = validate_a2ui_components(components=components, data=data, catalog=catalog)
        # ← Bug 3: catalog=None → validate 永远返回 valid=True

        if result["valid"]:
            return {"envelope": build_envelope(args), "ok": True}
        last_errors = result["errors"]

    return {"envelope": _wrap_recovery_exhausted_envelope(...), "ok": False}
```

当 `catalog=None` 时，`validate_a2ui_components()` 跳过组件名检查 — 任何组件名都通过验证，包括 `Title`。

#### 完整调用链图

```
Main Agent (LLM)
  │
  ├─ 看到工具: [generate_a2ui, ...]  (render_a2ui 已被 middleware 移除)
  │
  └─ 调用 generate_a2ui(intent="create")
       │
       ├─ Step 5: prepare_a2ui_request()
       │    └─ build_context_prompt(state)
       │         └─ state["ag-ui"]["a2ui_schema"]  ← Bug 1+2: 可能为空
       │
       ├─ Step 6: model.bind_tools([RENDER_A2UI_TOOL_DEF], tool_choice="render_a2ui")
       │    └─ 子 LLM (sub-agent) 被调用
       │         ├─ 输入: [SystemMessage(prompt), *messages]
       │         └─ 输出: tool_calls[0]["args"] = {surfaceId, components, data}
       │
       ├─ Step 7: run_a2ui_generation_with_recovery()
       │    └─ validate_a2ui_components(catalog=None)  ← Bug 3: 无验证
       │         └─ valid=True (永远通过)
       │
       └─ 返回 envelope JSON → A2UIMiddleware → ACTIVITY_SNAPSHOT → 前端
            └─ 如果子 agent 生成了 "Title" → "Unknown component: Title" ❌
```

#### 三个 Bug 的连锁反应

1. **Bug 1**（ag-ui-langgraph）: `state["ag-ui"]` 被 `filter_object_by_schema_keys()` 过滤 → key 丢失
2. **Bug 2**（CopilotKitMiddleware）: AG-UI native 路径返回 `component_schema = None` → 子 agent prompt 没有组件定义
3. **Bug 3**（CopilotKitMiddleware）: `catalog` 和 `recovery` 未传给 `get_a2ui_tools()` → 验证永远通过，无重试

结果：子 agent 生成无效组件名 → 验证无法拦截 → 前端显示 "Unknown component" 错误。

### Bug 详细分析（源码级别）

#### Bug 1: `ag-ui` state key 被 LangGraph 过滤

**根因**：`ag-ui`（带连字符）不在 LangGraph 的 state schema 中。

**源码追踪**：

1. `ag-ui-langgraph` 的 `langgraph_default_merge_state()` 写入 `state["ag-ui"]`：

   ```python
   # ag_ui_langgraph/agent.py:923
   return {
       **state,
       "messages": new_messages,
       "tools": unique_tools,
       "ag-ui": ag_ui_state,  # 写入带连字符的 key
   }
   ```

2. 但 `constant_schema_keys` 不包含 `"ag-ui"`：

   ```python
   # ag_ui_langgraph/agent.py:128
   self.constant_schema_keys = ['messages', 'tools']  # 没有 'ag-ui'!
   ```

3. `filter_object_by_schema_keys()` 过滤掉不在 schema 中的 key：

   ```python
   # ag_ui_langgraph/utils.py:49
   def filter_object_by_schema_keys(obj, schema_keys):
       return {k: v for k, v in obj.items() if k in schema_keys}
   ```

4. `get_state_snapshot()` 调用过滤：

   ```python
   # ag_ui_langgraph/agent.py:982
   state = filter_object_by_schema_keys(state, [*DEFAULT_SCHEMA_KEYS, *output_keys])
   ```

5. `CopilotKitMiddleware` 只读 `state.get("ag-ui")`：
   ```python
   # copilotkit_lg_middleware.py:387
   return (state.get("ag-ui") or {}).get("inject_a2ui_tool")
   # copilotkit_lg_middleware.py:356
   ag_ui = state.get("ag-ui") or {}
   ```

**结论**：这是 **bug**，不是设计意图。`ag-ui-langgraph` 写入 `"ag-ui"` key，但 `CopilotKitMiddleware` 假设这个 key 在 LangGraph checkpoint 中存活，而实际上它被 `filter_object_by_schema_keys()` 过滤掉了。

**为什么 Python 不用连字符**：Python 的 TypedDict/Pydantic 不允许连字符作为属性名（`ag-ui` 不是合法的 Python 标识符），所以 LangGraph 的 state schema 无法声明 `ag-ui` channel。

#### Bug 2: AG-UI native 路径不传 `composition_guide`

**源码追踪**：

```python
# copilotkit_lg_middleware.py:356-363
# AG-UI native path.
ag_ui = state.get("ag-ui") or {}
a2ui_schema = ag_ui.get("a2ui_schema")
if a2ui_schema:
    ...
    # Native path: the toolkit reads `a2ui_schema` from state itself,
    # so no composition_guide is needed — just surface the catalog id.
    return None, catalog_id  # ← component_schema = None!
```

**注释说**："the toolkit reads `a2ui_schema` from state itself" — 意思是 `build_context_prompt(state)` 会从 `state["ag-ui"]["a2ui_schema"]` 读取 schema。

**这部分正确**：`build_context_prompt(state)` 确实会读 `state["ag-ui"]["a2ui_schema"]`（见 `ag_ui_a2ui_toolkit/__init__.py:159`）。

**但问题是**：由于 Bug 1，`state["ag-ui"]` 可能已经被过滤掉了，所以 `build_context_prompt(state)` 读到的也是空的。即使 `state["ag-ui"]` 存活，子 agent 的 prompt 也只有 `composition_guide`（来自 `_resolve_a2ui_catalog` 返回的 `component_schema`），而 AG-UI native 路径返回 `None`，所以子 agent 不会收到组件定义。

**结论**：这是 **bug**。注释的假设（"the toolkit reads from state itself"）在 LangGraph 过滤 `ag-ui` key 后不成立。即使不过滤，也应该传 `composition_guide` 作为备用，因为 `build_context_prompt` 读取的 state 可能不包含 `a2ui_schema`。

#### Bug 3: 未传 `catalog` 和 `recovery` 参数

**源码追踪**：

```python
# copilotkit_lg_middleware.py:413-419
kwargs: dict[str, Any] = {}
if catalog_id:
    kwargs["default_catalog_id"] = catalog_id
if component_schema:
    kwargs["composition_guide"] = component_schema
# 缺少: kwargs["catalog"] = parsed_schema
# 缺少: kwargs["recovery"] = {"maxAttempts": 3}
tool = get_a2ui_tools(request.model, **kwargs)
```

`get_a2ui_tools` 明确支持 `catalog` 和 `recovery` 参数（见 `ag_ui_langgraph/a2ui_tool.py:42-43`），且 `validate_a2ui_components` 需要 `catalog` 才能检查组件是否存在（见 `ag_ui_a2ui_toolkit/validate.py:70-73`）。

**对比 TypeScript 官方实现**：TypeScript 版本的 showcase 传了 `catalog` 和 `recovery`，但 Python middleware 遗漏了。

**结论**：这是 **bug**。缺少 `catalog` 导致 `validate_a2ui_components` 无法检查组件是否存在；缺少 `recovery` 导致即使验证失败也没有重试机制。

### Bug 总结：设计意图 vs 实际行为

| Bug   | 设计意图                                           | 实际行为                                          | 性质                                   |
| ----- | -------------------------------------------------- | ------------------------------------------------- | -------------------------------------- |
| Bug 1 | `state["ag-ui"]` 在 LangGraph checkpoint 中存活    | `filter_object_by_schema_keys()` 过滤掉了         | Bug（未考虑 LangGraph state 过滤机制） |
| Bug 2 | `build_context_prompt(state)` 从 state 读取 schema | state 中 `ag-ui` key 不存在，读不到               | Bug（假设不成立）                      |
| Bug 3 | `get_a2ui_tools` 传 `catalog` + `recovery`         | 只传了 `default_catalog_id` + `composition_guide` | Bug（遗漏参数，与 TS 版本不一致）      |

## 更好的解决方案：在 agent.py 中手动构建 generate_a2ui 工具

**不修改第三方包**，而是在 `backend/agent.py` 中直接调用 `get_a2ui_tools()` 构建 A2UI 工具，绕过 `CopilotKitMiddleware` 的 bug。

### 方案原理

根据 CopilotKit 官方文档和 TypeScript 示例（[Dynamic Schema A2UI](https://docs.copilotkit.ai/langgraph-typescript/generative-ui/a2ui/dynamic-schema)、[Voice showcase](https://docs.copilotkit.ai/langgraph-typescript/voice)），标准做法是直接调用 `getA2UITools()` 并传入 model 和 catalogId：

```typescript
// TypeScript 官方示例
const generateA2ui = getA2UITools({
  model: new ChatOpenAI({ model: "gpt-4.1" }),
  defaultCatalogId: "copilotkit://app-dashboard-catalog",
});
const tools = [getWeather, queryData, generateA2ui];
```

Python 版本也有 `get_a2ui_tools()` 函数（来自 `ag_ui_langgraph` 包），可以直接在 `agent.py` 中手动构建工具。

### 实现代码

```python
# backend/agent.py — 替换 CopilotKitMiddleware 的自动注入

from ag_ui_langgraph import get_a2ui_tools
from langchain_openai import ChatOpenAI

# 手动构建 generate_a2ui 工具，传入 catalog ID
a2ui_tool = get_a2ui_tools(
    model=ChatOpenAI(model="deepseek-chat", base_url="...", api_key="..."),
    default_catalog_id="generative-agent-catalog",
)

# 注册到 agent 的 tools 列表
graph = create_agent(
    model,
    tools=[get_weather, calculate, a2ui_tool],  # 手动添加
    # middleware=[CopilotKitMiddleware()],  # 可以移除或保留
)
```

### 优势

| 对比项             | 修改第三方包 | 手动构建工具 |
| ------------------ | ------------ | ------------ |
| 重新安装后丢失     | 是           | 否           |
| 需要清除 .pyc 缓存 | 是           | 否           |
| 升级包时冲突       | 是           | 否           |
| 可维护性           | 差           | 好           |
| 依赖官方修复       | 是           | 否           |

### 局限性

- 手动构建的 `generate_a2ui` 工具不会自动读取前端传来的 `a2ui_schema`
- 需要手动传入 `default_catalog_id`（与前端 `createCatalog` 的 ID 一致）
- 如果需要 `composition_guide`（完整组件 schema），需要额外代码从 state 中读取
- `CopilotKitMiddleware` 仍然需要保留（处理其他功能如 shared state、forwarded actions）
- `build_context_prompt(state)` 仍然读 `state.get("ag-ui")`，所以 `A2UIFixedAgent` 保留 `ag-ui` key 的做法是必要的

### 补充：在 system prompt 中约束组件使用

无论用哪种方案，都应在 `SYSTEM_PROMPT` 中明确列出可用组件，防止 LLM 发明不存在的组件名：

```python
SYSTEM_PROMPT = """\
...
Available A2UI components (ONLY use these — do NOT invent names like "Title", "Header", "Paragraph"):
- Layout: Row, Column, List, Card
- Display: Text (use variant: h1/h2/h3/h4/h5/caption/body), Image, Icon, Divider
- Interactive: Button, TextField, CheckBox, ChoicePicker, Slider, DateTimeInput
- Container: Tabs, Modal
- Media: Video, AudioPlayer
- Custom: Heading, Grid, Table, KeyValueList, StatusBadge, Metric, InfoRow

For titles/headings, use Text with variant="h1"/"h2"/"h3" or the Heading component.
NEVER use "Title" or "Header" — they do not exist.
...
"""
```

## 已应用的临时修复（修改第三方包）

> 以下修复仅作为临时方案，推荐使用上面的"手动构建工具"方案。

### 修复1: 同时读取 `ag-ui` 和 `ag_ui`

**文件**: `backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py`

```python
# _a2ui_inject_decision — 修改前
return (state.get("ag-ui") or {}).get("inject_a2ui_tool")

# _a2ui_inject_decision — 修改后
return (state.get("ag-ui") or state.get("ag_ui") or {}).get("inject_a2ui_tool")

# _resolve_a2ui_catalog — 修改前
ag_ui = state.get("ag-ui") or {}

# _resolve_a2ui_catalog — 修改后
ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
```

### 修复2: AG-UI native 路径传递 composition_guide

**文件**: `backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py`

```python
# 修改前
if a2ui_schema:
    ...
    return None, catalog_id  # 不传 schema

# 修改后
if a2ui_schema:
    ...
    schema_text = a2ui_schema if isinstance(a2ui_schema, str) else json.dumps(a2ui_schema)
    return schema_text, catalog_id  # 传完整 schema 给子 agent
```

### 修复3: 传递 catalog 和 recovery 参数

**文件**: `backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py`

```python
# 在 _maybe_build_a2ui_tool 中添加
ag_ui_state = state.get("ag-ui") or state.get("ag_ui") or {}
_a2ui_schema_raw = ag_ui_state.get("a2ui_schema")
if _a2ui_schema_raw:
    try:
        _parsed_schema = json.loads(_a2ui_schema_raw) if isinstance(_a2ui_schema_raw, str) else _a2ui_schema_raw
        if isinstance(_parsed_schema, dict):
            kwargs["catalog"] = _parsed_schema
            kwargs["recovery"] = {"maxAttempts": 3}
    except (TypeError, ValueError):
        pass
```

## 修复效果

修复前：

- tools 列表: `get_weather`, `calculate`, `render_a2ui` ← 主 agent 直接调用，无 schema 约束
- 结果: LLM 生成 `Title` 组件 → "Unknown component: Title"

修复后：

- tools 列表: `get_weather`, `calculate`, `generate_a2ui` ← 子 agent 模式，有 schema 约束 + recovery
- 结果: 子 agent 只使用 catalog 中存在的组件（Text, Card, Row, Column 等）

## 验证方法

1. 启动服务: `pnpm run dev`
2. 在浏览器中发送 "list a coffee list with beautiful UI"
3. 检查是否还有 "Unknown component: Title" 错误
4. 检查终端日志中 tools 列表是否包含 `generate_a2ui`（而非 `render_a2ui`）

## 注意事项

- 修改的文件位于 `.venv/lib/python3.12/site-packages/`，是第三方包
- 重新安装 `copilotkit` 包后，修改会被覆盖
- 长期方案：向 CopilotKit 提交 PR 修复此问题
- `dev:frontend` 命令报 "No projects matched the filters"，需要单独修复 pnpm workspace 配置

## 调试日志

已在以下文件添加 `[A2UI-DEBUG]` 调试日志（使用 `print()` + `flush=True`）：

1. `copilotkit_lg_middleware.py` — `wrap_model_call` 入口：打印 state keys、`ag-ui`/`ag_ui` 是否存在
2. `copilotkit_lg_middleware.py` — `_maybe_build_a2ui_tool`：打印 `_resolve_a2ui_catalog` 结果、`get_a2ui_tools` kwargs
3. `ag_ui_langgraph/a2ui_tool.py` — `generate_a2ui` 工具：打印子 agent prompt、输出组件类型
4. `ag_ui_a2ui_toolkit/recovery.py` — recovery 循环：打印验证结果

注意：uvicorn 的 reload 机制可能缓存 `.pyc` 文件，修改后需清除缓存：

```bash
find backend/.venv/lib/python3.12/site-packages/copilotkit/__pycache__ -name "copilotkit_lg_middleware*" -delete
```

## 参考链接

- [CopilotKit Dynamic Schema A2UI 文档](https://docs.copilotkit.ai/langgraph-typescript/generative-ui/a2ui/dynamic-schema)
- [CopilotKit Voice Showcase（含 getA2UITools 用法）](https://docs.copilotkit.ai/langgraph-typescript/voice)
- [A2UI 官方组件参考](https://a2ui.org/reference/components/)
- [A2UI Renderer 实现指南](https://a2ui.org/guides/renderer-development/)
- [AG-UI LangGraph 集成](https://checklist.day/registry/ag-ui-langgraph)

## 源码参考

| 文件                                     | 关键行                                         | 说明                                                                     |
| ---------------------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------ |
| `ag_ui_langgraph/agent.py:128`           | `constant_schema_keys = ['messages', 'tools']` | 不包含 `ag-ui`，导致 state 过滤                                          |
| `ag_ui_langgraph/agent.py:923`           | `"ag-ui": ag_ui_state`                         | 写入带连字符的 key                                                       |
| `ag_ui_langgraph/utils.py:49`            | `filter_object_by_schema_keys()`               | 过滤掉不在 schema 中的 key                                               |
| `copilotkit_lg_middleware.py:377-387`    | `_a2ui_inject_decision(state)`                 | 只读 `state["ag-ui"]`，不读 `state["ag_ui"]`                             |
| `copilotkit_lg_middleware.py:356-363`    | `_resolve_a2ui_catalog()` AG-UI native 路径    | 返回 `None` 作为 `component_schema`                                      |
| `copilotkit_lg_middleware.py:413-419`    | `get_a2ui_tools(request.model, **kwargs)`      | 缺少 `catalog` 和 `recovery` 参数                                        |
| `copilotkit_lg_middleware.py:454-456`    | `wrap_model_call` 替换 render_a2ui             | 移除 render_a2ui，添加 generate_a2ui                                     |
| `ag_ui_langgraph/a2ui_tool.py:88-100`    | `generate_a2ui` 工具定义                       | 主 agent 调用入口，调用 prepare_a2ui_request                             |
| `ag_ui_langgraph/a2ui_tool.py:103-110`   | `_invoke_subagent()`                           | 子 agent LLM 调用，绑定 RENDER_A2UI_TOOL_DEF + tool_choice="render_a2ui" |
| `ag_ui_a2ui_toolkit/__init__.py:87-115`  | `RENDER_A2UI_TOOL_DEF`                         | 子 agent 的结构化输出 schema（surfaceId, components, data）              |
| `ag_ui_a2ui_toolkit/__init__.py:159-170` | `build_context_prompt(state)`                  | 从 `state["ag-ui"]["a2ui_schema"]` 读取 schema                           |
| `ag_ui_a2ui_toolkit/__init__.py:279-296` | `prepare_a2ui_request()`                       | 组装子 agent prompt（context + composition_guide + edit_context）        |
| `ag_ui_a2ui_toolkit/recovery.py:72-99`   | `run_a2ui_generation_with_recovery()`          | 验证→重试循环，catalog=None 时验证无效                                   |
| `ag_ui_a2ui_toolkit/validate.py:70-73`   | `validate_a2ui_components()`                   | 需要 `catalog` 参数才能验证组件                                          |
| `@ag-ui/a2ui-middleware/src/index.ts`    | `A2UIMiddleware.run()`                         | Runtime 端注入 render_a2ui + 拦截事件流                                  |
| `@ag-ui/a2ui-middleware/src/tools.ts`    | `RENDER_A2UI_TOOL`                             | render_a2ui 工具定义（无 schema 约束）                                   |

---

## 生产部署补丁指南

部署到生产环境时，需要重新安装 Python 依赖，以下补丁会丢失。每次 `pip install` 或 `uv sync` 后，必须重新应用这些补丁。

### 受影响的包及版本

| 包名                 | 版本   | 补丁文件                                 | 必需?      |
| -------------------- | ------ | ---------------------------------------- | ---------- |
| `copilotkit`         | 0.1.94 | `copilotkit/copilotkit_lg_middleware.py` | **是**     |
| `ag-ui-a2ui-toolkit` | 0.0.2  | `ag_ui_a2ui_toolkit/__init__.py`         | **是**     |
| `ag-ui-langgraph`    | 0.0.40 | `ag_ui_langgraph/agent.py`               | 否（见下） |

> **为什么 `ag-ui-langgraph` 不需要补丁？** 之前的 Patch 2a（添加 `'ag-ui'` 到 `constant_schema_keys`）只影响 `get_state_snapshot()` 中的 `filter_object_by_schema_keys()` 过滤。但 middleware 读取的是 LangGraph 原始 state dict，不经过 `get_state_snapshot()` 过滤。而且 `A2UIFixedAgent` 已经将 `"ag_ui"` 添加到 `constant_schema_keys`，足以让 STATE_SNAPSHOT 事件包含 A2UI 数据。

### 根因：为什么需要这些补丁？

所有补丁都源于同一个根因：**LangGraph 无法在 TypedDict 中声明 `ag-ui`（连字符）作为 channel**，所以 `ag-ui` key 在 LangGraph graph 执行后被丢弃。`A2UIFixedAgent` 将数据复制到 `ag_ui`（下划线）使其存活，但第三方包只读 `state["ag-ui"]`，导致读到空值。

```
langgraph_default_merge_state() 写入 state["ag-ui"] = {...}
  → LangGraph graph 执行，ag-ui 不是 TypedDict channel → 被丢弃
  → A2UIFixedAgent 复制到 state["ag_ui"] = {...} → 存活

第三方包读 state.get("ag-ui") → None ❌
应该读 state.get("ag_ui") → 有值 ✅
```

### 补丁 1: `copilotkit/copilotkit_lg_middleware.py` (3 处修改)

**路径**: `backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py`

#### 1a. `_resolve_a2ui_catalog` 方法 — 同时读取 `ag-ui` 和 `ag_ui`

**行号**: ~345

```python
# ── 原始代码 ──
        # AG-UI native path.
        ag_ui = state.get("ag-ui") or {}

# ── 替换为 ──
        # AG-UI native path.
        # Try both "ag-ui" and "ag_ui" — LangGraph drops hyphenated keys
        # that aren't declared TypedDict channels; A2UIFixedAgent copies
        # the data to "ag_ui" (underscore) so it survives.
        ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
```

#### 1b. `_a2ui_inject_decision` 方法 — 同时读取 `ag-ui` 和 `ag_ui`

**行号**: ~392

```python
# ── 原始代码 ──
        return (state.get("ag-ui") or {}).get("inject_a2ui_tool")

# ── 替换为 ──
        return (state.get("ag-ui") or state.get("ag_ui") or {}).get("inject_a2ui_tool")
```

#### 1c. `_maybe_build_a2ui_tool` 方法 — 传递 `catalog` 和 `recovery` 参数

**行号**: ~436（在 `if component_schema:` 块之后，`tool = get_a2ui_tools(...)` 之前）

> 这是**独立的 bug**，与 ag-ui/ag_ui key 问题无关。即使 key 正确，`catalog` 和 `recovery` 也未被传递。

```python
# ── 原始代码 ──
        if component_schema:
            kwargs["composition_guide"] = component_schema

        tool = get_a2ui_tools(request.model, **kwargs)

# ── 替换为 ──
        if component_schema:
            kwargs["composition_guide"] = component_schema

        # Pass catalog for component validation + recovery for auto-retry.
        ag_ui_state = state.get("ag-ui") or state.get("ag_ui") or {}
        a2ui_schema_raw = ag_ui_state.get("a2ui_schema")
        if a2ui_schema_raw:
            try:
                parsed_schema = (
                    json.loads(a2ui_schema_raw)
                    if isinstance(a2ui_schema_raw, str)
                    else a2ui_schema_raw
                )
                if isinstance(parsed_schema, dict):
                    kwargs["catalog"] = parsed_schema
                    kwargs["recovery"] = {"maxAttempts": 3}
            except (TypeError, ValueError):
                pass

        tool = get_a2ui_tools(request.model, **kwargs)
```

---

### 补丁 2: `ag_ui_a2ui_toolkit/__init__.py` (1 处修改)

**路径**: `backend/.venv/lib/python3.12/site-packages/ag_ui_a2ui_toolkit/__init__.py`

#### 2a. `build_context_prompt` 函数 — 同时读取 `ag-ui` 和 `ag_ui`

**行号**: ~163

```python
# ── 原始代码 ──
    ag_ui = state.get("ag-ui", {}) or {}

# ── 替换为 ──
    # Try both "ag-ui" and "ag_ui" — LangGraph drops hyphenated keys
    # that aren't declared TypedDict channels; A2UIFixedAgent copies
    # the data to "ag_ui" (underscore) so it survives.
    ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}
```

**作用**: 子 agent 的 prompt 通过 `build_context_prompt(state)` 构建。如果只读 `state["ag-ui"]`，而 LangGraph 将数据存在 `state["ag_ui"]` 下，子 agent 就收不到组件 schema，导致生成无效组件名（如 `Title`）。

---

### 快速补丁脚本

部署后运行以下脚本自动应用所有补丁：

```bash
#!/bin/bash
# apply_a2ui_patches.sh — 在 pip install / uv sync 之后运行
set -e

VENV_SITE="backend/.venv/lib/python3.12/site-packages"

echo "Applying A2UI patches..."

python3 << PYEOF
import os, glob

site = os.environ.get("VENV_SITE", "backend/.venv/lib/python3.12/site-packages")

# ── Patch 1: copilotkit_lg_middleware.py (3 处) ──
path = os.path.join(site, "copilotkit", "copilotkit_lg_middleware.py")
with open(path, "r") as f:
    content = f.read()

# 1a: _resolve_a2ui_catalog
content = content.replace(
    '        # AG-UI native path.\n        ag_ui = state.get("ag-ui") or {}',
    '        # AG-UI native path.\n'
    '        # Try both "ag-ui" and "ag_ui" — LangGraph drops hyphenated keys\n'
    '        # that aren\'t declared TypedDict channels; A2UIFixedAgent copies\n'
    '        # the data to "ag_ui" (underscore) so it survives.\n'
    '        ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}'
)

# 1b: _a2ui_inject_decision
content = content.replace(
    'return (state.get("ag-ui") or {}).get("inject_a2ui_tool")',
    'return (state.get("ag-ui") or state.get("ag_ui") or {}).get("inject_a2ui_tool")'
)

# 1c: catalog + recovery kwargs
catalog_patch = '''        # Pass catalog for component validation + recovery for auto-retry.
        ag_ui_state = state.get("ag-ui") or state.get("ag_ui") or {}
        a2ui_schema_raw = ag_ui_state.get("a2ui_schema")
        if a2ui_schema_raw:
            try:
                parsed_schema = (
                    json.loads(a2ui_schema_raw)
                    if isinstance(a2ui_schema_raw, str)
                    else a2ui_schema_raw
                )
                if isinstance(parsed_schema, dict):
                    kwargs["catalog"] = parsed_schema
                    kwargs["recovery"] = {"maxAttempts": 3}
            except (TypeError, ValueError):
                pass

'''
marker = "        tool = get_a2ui_tools(request.model, **kwargs)"
if 'kwargs["catalog"]' not in content and marker in content:
    content = content.replace(marker, catalog_patch + marker)

with open(path, "w") as f:
    f.write(content)
print(f"Patched: {path}")

# ── Patch 2: ag_ui_a2ui_toolkit/__init__.py (1 处) ──
path = os.path.join(site, "ag_ui_a2ui_toolkit", "__init__.py")
with open(path, "r") as f:
    content = f.read()
content = content.replace(
    '    ag_ui = state.get("ag-ui", {}) or {}',
    '    # Try both "ag-ui" and "ag_ui" — LangGraph drops hyphenated keys\n'
    '    # that aren\'t declared TypedDict channels; A2UIFixedAgent copies\n'
    '    # the data to "ag_ui" (underscore) so it survives.\n'
    '    ag_ui = state.get("ag-ui") or state.get("ag_ui") or {}'
)
with open(path, "w") as f:
    f.write(content)
print(f"Patched: {path}")

# ── Clear .pyc cache ──
for pattern in [
    f"{site}/copilotkit/__pycache__/copilotkit_lg_middleware*",
    f"{site}/ag_ui_a2ui_toolkit/__pycache__/*",
]:
    for f in glob.glob(pattern):
        os.remove(f)
        print(f"Removed cache: {f}")

print("\nAll A2UI patches applied successfully!")
PYEOF
```

### 验证补丁是否生效

```bash
# 检查 copilotkit 补丁（应输出 3 行：1a, 1b, 1c）
grep -n 'state.get("ag_ui")' backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py

# 检查 a2ui-toolkit 补丁（应输出 1 行）
grep -n 'state.get("ag_ui")' backend/.venv/lib/python3.12/site-packages/ag_ui_a2ui_toolkit/__init__.py

# 检查 catalog + recovery 是否已注入
grep -n 'kwargs\["catalog"\]' backend/.venv/lib/python3.12/site-packages/copilotkit/copilotkit_lg_middleware.py
```

### 注意事项

1. **每次 `uv sync` 或 `pip install` 后必须重新应用补丁**，因为依赖会被重新安装
2. **升级包版本时需检查补丁是否仍然适用** — 新版本可能已修复这些 bug，或代码结构有变化
3. **清除 `.pyc` 缓存** — Python 会缓存字节码，修改源码后必须删除 `__pycache__` 中的对应文件
4. **关注官方修复** — CopilotKit/ag-ui-langgraph 的后续版本可能已修复这些问题，届时可移除补丁
5. **考虑 fork + patch 方案** — 如果频繁部署，建议 fork 这两个包，应用补丁后用 `pip install -e` 或私有 PyPI 安装
