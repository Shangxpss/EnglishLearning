import { z } from "zod";
import type { CatalogDefinitions } from "@copilotkit/a2ui-renderer";

/**
 * Custom A2UI component definitions for the EnglishPro assistant.
 *
 * Mirrors AI-Demo/front/src/a2ui/definitions.ts but adds English-learning-
 * specific components (WordCard, StoryCard, VocabularyDashboard, GrammarCard)
 * on top of the generic dashboard components inherited from the AI-Demo.
 *
 * The 18 built-in components (Text, Row, Column, Card, List, Button,
 * Divider, Tabs, Modal, Image, Icon, Video, AudioPlayer, TextField,
 * CheckBox, ChoicePicker, Slider, DateTimeInput) are provided by the basic
 * catalog via `includeBasicCatalog: true` in catalog.ts — we only define
 * custom components here, and the A2UIMiddleware injects the schema into
 * the LLM context automatically.
 */
export const customDefinitions = {
  // ── Generic dashboard components (ported from AI-Demo) ────────────────
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
      "A data table with headers and rows. Provide `headers` (array of column labels) and `rows` (array of row objects, each key matching a header). Renders a clean, styled HTML table. Ideal for vocabulary lists, comparisons, schedules, etc.",
    props: z.object({
      headers: z.array(z.string()),
      rows: z.array(z.record(z.string())),
      caption: z.string().optional(),
    }),
  },

  KeyValueList: {
    description:
      "A vertical list of label-value pairs, rendered as a clean definition list. Each item has a `label` (bold, left) and `value` (right). Perfect for word definitions, grammar rules, metadata — anything that is a flat list of key-value pairs.",
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
      "A small coloured pill communicating the state of something (new/learning/familiar/mastered, healthy/degraded/down). Choose `variant` to match the intent.",
    props: z.object({
      text: z.string(),
      variant: z.enum(["success", "warning", "error", "info"]).optional(),
    }),
  },

  Metric: {
    description:
      "A key/value KPI display with an optional trend indicator. Ideal for dashboards (e.g. 'Total Words - 42 - up').",
    props: z.object({
      label: z.string(),
      value: z.string(),
      trend: z.enum(["up", "down", "neutral"]).optional(),
    }),
  },

  InfoRow: {
    description:
      "A compact two-column 'label: value' row. Good for stacks of facts inside a Card (word, definition, example, etc.).",
    props: z.object({
      label: z.string(),
      value: z.string(),
    }),
  },

  BarChart: {
    description:
      "A horizontal bar chart for comparing values across categories. Each bar has a label, value, and optional color. Use for vocabulary familiarity breakdowns, skill levels, etc.",
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
      "A pie/donut chart with a coloured legend. Provide `title`, `description`, and `data` as an array of `{ label, value }` objects. Great for part-of-whole breakdowns (words by familiarity, learning sources, etc.).",
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

  Spacer: {
    description:
      "A vertical spacer that adds empty space between components. Use to add breathing room in layouts.",
    props: z.object({
      height: z.number().optional(),
    }),
  },

  // ── English-learning-specific components ─────────────────────────────
  WordCard: {
    description:
      "A vocabulary card showing a word, its definition, an example sentence, and a familiarity badge. Use when the learner asks about a specific word.",
    props: z.object({
      word: z.string(),
      definition: z.string(),
      example: z.string(),
      familiarity: z
        .enum(["new", "learning", "familiar", "mastered"])
        .optional(),
    }),
  },

  StoryCard: {
    description:
      "A card displaying a generated story with a title, the source words, and the story body. Use to render stories produced by the generate_story tool.",
    props: z.object({
      title: z.string(),
      words: z.array(z.string()),
      body: z.string(),
      tone: z.string().optional(),
    }),
  },

  GrammarCard: {
    description:
      "A card explaining a grammar topic with a title, an explanation paragraph, and a list of example sentences. Use when the learner asks about tenses, articles, conditionals, etc.",
    props: z.object({
      topic: z.string(),
      explanation: z.string(),
      examples: z.array(z.string()),
    }),
  },

  VocabularyDashboard: {
    description:
      "A dashboard summarising the learner's saved vocabulary: total/learning/mastered metrics, a bar chart of words by familiarity, and a table of recent words. Use when the learner asks for an overview of their progress.",
    props: z.object({
      total: z.number(),
      learning: z.number(),
      mastered: z.number(),
      bars: z.array(
        z.object({
          label: z.string(),
          value: z.number(),
          color: z
            .enum(["blue", "emerald", "amber", "red", "purple", "gray"])
            .optional(),
        }),
      ),
      recentWords: z.array(
        z.object({
          word: z.string(),
          familiarity: z.string(),
          score: z.number(),
        }),
      ),
    }),
  },
} satisfies CatalogDefinitions;

export type MyDefinitions = typeof customDefinitions;
