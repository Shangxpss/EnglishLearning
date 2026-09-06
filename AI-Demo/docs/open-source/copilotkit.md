# CopilotKit Repository Maintainer Guide

## Overview

**CopilotKit** is the main umbrella repository containing multiple packages for building agentic applications. It includes frontend frameworks (React, Vue, Angular), backend integrations (LangGraph, FastAPI), and the A2UI renderer.

- **Repository**: [CopilotKit/CopilotKit](https://github.com/CopilotKit/CopilotKit)
- **License**: MIT
- **Monorepo Structure**: Uses Nx for managing multiple packages

## Repository Structure

```
CopilotKit/
├── packages/                      # Core npm packages
│   ├── react-core/                 # React components (v1 & v2)
│   │   ├── src/
│   │   │   ├── v2/                # v2 API (recommended)
│   │   │   │   ├── index.ts       # Main exports
│   │   │   │   ├── providers/     # CopilotKit provider
│   │   │   │   ├── components/    # Chat, Sidebar, Popup
│   │   │   │   ├── hooks/        # useCopilotChat, useCoagent
│   │   │   │   └── types/        # TypeScript types
│   │   │   └── v1/               # Legacy v1 API
│   │   └── package.json
│   ├── react-ui/                  # UI components (MessageInput, etc.)
│   ├── react-textarea/            # Auto-suggestions textarea
│   ├── react-native/             # React Native support
│   ├── runtime/                   # Backend runtime (v1 & v2)
│   │   ├── src/
│   │   │   ├── v2/               # v2 API
│   │   │   │   ├── index.ts      # CopilotRuntime, createCopilotRuntimeHandler
│   │   │   │   ├── runtime/      # CopilotRuntime core
│   │   │   │   │   ├── index.ts  # Main runtime class
│   │   │   │   │   ├── runner/   # AgentRunner implementations
│   │   │   │   │   └── endpoints/ # HTTP endpoints
│   │   │   │   ├── lib/
│   │   │   │   │   ├── integrations/ # Node, Express, Nest, Hono
│   │   │   │   │   └── cloud/    # CopilotKit Intelligence
│   │   │   │   └── service-adapters/ # Service adapters
│   │   │   ├── agent/            # v1 agent abstractions
│   │   │   └── graphql/          # GraphQL message conversion
│   │   └── package.json
│   ├── a2ui-renderer/            # A2UI React renderer
│   │   └── src/
│   │       └── react-renderer/
│   │           ├── index.ts      # createCatalog, main exports
│   │           ├── a2ui-react/   # A2UI rendering logic
│   │           │   ├── catalog/  # Catalog implementations
│   │           │   │   ├── basic/ # Basic catalog (18 components)
│   │           │   │   └── minimal/ # Minimal catalog
│   │           │   └── styles/   # CSS styles
│   │           └── styles/       # Component styles
│   ├── sqlite-runner/            # SQLite state persistence
│   ├── shared/                   # Shared utilities
│   ├── sdk-js/                  # JavaScript SDK
│   ├── vue/                      # Vue 3 support (v2)
│   ├── angular/                  # Angular support
│   ├── voice/                    # Voice capabilities
│   └── bot/                      # Bot integrations
├── sdk-python/                   # Python SDK
│   ├── copilotkit/
│   │   ├── __init__.py          # Main exports
│   │   ├── copilotkit_lg_middleware.py # LangGraph middleware
│   │   ├── langgraph.py         # LangGraph integration
│   │   ├── header_propagation.py # Header forwarding
│   │   └── crewai/              # CrewAI integration
│   └── pyproject.toml
├── examples/                     # Example applications
│   ├── v2/                      # v2 examples
│   │   ├── runtime/             # Runtime examples (Node, Express, Hono, etc.)
│   │   └── node/               # Node.js example
│   └── integrations/            # Framework integrations
├── showcase/                    # Demo applications
│   ├── integrations/            # Third-party integrations
│   └── shell/                  # Demo shell
└── docs/                        # Documentation site
```

## Key Packages

### @copilotkit/react-core

**Purpose**: React components and hooks for building chat interfaces.

**Entry Point**: `packages/react-core/src/v2/index.ts`

**Main Exports**:
```typescript
// Provider
export { CopilotKit } from "./providers/CopilotKit";

// Components
export { CopilotChat } from "./components/chat/CopilotChat";
export { CopilotSidebar } from "./components/chat/CopilotSidebar";
export { CopilotPopup } from "./components/chat/CopilotPopup";

// Hooks
export { useCopilotChat } from "./hooks/useCopilotChat";
export { useCoagent } from "./hooks/useCoagent";
export { useCopilotAction } from "./hooks/useCopilotAction";

// Utilities
export { defineToolCallRenderer } from "./hooks/useMakeChatMessages";
export type { ToolCallRenderer } from "./hooks/useMakeChatMessages";

// Types
export type { CopilotKitProps } from "./types";
export type { ChatComponentProps } from "./components/chat/types";
```

**Key Source Files**:
| File | Purpose |
|------|---------|
| `providers/CopilotKit.tsx` | Root provider component |
| `components/chat/CopilotChat.tsx` | Main chat interface |
| `hooks/useCopilotChat.ts` | Chat state management |
| `hooks/useCopilotAction.ts` | Action registration |

### @copilotkit/runtime

**Purpose**: Server-side runtime for request handling and agent management.

**Entry Point**: `packages/runtime/src/v2/index.ts`

**Main Exports**:
```typescript
// Core
export { CopilotRuntime } from "./runtime";
export { createCopilotRuntimeHandler } from "./runtime/endpoints";

// Agents
export { HttpAgent } from "@ag-ui/client";
export type { AgentConfig } from "./runtime";

// Runners
export { InMemoryAgentRunner } from "./runtime/runner";
export { SqliteAgentRunner } from "@copilotkit/sqlite-runner";

// Types
export type { CopilotRuntimeConfig } from "./runtime/types";
```

**Key Source Files**:
| File | Purpose |
|------|---------|
| `runtime/index.ts` | CopilotRuntime class |
| `runtime/endpoints/index.ts` | HTTP handler factory |
| `runtime/runner/index.ts` | AgentRunner base class |

### @copilotkit/a2ui-renderer

**Purpose**: A2UI component catalog and rendering for React.

**Entry Point**: `packages/a2ui-renderer/src/index.ts`

**Main Exports**:
```typescript
export { createCatalog } from "./react-renderer";
export type { Catalog, CatalogDefinitions, CatalogRenderers } from "./react-renderer/types";
```

**Key Source Files**:
| File | Purpose |
|------|---------|
| `react-renderer/index.ts` | createCatalog implementation |
| `react-renderer/a2ui-react/catalog/basic/index.ts` | Basic catalog (18 components) |

## copilotkit Python SDK

**Purpose**: Python middleware and tools for LangGraph integration.

**Entry Point**: `sdk-python/copilotkit/__init__.py`

**Main Components**:

### copilotkit_lg_middleware.py

**Purpose**: LangGraph middleware for CopilotKit integration.

**Key Classes**:
| Class | Purpose |
|-------|---------|
| `StateSchema` | Base state schema with CopilotKit properties |
| `CopilotKitMiddleware` | Main middleware for agent integration |

**Key Methods**:
| Method | Purpose |
|--------|---------|
| `wrap_model_call()` | Intercept LLM calls for header propagation and A2UI injection |
| `wrap_tool_call()` | Handle tool execution (including generate_a2ui) |

**Source Location**: `sdk-python/copilotkit/copilotkit_lg_middleware.py`

### Key Functions

```python
# Main exports from __init__.py
from copilotkit import (
    CopilotKitMiddleware,  # LangGraph middleware
    a2ui,                 # A2UI operations
    CopilotKit,           # LangGraphAGUIAgent (FastAPI)
    LangGraphAGUIAgent,   # FastAPI endpoint wrapper
)
```

## Development Workflow

### Setting Up

```bash
# Clone the repository
git clone https://github.com/CopilotKit/CopilotKit.git
cd CopilotKit

# Install dependencies (requires pnpm)
pnpm install

# Build all packages
pnpm build

# Run tests
pnpm test
```

### Running Examples

```bash
# Run a specific example
cd examples/v2/runtime/node
pnpm install
pnpm dev
```

### Making Changes

1. **Code Changes**: Edit files in `packages/*/src/`
2. **Rebuild**: `pnpm build --filter=@copilotkit/<package-name>`
3. **Test**: `pnpm test --filter=@copilotkit/<package-name>`
4. **Linting**: `pnpm lint`

## Testing

### Unit Tests

```bash
pnpm test --filter=@copilotkit/react-core
```

### E2E Tests

```bash
cd showcase/dojo
pnpm install
pnpm test
```

## Release Process

1. **Version Bump**: Update version in `package.json`
2. **Build**: `pnpm build`
3. **Publish**: `pnpm publish`

## Common Issues and Solutions

### 1. TypeScript Compilation Errors

**Problem**: Missing type definitions or import errors.

**Solution**: Ensure all packages are built in dependency order:
```bash
pnpm build --filter=@copilotkit/shared
pnpm build --filter=@copilotkit/runtime
pnpm build --filter=@copilotkit/react-core
```

### 2. Runtime Request Handling Issues

**Problem**: Requests not reaching the agent.

**Debugging**:
- Check `CopilotRuntime` agent configuration matches frontend `agent` prop
- Verify `runtimeUrl` is correct
- Check CORS configuration

### 3. Python SDK Import Errors

**Problem**: `ag_ui_langgraph` module not found.

**Solution**: Install the full SDK with all dependencies:
```bash
pip install copilotkit[langgraph]
```

## Contributing Guidelines

### Code Style

- Use TypeScript for new code
- Follow existing patterns in the codebase
- Add JSDoc comments for public APIs
- Include type definitions

### Pull Request Process

1. Fork the repository
2. Create a feature branch
3. Make changes with tests
4. Submit PR with description
5. Address review feedback

### Testing Requirements

- Unit tests for new functionality
- E2E tests for UI changes
- Type checking passes (`pnpm typecheck`)

## Architecture Decisions

### Why Nx Monorepo?

- Shared tooling across packages
- Incremental builds
- Dependency graph management

### Why v1/v2 APIs?

- v1: Original API, maintained for backwards compatibility
- v2: Improved API with better patterns, recommended for new projects

### A2UI Integration

- CopilotKit uses A2UI for component rendering
- Frontend registers component catalog
- Agent generates component definitions
- Runtime streams events to frontend

## References

- [CopilotKit Documentation](https://docs.copilotkit.ai/)
- [GitHub Repository](https://github.com/CopilotKit/CopilotKit)
- [Discord Community](https://discord.gg/6dffbvGU3D)