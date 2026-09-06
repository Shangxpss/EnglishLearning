# Frontend Repository Documentation

## Overview

The `front` repository is the **client-side layer** of the three-tier architecture. It provides a React-based chat interface with A2UI component rendering capabilities.

```
Frontend (React/Vite)  ←── HTTP/SSE ──→  Agent Runtime
```

## Repository Structure

```
front/
├── public/                      # Static assets
│   ├── favicon.svg
│   └── icons.svg
├── src/
│   ├── a2ui/                    # A2UI component catalog
│   │   ├── catalog.ts           # Catalog registration
│   │   ├── definitions.ts       # Custom component schemas
│   │   └── renderers.tsx        # React component renderers
│   ├── api/                     # API client
│   │   └── index.ts             # Axios instance with auth
│   ├── store/                   # Redux store (unused)
│   │   └── index.ts
│   ├── assets/                  # Static images
│   ├── App.tsx                  # Root component
│   ├── Chat.tsx                 # Main chat interface
│   ├── Popup.tsx                # Popup chat component
│   ├── Sidebar.tsx              # Sidebar chat component
│   ├── index.css                # Global styles
│   └── main.tsx                 # Application entry point
├── index.html                   # HTML template
├── package.json                 # Dependencies and scripts
├── vite.config.ts               # Vite configuration
├── tailwind.config.js           # Tailwind CSS config
└── tsconfig.json                # TypeScript configuration
```

## Core Components

### 1. CopilotKit Provider (`Chat.tsx:42-58`)

The `CopilotKit` component is the root provider from `@copilotkit/react-core/v2`:

```tsx
<CopilotKit
  runtimeUrl="/copilotkit" // Runtime API endpoint
  agent="sample_agent" // Agent ID (must match runtime)
  useSingleEndpoint={false} // Multi-route mode
  a2ui={{ catalog: myCatalog, includeSchema: true }} // A2UI config
  renderToolCalls={cleanToolCallRenderers} // Tool call display
>
  <CopilotChat agentId="sample_agent" />
</CopilotKit>
```

#### Key Props

| Prop                 | Type                 | Purpose                             |
| -------------------- | -------------------- | ----------------------------------- |
| `runtimeUrl`         | `string`             | URL to the agent runtime            |
| `agent`              | `string`             | Agent ID selector                   |
| `useSingleEndpoint`  | `boolean`            | `false` = multi-route mode          |
| `a2ui.catalog`       | `A2UICatalog`        | Component catalog for rendering     |
| `a2ui.includeSchema` | `boolean`            | Send full component schema to agent |
| `renderToolCalls`    | `ToolCallRenderer[]` | Custom tool call display            |

### 2. CopilotChat (`Chat.tsx:51-55`)

The chat interface component:

```tsx
<CopilotChat agentId="sample_agent" className="h-full rounded-2xl" />
```

### 3. A2UI Catalog (`src/a2ui/catalog.ts`)

Registers custom components with the A2UI renderer:

```typescript
export const myCatalog = createCatalog(
  customDefinitions, // Custom component schemas
  myRenderers, // React renderers
  {
    catalogId: "generative-agent-catalog", // Must match runtime
    includeBasicCatalog: true, // Include 18 built-in components
  },
);
```

#### Built-in Components (from Basic Catalog)

| Component       | Purpose                       |
| --------------- | ----------------------------- |
| `Text`          | Text display with variants    |
| `Row`           | Horizontal layout             |
| `Column`        | Vertical layout               |
| `Card`          | Styled card container         |
| `List`          | List of items                 |
| `Button`        | Clickable button with actions |
| `Divider`       | Visual separator              |
| `Tabs`          | Tab navigation                |
| `Modal`         | Modal dialog                  |
| `Image`         | Image display                 |
| `Icon`          | Icon display                  |
| `Video`         | Video player                  |
| `AudioPlayer`   | Audio player                  |
| `TextField`     | Text input field              |
| `CheckBox`      | Checkbox input                |
| `ChoicePicker`  | Dropdown/select               |
| `Slider`        | Range slider                  |
| `DateTimeInput` | Date/time picker              |

### 4. Custom Component Definitions (`src/a2ui/definitions.ts`)

Defines app-specific components using Zod schemas:

```typescript
export const customDefinitions = {
  Heading: {
    description: "A section heading with optional subtitle",
    props: z.object({
      title: z.string(),
      subtitle: z.string().optional(),
    }),
  },
  // ... 12 more custom components
} satisfies CatalogDefinitions;
```

#### Custom Components List

| Component      | Description             | Key Props                                      |
| -------------- | ----------------------- | ---------------------------------------------- |
| `Heading`      | Section title block     | `title`, `subtitle`                            |
| `Grid`         | Responsive grid layout  | `columns`, `gap`, `children`                   |
| `Table`        | Data table              | `headers`, `rows`, `caption`                   |
| `KeyValueList` | Label-value pairs       | `title`, `items`                               |
| `StatusBadge`  | Colored status pill     | `text`, `variant`                              |
| `Metric`       | KPI display             | `label`, `value`, `trend`                      |
| `InfoRow`      | Compact label-value row | `label`, `value`                               |
| `BarChart`     | Horizontal bar chart    | `title`, `bars`                                |
| `PieChart`     | Pie/donut chart         | `title`, `description`, `data`                 |
| `UserCard`     | User profile card       | `name`, `email`, `role`, `avatarUrl`           |
| `ResetButton`  | Form reset button       | `label`, `surfaceId`, `data`                   |
| `Spacer`       | Vertical spacing        | `height`                                       |
| `WeatherCard`  | Weather display         | `location`, `temperature`, `condition`, `icon` |

### 5. Custom Component Renderers (`src/a2ui/renderers.tsx`)

React implementations for custom components:

```typescript
export const myRenderers: CatalogRenderers<MyDefinitions> = {
  Heading: ({ props }) => (
    <div className="flex flex-col gap-1">
      <h2 className="text-xl font-bold text-gray-900">{props.title}</h2>
      {props.subtitle && <p className="text-sm text-gray-500">{props.subtitle}</p>}
    </div>
  ),
  // ... more renderers
};
```

### 6. Tool Call Renderers (`Chat.tsx:9-39`)

Controls how tool calls appear in the chat UI:

```typescript
const cleanToolCallRenderers = [
  // Hide A2UI tool calls (render_a2ui, log_a2ui_event)
  defineToolCallRenderer({
    name: "log_a2ui_event",
    args: z.any(),
    render: () => null, // Render nothing in chat
  }),
  defineToolCallRenderer({
    name: "render_a2ui",
    args: z.any(),
    render: () => null, // Render nothing in chat
  }),
  // Show status indicator for other tools
  defineToolCallRenderer({
    name: "*",
    render: ({ name, status }) => {
      /* ... */
    },
  }),
];
```

**Why `render: () => null` for A2UI tools**: The A2UI surface is rendered as a rich React component in a dedicated area. Showing raw JSON would be confusing.

## A2UI Rendering Flow

1. Agent emits `ACTIVITY_SNAPSHOT` event via SSE
2. CopilotKitCore receives and parses the event
3. A2UI renderer looks up components in the catalog
4. React components are rendered in the UI surface
5. User interactions dispatch actions back to the agent

## Development Workflow

### Installation

```bash
pnpm install
```

### Environment Variables

| Variable            | Default                 | Purpose      |
| ------------------- | ----------------------- | ------------ |
| `VITE_API_BASE_URL` | `http://localhost:3000` | API base URL |

### Running

```bash
pnpm run dev
```

### Building

```bash
pnpm run build
```

### Linting

```bash
pnpm run lint
```

## Key Dependencies

| Package                     | Version    | Purpose                     |
| --------------------------- | ---------- | --------------------------- |
| `@copilotkit/react-core`    | `^1.59.5`  | CopilotKit React components |
| `@copilotkit/a2ui-renderer` | `^1.59.5`  | A2UI component rendering    |
| `@ag-ui/client`             | `^0.0.55`  | AG-UI client utilities      |
| `react`                     | `^19.2.6`  | React framework             |
| `tailwindcss`               | `^4.3.0`   | CSS framework               |
| `zod`                       | `^3.25.76` | Schema validation           |

## Adding a New Custom Component

1. **Define schema** in `definitions.ts`:

   ```typescript
   MyComponent: {
     description: "Component description for LLM",
     props: z.object({ /* prop schema */ }),
   },
   ```

2. **Implement renderer** in `renderers.tsx`:

   ```typescript
   MyComponent: ({ props }) => <div>{/* React JSX */}</div>,
   ```

3. **Verify catalog registration** in `catalog.ts` — it automatically picks up new definitions.

4. **Test** by asking the agent to use the new component.

## Catalog ID Synchronization

The catalog ID must match across:

- Frontend: `createCatalog({ catalogId: "generative-agent-catalog" })`
- Runtime: `CopilotRuntime({ a2ui: { defaultCatalogId: "generative-agent-catalog" } })`
- Backend: `a2ui.create_surface(SURFACE_ID, catalog_id=CATALOG_ID)`

## Debugging

- Check browser console for A2UI rendering errors
- Use React DevTools to inspect component tree
- Monitor SSE stream in Network tab for `ACTIVITY_SNAPSHOT` events
- Enable `debug: true` in CopilotKit options for verbose logging

## Maintenance Notes

### Updating Component Schema

When modifying component props:

1. Update the Zod schema in `definitions.ts`
2. Update the renderer in `renderers.tsx`
3. The catalog automatically regenerates on next build

### Adding Frontend Tools

Register tools using `useCopilotAction()` or pass to `CopilotKit`'s `actions` prop.

### Styling

- Uses Tailwind CSS 4.x (CSS-first approach)
- Components are styled inline with Tailwind classes
- Global styles in `index.css`

## Known Issues

1. **Redux store**: The `store/` directory exists but is not actively used.
2. **Popup/Sidebar**: These components exist but are not used in the main app.
3. **API client**: The `api/index.ts` is a generic axios client but not used for CopilotKit communication.
