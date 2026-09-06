# Agent Runtime Repository Documentation

## Overview

The `agent-runtime` repository is the **middle layer** of the three-tier architecture. It acts as a proxy between the frontend and the Python LangGraph agent, handling request routing, SSE streaming, and A2UI protocol translation.

```
Frontend (React)  →  Agent Runtime (Bun/TypeScript)  →  Backend (Python)
```

## Repository Structure

```
agent-runtime/
├── index.ts          # Main entry point — CopilotRuntime + HTTP server
├── package.json      # Dependencies and scripts
├── tsconfig.json     # TypeScript configuration
├── build.ts          # Build script
├── bun.lock          # Bun package lockfile
└── README.md         # Project documentation
```

## Core Components

### 1. CopilotRuntime (`index.ts:12-19`)

The `CopilotRuntime` is the central orchestrator from `@copilotkit/runtime/v2`. It manages:

- **Agent registration**: Maps agent IDs to agent instances
- **A2UI configuration**: Enables dynamic UI generation
- **Thread management**: Tracks conversation state via `AgentRunner`

```typescript
const runtime = new CopilotRuntime({
  agents: { sample_agent: agent }, // Agent registry
  a2ui: {
    injectA2UITool: true, // Enable A2UI tool injection
    defaultCatalogId: "generative-agent-catalog", // Must match frontend catalog
  },
  runner: new InMemoryAgentRunner(), // State storage (dev only)
});
```

#### Key Configuration Options

| Option                  | Type                    | Purpose                                        |
| ----------------------- | ----------------------- | ---------------------------------------------- |
| `agents`                | `Record<string, Agent>` | Maps agent IDs to agent instances              |
| `a2ui.injectA2UITool`   | `boolean`               | If true, injects `render_a2ui` tool into agent |
| `a2ui.defaultCatalogId` | `string`                | Catalog ID for A2UI surfaces                   |
| `runner`                | `AgentRunner`           | State persistence strategy                     |

### 2. HttpAgent (`index.ts:10`)

Connects the runtime to the Python backend via HTTP:

```typescript
const agent = new HttpAgent({ url: `${AGENT_URL}/` });
```

The `HttpAgent` is from `@ag-ui/client` and handles:

- HTTP request forwarding to the Python FastAPI endpoint
- SSE stream proxying
- AG-UI protocol translation

### 3. CopilotRuntimeHandler (`index.ts:21-24`)

Creates the HTTP handler for the runtime:

```typescript
const handler = createCopilotRuntimeHandler({
  runtime,
  basePath: "/copilotkit",
});
```

#### Endpoints Exposed

| Method     | Path                                        | Purpose           |
| ---------- | ------------------------------------------- | ----------------- |
| `GET`      | `/copilotkit/info`                          | Agent information |
| `POST`     | `/copilotkit/agent/:agentId/run`            | Execute agent     |
| `GET`      | `/copilotkit/agent/:agentId/connect`        | SSE connection    |
| `POST`     | `/copilotkit/agent/:agentId/stop/:threadId` | Stop execution    |
| `GET/POST` | `/copilotkit/threads`                       | Thread management |

### 4. Bun HTTP Server (`index.ts:28-43`)

The runtime uses Bun as its HTTP server:

```typescript
Bun.serve({
  port: PORT,
  async fetch(req) {
    const res = await handler(req);
    // CORS headers added here
    return res;
  },
});
```

#### CORS Configuration

The server adds permissive CORS headers for development:

```typescript
res.headers.set("Access-Control-Allow-Origin", "*");
res.headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
res.headers.set("Access-Control-Allow-Headers", "Content-Type, Authorization");
```

**⚠️ Production Note**: Replace `*` with specific origins in production.

## A2UI Middleware Integration

The `A2UIMiddleware` (from `@ag-ui/a2ui-middleware`) is automatically applied when `a2ui.injectA2UITool: true`. Its pipeline:

1. **processUserAction**: Handles button clicks and user interactions
2. **injectSchemaContext**: Adds component schema to request context
3. **injectToolAndFlag**: Adds `render_a2ui` tool + `injectA2UITool` flag
4. **injectToolGuidelines**: Adds usage instructions to LLM context
5. **processStream**: Intercepts tool calls, emits `ACTIVITY_SNAPSHOT` events

## Agent Runner Options

| Runner                    | Use Case    | Persistence                            |
| ------------------------- | ----------- | -------------------------------------- |
| `InMemoryAgentRunner`     | Development | Ephemeral (lost on restart)            |
| `SqliteAgentRunner`       | Production  | File-backed (persists across restarts) |
| `IntelligenceAgentRunner` | Enterprise  | Cloud-managed                          |

## Development Workflow

### Installation

```bash
bun install
```

### Environment Variables

| Variable    | Default                 | Purpose             |
| ----------- | ----------------------- | ------------------- |
| `AGENT_URL` | `http://localhost:8000` | Python backend URL  |
| `PORT`      | `4000`                  | Runtime server port |

### Running

```bash
bun run index.ts
```

### Building

```bash
bun run build.ts
```

## Key Dependencies

| Package               | Version   | Purpose              |
| --------------------- | --------- | -------------------- |
| `@copilotkit/runtime` | `^1.59.5` | CopilotRuntime core  |
| `@ag-ui/client`       | (peer)    | HTTP agent for AG-UI |

## Debugging

- Enable `debug: true` in `CopilotRuntime` options for verbose logging
- Check console output for request/response details
- Monitor SSE stream via browser DevTools (Network tab)

## Maintenance Notes

### Adding a New Agent

1. Create a new `HttpAgent` instance
2. Add it to the `agents` config object
3. Ensure the agent ID matches the frontend's `agent` prop

### Changing Catalog ID

Update `defaultCatalogId` in `a2ui` config and ensure it matches the frontend's `createCatalog` call.

### Switching to Production Runner

```typescript
import { SqliteAgentRunner } from "@copilotkit/sqlite-runner";

const runtime = new CopilotRuntime({
  agents: { sample_agent: agent },
  runner: new SqliteAgentRunner({ dbPath: "./data/threads.db" }),
});
```

## Known Issues

1. **CORS security**: Current configuration allows all origins. Restrict in production.
2. **State persistence**: `InMemoryAgentRunner` loses state on restart. Use `SqliteAgentRunner` for production.
3. **Error handling**: Missing custom error handlers for HTTP errors.
