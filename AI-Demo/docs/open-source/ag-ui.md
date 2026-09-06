# AG-UI Repository Maintainer Guide

## Overview

**AG-UI** (Agent-User Interaction Protocol) is an open, lightweight, event-based protocol that standardizes how AI agents connect to user-facing applications. It was created by CopilotKit and is now adopted by Google, LangChain, AWS, Microsoft, Mastra, and PydanticAI.

- **Repository**: [ag-ui-protocol/ag-ui](https://github.com/ag-ui-protocol/ag-ui)
- **License**: MIT
- **Protocol Version**: Active development, v1.0+ adopted by partners

## Protocol Stack Position

AG-UI sits at the top of the agent protocol stack:
- **MCP** (Model Context Protocol): Gives agents tools
- **A2A** (Agent-to-Agent Protocol): Agents communicate with other agents
- **AG-UI**: Brings agents into user-facing applications

## Repository Structure

```
ag-ui/
├── packages/                      # Protocol implementations
│   ├── core/                     # Core TypeScript types and utilities
│   │   ├── src/
│   │   │   ├── index.ts         # Main exports
│   │   │   ├── events/         # Event type definitions
│   │   │   ├── types/          # Protocol types
│   │   │   └── encoder/        # Event encoding/decoding
│   │   └── package.json
│   ├── client/                   # Client-side implementation
│   │   ├── src/
│   │   │   ├── index.ts        # Client exports
│   │   │   ├── client/        # HTTP client with SSE support
│   │   │   ├── subscriber/     # Event subscribers
│   │   │   └── types/         # Client types
│   │   └── package.json
│   ├── python/                   # Python SDK
│   │   ├── src/
│   │   │   ├── core/          # Core Python types
│   │   │   ├── client/        # Python HTTP client
│   │   │   └── encoder/       # Event encoding
│   │   └── pyproject.toml
│   ├── java/                    # Java SDK
│   │   ├── src/
│   │   │   └── main/
│   │   │       └── java/
│   │   │           └── com/
│   │   │               └── agui/
│   │   │                   └── client/
│   └── ...                      # Other language SDKs (Go, Dart, Kotlin)
├── apps/
│   ├── client-cli-example/     # CLI example application
│   │   └── src/
│   │       ├── agent.ts        # Example agent
│   │       └── index.ts        # CLI entry point
│   └── dojo/                   # Interactive demo application
│       ├── e2e/               # E2E tests
│       └── src/
│           ├── app/           # Next.js app
│           ├── components/    # UI components
│           ├── mastra/        # Mastra agent integration
│           └── lib/           # Utilities
└── docs/                       # Protocol documentation
    ├── concepts/              # Protocol concepts
    ├── quickstart/            # Quick start guides
    └── sdk/                   # SDK documentation
```

## Core Concepts

### Event-Driven Architecture

AG-UI uses events as the fundamental units of communication:

1. **Agent emits events** → Client receives and processes
2. **Client sends events** → Agent receives and processes
3. **Bi-directional streaming** via SSE (Server-Sent Events)

### Event Types

#### Lifecycle Events

| Event | Description | Required |
|-------|-------------|----------|
| `RUN_STARTED` | Agent execution begins | Yes |
| `RUN_FINISHED` | Agent execution completes | Yes |
| `RUN_ERROR` | Agent execution fails | No |
| `STEP_STARTED` | Processing step begins | No |
| `STEP_FINISHED` | Processing step completes | No |

#### Text Events

| Event | Description |
|-------|-------------|
| `TEXT_MESSAGE_START` | Start of text message |
| `TEXT_MESSAGE_CHUNK` | Streaming text content |
| `TEXT_MESSAGE_END` | End of text message |

#### Tool Events

| Event | Description |
|-------|-------------|
| `TOOL_CALL_STARTED` | Tool execution begins |
| `TOOL_CALL_ARGS_CHUNK` | Streaming tool arguments |
| `TOOL_CALL_RESULT` | Tool execution result |

#### State Events

| Event | Description |
|-------|-------------|
| `STATE_SNAPSHOT` | Full state snapshot |
| `STATE_PATCH` | Incremental state update |

#### Activity Events

| Event | Description |
|-------|-------------|
| `ACTIVITY_SNAPSHOT` | A2UI surface update |
| `ACTIVITY_INTERRUPT` | Agent paused for input |

## Key Packages

### @ag-ui/core

**Purpose**: Core TypeScript types and utilities for AG-UI protocol.

**Entry Point**: `packages/core/src/index.ts`

**Main Exports**:
```typescript
// Event types
export type {
  AGUIEvent,
  RunStartedEvent,
  RunFinishedEvent,
  RunErrorEvent,
  StepStartedEvent,
  StepFinishedEvent,
  TextMessageStartEvent,
  TextMessageChunkEvent,
  TextMessageEndEvent,
  ToolCallStartedEvent,
  ToolCallArgsChunkEvent,
  ToolCallResultEvent,
  StateSnapshotEvent,
  StatePatchEvent,
  ActivitySnapshotEvent,
} from "./events";

// Encoding
export { encodeEvent, decodeEvent } from "./encoder";

// Types
export type { AGUIEventType } from "./types";
```

**Key Source Files**:
| File | Purpose |
|------|---------|
| `events/index.ts` | All event type definitions |
| `types/index.ts` | Base types and enums |
| `encoder/index.ts` | Event encoding/decoding |

### @ag-ui/client

**Purpose**: Client-side HTTP client with SSE support.

**Entry Point**: `packages/client/src/index.ts`

**Main Exports**:
```typescript
// Client
export { AGUIClient } from "./client";
export type { AGUIClientOptions } from "./client";

// Subscribers
export type { AGUIEventSubscriber } from "./subscriber";
export { inMemorySubscriber } from "./subscriber";

// Types
export type { AgentRunRequest, AgentRunResponse } from "./types";
```

**Key Source Files**:
| File | Purpose |
|------|---------|
| `client/client.ts` | Main client implementation |
| `client/sse.ts` | SSE connection handling |
| `subscriber/index.ts` | Event subscriber interface |

### ag-ui-python (Python SDK)

**Purpose**: Python implementation of AG-UI protocol.

**Entry Point**: `packages/python/src/ag_ui/__init__.py`

**Main Components**:
```python
from ag_ui import (
    # Core
    AgentProtocolClient,
    SSEClient,
    
    # Events
    RunStartedEvent,
    RunFinishedEvent,
    TextMessageChunkEvent,
    
    # Encoder
    encode_event,
    decode_event,
)
```

## Event Flow

### Typical Agent Run

```
1. Client sends RunAgentInput
   ↓
2. Agent emits RUN_STARTED
   ↓
3. Agent may emit STEP_STARTED / STEP_FINISHED
   ↓
4. Agent emits TEXT_MESSAGE_CHUNK (streaming)
   ↓
5. Agent may emit TOOL_CALL_STARTED / TOOL_CALL_RESULT
   ↓
6. Agent emits RUN_FINISHED
   ↓
7. Client processes final state
```

### A2UI Integration

```
Agent generates UI definition
   ↓
Emits ACTIVITY_SNAPSHOT with A2UI payload
   ↓
Client renders A2UI components
   ↓
User interacts with UI
   ↓
Client sends action event back
   ↓
Agent processes and responds
```

## Development Workflow

### Setting Up

```bash
# Clone the repository
git clone https://github.com/ag-ui-protocol/ag-ui.git
cd ag-ui

# Install dependencies (requires pnpm)
pnpm install

# Build all packages
pnpm build

# Run tests
pnpm test
```

### Running the Dojo (Demo App)

```bash
cd apps/dojo
pnpm install
pnpm dev
# Opens at http://localhost:3000
```

### Testing Protocol Implementations

```bash
# Run core tests
pnpm test --filter=@ag-ui/core

# Run client tests
pnpm test --filter=@ag-ui/client

# Run Python tests
cd packages/python
uv run pytest
```

## Creating a New SDK

### 1. Define Event Types

```typescript
// events.ts
export interface RunStartedEvent {
  type: "RUN_STARTED";
  threadId: string;
  runId: string;
  parentRunId?: string;
  input?: unknown;
}
```

### 2. Implement Encoder

```typescript
// encoder.ts
export function encodeEvent(event: AGUIEvent): string {
  return JSON.stringify(event);
}

export function decodeEvent(data: string): AGUIEvent {
  return JSON.parse(data);
}
```

### 3. Implement SSE Client

```typescript
// client.ts
export class AGUIClient {
  async *streamEvents(request: RunAgentInput): AsyncGenerator<AGUIEvent> {
    const response = await fetch("/agent/run", {
      method: "POST",
      body: JSON.stringify(request),
      headers: { "Content-Type": "application/json" },
    });

    const reader = response.body?.getReader();
    const decoder = new TextDecoder();

    while (reader) {
      const { done, value } = await reader.read();
      if (done) break;
      yield decodeEvent(decoder.decode(value));
    }
  }
}
```

## Protocol Specifications

### Event Contract Rules

1. **Required Events**: Every run MUST emit `RUN_STARTED` and either `RUN_FINISHED` or `RUN_ERROR`
2. **Ordering**: Events must be emitted in chronological order
3. **Timestamps**: Events may include optional `timestamp` field
4. **IDs**: `threadId` and `runId` must be consistent across related events

### State Management

- **STATE_SNAPSHOT**: Full state replacement
- **STATE_PATCH**: Incremental updates (JSON Patch format)
- **Filtering**: Clients filter events by `threadId` and `runId`

### Interrupt Handling

AG-UI supports human-in-the-loop via interrupts:

```typescript
// Agent emits interrupt
{
  type: "RUN_ERROR",
  message: "Human input required",
  code: "INTERRUPT"
}

// Client resumes with user input
{
  type: "RUN_AGENT_INPUT",
  resume: [{ interruptId: "...", value: userInput }]
}
```

## Common Issues and Solutions

### 1. SSE Connection Drops

**Problem**: SSE connection closes unexpectedly.

**Solution**:
- Implement reconnection logic
- Send heartbeat events
- Check server-side timeout settings

### 2. Event Ordering Issues

**Problem**: Events arrive out of order.

**Solution**:
- Use `timestamp` field for ordering
- Implement event buffer with reordering
- Use `sequenceId` if available

### 3. Large Payload Handling

**Problem**: Large `STATE_SNAPSHOT` events cause issues.

**Solution**:
- Use `STATE_PATCH` for incremental updates
- Compress large payloads
- Stream large data separately

## Contributing Guidelines

### Protocol Change Process

1. **Propose**: Open RFC issue with protocol change proposal
2. **Discuss**: Community review and feedback
3. **Implement**: Update core package with new event types
4. **Adopt**: Partners update their implementations
5. **Release**: Version bump and documentation

### SDK Contribution Process

1. **Fork** the repository
2. **Implement** in language-specific package
3. **Test** with Dojo app
4. **Document** with examples
5. **PR**: Submit for review

### Code Style

- TypeScript for core and JS SDKs
- Follow language-specific idioms for other SDKs
- Include JSDoc comments
- Add unit tests

## Architecture Decisions

### Why Event-Based?

- **Streaming**: Natural fit for SSE
- **Decoupling**: Agent and UI are independent
- **Debugging**: Easy to trace execution flow
- **Extensibility**: New events without breaking changes

### Why SSE Over WebSocket?

- **Simplicity**: HTTP-only, no special protocols
- **Proxy friendly**: Works with standard HTTP infrastructure
- **Backpressure**: Built-in flow control
- **Unary support**: Can also do request-response

### Why JSON Encoding?

- **Human readable**: Easy to debug
- **Universal**: Every language has JSON support
- **Extensible**: Can add fields without breaking

## Integration Examples

### LangGraph Integration

```python
from ag_ui_python import AGUIEventHandler
from langgraph.graph import StateGraph

# Agent emits AG-UI events
@app.agent
async def agent_handler(request: RunAgentInput):
    async for event in agent.stream(request):
        await handler.emit(event)
```

### Mastra Integration

```typescript
import { AGUIClient } from "@ag-ui/client";

const client = new AGUIClient({
  baseUrl: "http://localhost:4111",
});

for await (const event of client.streamEvents({ threadId })) {
  renderEvent(event);
}
```

## References

- [AG-UI Official Documentation](https://docs.ag-ui.com/)
- [AG-UI Dojo (Interactive Demo)](https://dojo.ag-ui.com/)
- [Protocol Specifications](https://github.com/ag-ui-protocol/ag-ui/blob/main/docs/concepts/)
- [Discord Community](https://discord.gg/Jd3FzfdJa8)