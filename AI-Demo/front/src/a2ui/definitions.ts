import { z } from "zod";
import type { CatalogDefinitions } from "@copilotkit/a2ui-renderer";

// ─── Custom Domain-Specific Components ─────────────────────────────────
// Only app-specific components that go beyond the basic catalog.
// The 18 built-in components (Text, Row, Column, Card, List, Button,
// Divider, Tabs, Modal, Image, Icon, Video, AudioPlayer, TextField,
// CheckBox, ChoicePicker, Slider, DateTimeInput) are handled by the
// basic catalog with correct CommonSchemas (DynamicString, ChildList,
// Action, etc.) — we only override their renderers, not their schemas.

export const customDefinitions = {
  Heading: {
    description:
      "A section heading with an optional subtitle. Renders as a styled title block. Use as the first child inside a Card or at the top of a Column to label a section.",
    props: z.object({
      title: z.string(),
      subtitle: z.string().optional(),
    }),
  },

  Grid: {
    description:
      "A responsive grid layout. Renders children in N equal-width columns with a configurable gap. Use for dashboards, card grids, or any multi-column layout.",
    props: z.object({
      columns: z.number().optional(),
      gap: z.number().optional(),
      children: z.array(z.string()),
    }),
  },

  Table: {
    description:
      "A data table with headers and rows. Provide `headers` (array of column labels) and `rows` (array of row objects, each key matching a header). Renders a clean, styled HTML table. Ideal for attribute lists, comparisons, schedules, etc.",
    props: z.object({
      headers: z.array(z.string()),
      rows: z.array(z.record(z.string())),
      caption: z.string().optional(),
    }),
  },

  KeyValueList: {
    description:
      "A vertical list of label-value pairs, rendered as a clean definition list. Each item has a `label` (bold, left) and `value` (right). Perfect for attribute tables, specs, settings, metadata — anything that is a flat list of key-value pairs. Much simpler than building a Table for two-column data.",
    props: z.object({
      title: z.string().optional(),
      items: z.array(
        z.object({
          label: z.string(),
          value: z.string(),
        }),
      ),
    }),
  },

  StatusBadge: {
    description:
      "A small coloured pill communicating the state of something (healthy/degraded/down, online/offline). Choose `variant` to match the intent.",
    props: z.object({
      text: z.string(),
      variant: z.enum(["success", "warning", "error", "info"]).optional(),
    }),
  },

  Metric: {
    description:
      "A key/value KPI display with an optional trend indicator. Ideal for dashboards (e.g. 'Revenue - $12.4k - up').",
    props: z.object({
      label: z.string(),
      value: z.string(),
      trend: z.enum(["up", "down", "neutral"]).optional(),
    }),
  },

  InfoRow: {
    description:
      "A compact two-column 'label: value' row. Good for stacks of facts inside a Card (owner, region, last updated, etc.).",
    props: z.object({
      label: z.string(),
      value: z.string(),
    }),
  },

  BarChart: {
    description:
      "A horizontal bar chart for comparing values across categories. Each bar has a label, value, and optional color. Use for stack metrics, skill levels, resource usage, etc.",
    props: z.object({
      title: z.string().optional(),
      bars: z.array(
        z.object({
          label: z.string(),
          value: z.number(),
          maxValue: z.number().optional(),
          color: z
            .enum(["blue", "emerald", "amber", "red", "purple", "gray"])
            .optional(),
        }),
      ),
    }),
  },

  PieChart: {
    description:
      "A pie/donut chart with a coloured legend. Provide `title`, `description`, and `data` as an array of `{ label, value }` objects. Great for part-of-whole breakdowns (sales by region, traffic sources, portfolio allocation).",
    props: z.object({
      title: z.string(),
      description: z.string().optional(),
      data: z.array(
        z.object({
          label: z.string(),
          value: z.number(),
        }),
      ),
    }),
  },

  UserCard: {
    description:
      "A user profile card showing avatar, name, email, and role. Use for displaying user information in a visually appealing way.",
    props: z.object({
      name: z.string(),
      email: z.string(),
      role: z.string(),
      avatarUrl: z.string().optional(),
    }),
  },

  // PrimaryButton: {
  //   description:
  //     "A styled primary call-to-action button. Attach an optional `action` that will be dispatched back to the agent when the user clicks it.",
  //   props: z.object({
  //     label: z.string(),
  //     action: z.any().optional(),
  //   }),
  // },

  ResetButton: {
    description:
      "A button that resets form fields to their default values locally, without sending an event to the agent. When clicked, it updates the data model of the specified surface to the values in the 'data' prop. Use this instead of a regular Button for reset/clear form actions.",
    props: z.object({
      label: z.string(),
      surfaceId: z.string(),
      data: z.record(z.any()),
    }),
  },

  Spacer: {
    description:
      "A vertical spacer that adds empty space between components. Use to add breathing room in layouts.",
    props: z.object({
      height: z.number().optional(),
    }),
  },

  WeatherCard: {
    description:
      "A weather display card showing location, temperature, condition, and icon. Use for weather information.",
    props: z.object({
      location: z.string(),
      temperature: z.string(),
      condition: z.string(),
      icon: z.string().optional(),
    }),
  },
} satisfies CatalogDefinitions;

export type MyDefinitions = typeof customDefinitions;
