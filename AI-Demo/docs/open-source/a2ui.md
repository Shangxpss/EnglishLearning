# A2UI Repository Maintainer Guide

## Overview

**A2UI** (Agent-to-User Interface) is an open-source project consisting of a declarative JSON format for representing updatable agent-generated UIs and an initial set of renderers. It allows agents to generate or populate rich user interfaces safely and framework-agnostically.

- **Repository**: [CopilotKit/A2UI](https://github.com/CopilotKit/A2UI)
- **License**: MIT
- **Status**: v0.8 (Public Preview) - Currently evolving

## Core Philosophy

A2UI was designed with these core principles:

1. **Security First**: Agents send declarative JSON, not executable code. The client maintains a catalog of pre-approved UI components.

2. **LLM-Friendly**: UI is a flat list of components with IDs, easy for LLMs to generate incrementally.

3. **Framework-Agnostic**: Same JSON payload can render on React, Flutter, Angular, Lit, SwiftUI, etc.

4. **Incremental Updates**: Agents can efficiently make partial changes to the UI.

## Repository Structure

```
a2ui/
├── docs/                       # Documentation
│   ├── concepts/              # Core concepts
│   │   ├── overview.md        # A2UI overview
│   │   ├── components.md      # Component system
│   │   ├── catalogs.md       # Component catalogs
│   │   ├── data-binding.md   # Data binding system
│   │   ├── actions.md        # User actions
│   │   └── data-flow.md       # Data flow patterns
│   ├── guides/                # How-to guides
│   ├── reference/             # API reference
│   └── ecosystem/             # Community renderers
├── agent_sdks/                # Agent-side SDKs
│   ├── python/               # Python SDK
│   ├── kotlin/               # Kotlin SDK
│   └── conformance/          # Conformance tests
├── renderers/                # Client-side renderers
│   ├── react/                # React renderer
│   │   ├── src/
│   │   │   ├── index.ts     # Main exports
│   │   │   ├── core/        # Core rendering logic
│   │   │   └── components/   # Built-in components
│   │   └── package.json
│   ├── lit/                  # Lit (Web Components) renderer
│   │   ├── src/
│   │   │   ├── v0_8/        # v0.8 implementation
│   │   │   └── v0_9/        # v0.9 implementation
│   │   └── package.json
│   ├── flutter/              # Flutter renderer
│   └── angular/              # Angular renderer
│       ├── src/
│       │   ├── v0_8/        # v0.8 implementation
│       │   └── v0_9/        # v0.9 implementation
│       └── package.json
├── eval/                      # Evaluation framework
│   ├── a2ui_eval/            # Evaluation code
│   ├── datasets/              # Test datasets
│   └── tests/                # Evaluation tests
└── pyproject.toml
```

## Core Concepts

### The A2UI Format

Agents generate a declarative JSON payload:

```json
{
  "surfaceId": "chat-1",
  "components": [
    {
      "id": "weather-card-1",
      "component": "Card",
      "children": [
        {
          "id": "title-1",
          "component": "Heading",
          "level": 2,
          "text": "Weather in Beijing"
        },
        {
          "id": "temp-1",
          "component": "Text",
          "text": "22°C - Sunny"
        }
      ]
    }
  ],
  "dataModel": {
    "/weather/temperature": 22,
    "/weather/condition": "sunny"
  }
}
```

### Component Catalog

A **catalog** maps abstract component names to actual implementations:

```typescript
// Example catalog definition
const catalog = {
  Card: {
    description: "Container for grouped content",
    properties: {
      elevated: { type: "boolean", default: false },
    },
  },
  Heading: {
    description: "Text heading",
    properties: {
      level: { type: "number", enum: [1, 2, 3, 4, 5, 6] },
      text: { type: "string" },
    },
  },
};
```

### Data Binding

Components can reference data from a data model:

```json
{
  "id": "temp-display",
  "component": "Text",
  "value": { "path": "/weather/temperature" }
}
```

### User Actions

Users can interact with components:

```json
{
  "id": "submit-btn",
  "component": "Button",
  "label": "Submit",
  "onClick": {
    "action": "submit_form",
    "params": { "formId": "reg-form" }
  }
}
```

## Key Packages

### React Renderer

**Purpose**: Render A2UI components in React applications.

**Entry Point**: `renderers/react/src/index.ts`

**Main Exports**:
```typescript
import {
  A2UIProvider,           // Context provider
  useA2UI,               // Hook for A2UI operations
  useCatalog,            // Hook to access catalog
  renderComponent,       // Render a single component
  createRenderer,        // Create custom renderer
} from "@a2ui/react";

import {
  createCatalog,         // Create component catalog
  basicCatalog,          // Built-in catalog (18 components)
} from "@a2ui/react";
```

**Built-in Components** (from `basicCatalog`):
| Component | Description |
|-----------|-------------|
| `Text` | Plain text content |
| `Heading` | Heading text (h1-h6) |
| `Row` | Horizontal layout |
| `Column` | Vertical layout |
| `Card` | Elevated container |
| `List` | List of items |
| `Button` | Clickable button |
| `Divider` | Horizontal separator |
| `Tabs` | Tabbed interface |
| `Modal` | Dialog overlay |
| `Image` | Image display |
| `Icon` | Icon display |
| `Video` | Video player |
| `AudioPlayer` | Audio player |
| `TextField` | Text input |
| `CheckBox` | Checkbox input |
| `ChoicePicker` | Radio/select input |
| `Slider` | Range slider |
| `DateTimeInput` | Date/time picker |

**Key Source Files**:
| File | Purpose |
|------|---------|
| `src/index.ts` | Main exports |
| `src/core/renderer.tsx` | Core rendering logic |
| `src/core/catalog.ts` | Catalog implementation |
| `src/core/data-binding.ts` | Data binding logic |
| `src/core/actions.ts` | Action handling |

### Lit Renderer

**Purpose**: Render A2UI components as Web Components.

**Versions**:
- `v0_8/`: Original v0.8 implementation
- `v0_9/`: Latest v0.9 implementation

**Main Exports**:
```typescript
import {
  defineComponents,      // Define all components
  createSurface,         // Create UI surface
  updateComponents,      // Update components
  deleteSurface,        // Delete surface
} from "@a2ui/lit";
```

### Flutter Renderer

**Purpose**: Render A2UI components in Flutter apps.

**Entry Point**: `renderers/flutter/lib/src/`

**Main Components**:
```dart
import 'package:a2ui_flutter';

// A2UI Widget
A2UIWidget(
  catalog: myCatalog,
  onAction: (action) => handleAction(action),
)

// Catalog builder
final catalog = A2UICatalogBuilder()
  .register('Card', myCardBuilder)
  .register('Text', myTextBuilder)
  .build();
```

## Message Types

### Operations

| Operation | Purpose | Payload |
|-----------|---------|---------|
| `createSurface` | Create new UI surface | `{ surfaceId, catalogId, initialComponents? }` |
| `updateComponents` | Add/update components | `{ surfaceId, components }` |
| `updateDataModel` | Update data model | `{ surfaceId, dataModel }` |
| `deleteSurface` | Remove surface | `{ surfaceId }` |

### Component Structure

```typescript
interface A2UIComponent {
  id: string;                    // Unique identifier
  component: string;             // Component type name
  children?: A2UIComponent[];   // Child components
  [key: string]: any;           // Component-specific properties
}
```

### Data Binding

```typescript
interface DataBinding {
  path: string;                  // JSON path in data model
  transform?: string;           // Optional transformation
}

interface ValueBinding {
  value: any;                   // Static value
}
```

### Actions

```typescript
interface ComponentAction {
  action: string;               // Action identifier
  params?: Record<string, any>; // Action parameters
}
```

## Catalog System

### Creating a Catalog

```typescript
import { createCatalog, z } from "@a2ui/react";

// Define component schemas with Zod
const customComponents = {
  WeatherCard: {
    description: "Weather information card",
    properties: {
      location: z.string(),
      temperature: z.number(),
      condition: z.enum(["sunny", "cloudy", "rainy", "snowy"]),
    },
  },
};

// Define renderers
const customRenderers = {
  WeatherCard: ({ location, temperature, condition }) => (
    <div className="weather-card">
      <h3>{location}</h3>
      <p>{temperature}°C - {condition}</p>
    </div>
  ),
};

// Create catalog
const myCatalog = createCatalog(customComponents, customRenderers, {
  catalogId: "my-weather-catalog",
  includeBasicCatalog: true,  // Include 18 built-in components
});
```

### Smart Wrappers

Smart wrappers allow mapping server-side types to custom implementations:

```typescript
const smartWrappers = {
  // Map server type to client component
  "UserProfile": {
    component: "div",
    mapData: (data) => ({
      children: [
        { id: "name", component: "Text", text: data.name },
        { id: "avatar", component: "Image", src: data.avatarUrl },
      ],
    }),
  },
};
```

## Development Workflow

### Setting Up

```bash
# Clone the repository
git clone https://github.com/CopilotKit/A2UI.git
cd A2UI

# Install dependencies (requires uv for Python, pnpm for JS)
uv sync          # Python dependencies
pnpm install     # JS dependencies

# Build all packages
pnpm build

# Run tests
pnpm test
```

### Creating a New Renderer

1. **Create package directory**:
```bash
mkdir renderers/my-framework
cd renderers/my-framework
```

2. **Define component interface**:
```typescript
// src/component.ts
export interface A2UIComponent {
  id: string;
  component: string;
  children?: A2UIComponent[];
  [key: string]: any;
}
```

3. **Implement catalog interface**:
```typescript
// src/catalog.ts
export interface CatalogRenderer {
  canRender(componentName: string): boolean;
  render(component: A2UIComponent, context: RenderContext): any;
}
```

4. **Implement surface management**:
```typescript
// src/surface.ts
export interface SurfaceManager {
  createSurface(id: string, catalogId: string): void;
  updateComponents(surfaceId: string, components: A2UIComponent[]): void;
  deleteSurface(surfaceId: string): void;
}
```

5. **Add tests and documentation**

### Testing with Conformance Suite

```bash
cd agent_sdks/conformance
uv run pytest
```

## Evaluation Framework

The `eval/` directory contains tools for evaluating A2UI implementations:

### Running Evaluations

```bash
cd eval
uv run python -m a2ui_eval.run --dataset=recipes
```

### Datasets

| Dataset | Purpose |
|---------|---------|
| `recipes` | Recipe generation and modification |
| `forms` | Dynamic form generation |
| `dashboards` | Dashboard composition |

### Scorers

```python
from a2ui_eval.scorers import ComponentAccuracy, LayoutScore, DataBindingScore

scorer = ComponentAccuracy(catalog=my_catalog)
score = scorer.score(expected=expected, actual=actual)
```

## Common Issues and Solutions

### 1. Unknown Component Error

**Problem**: Renderer doesn't recognize a component.

**Solution**:
- Ensure component is registered in catalog
- Check catalog ID matches
- Verify component name spelling

### 2. Data Binding Not Updating

**Problem**: Component doesn't reflect data model changes.

**Solution**:
- Check path format is correct
- Ensure `updateDataModel` was called
- Verify data model structure matches bindings

### 3. Action Not Triggering

**Problem**: Click/interaction doesn't call agent.

**Solution**:
- Check action is properly defined
- Verify surface is connected to agent
- Ensure event handler is registered

### 4. Renderer Performance Issues

**Problem**: Large component trees render slowly.

**Solution**:
- Use incremental updates (`updateComponents`)
- Implement virtual scrolling for lists
- Memoize component renderers

## Security Considerations

### Why Declarative?

A2UI uses declarative JSON instead of executable code:

```json
// Safe: Just data
{ "component": "Button", "label": "Submit" }

// NOT allowed: No executable code
{ "component": "eval", "code": "deleteUserData()" }
```

### Catalog Enforcement

Only components in the catalog can be rendered:

```typescript
const catalog = createCatalog(allowedComponents, renderers);
// Agent cannot render components outside `allowedComponents`
```

### Sandboxing

Smart wrappers can implement sandboxing:

```typescript
const smartWrappers = {
  "Iframe": {
    component: "iframe",
    sandbox: true,  // Enable sandbox mode
    mapData: (data) => ({
      src: data.url,
      sandbox: "allow-scripts",
    }),
  },
};
```

## Contributing Guidelines

### Protocol Change Process

1. **Propose**: Open RFC issue with format change proposal
2. **Discuss**: Community review and feedback
3. **Implement**: Update core format and all renderers
4. **Test**: Run conformance tests
5. **Release**: Version bump and changelog

### Renderer Contribution

1. **Follow interface**: Implement required interfaces
2. **Test thoroughly**: Include unit and integration tests
3. **Document**: API docs and usage examples
4. **Performance**: Optimize for large component trees

### Code Style

- TypeScript for JS/TS code
- Dart for Flutter
- Swift for iOS (if adding)
- Include JSDoc comments
- Add unit tests

## Version History

### v0.9 (Latest)

- Improved data binding syntax
- Enhanced smart wrappers
- Better TypeScript types

### v0.8

- Initial public preview
- Basic component catalog
- React and Flutter renderers

## Integration with CopilotKit

A2UI is the UI rendering layer for CopilotKit:

```
Agent (LangGraph) → AG-UI events → CopilotKit Runtime → A2UI Components → React/Vue/etc.
```

```typescript
// CopilotKit integrates A2UI
<CopilotKit a2ui={{ catalog: myCatalog, includeSchema: true }}>
  <CopilotChat />
</CopilotKit>
```

## References

- [A2UI Official Documentation](https://docs.a2ui.ai/)
- [GitHub Repository](https://github.com/CopilotKit/A2UI)
- [Component Reference](https://docs.a2ui.ai/reference/components/)
- [API Reference](https://docs.a2ui.ai/reference/)