import React from "react";
import { type CatalogRenderers } from "@copilotkit/a2ui-renderer";

import type { MyDefinitions } from "./definitions";

/**
 * Custom A2UI component renderers for the EnglishPro assistant.
 *
 * Mirrors AI-Demo/front/src/a2ui/renderers.tsx and adds renderers for the
 * English-learning-specific components defined in definitions.ts
 * (WordCard, StoryCard, GrammarCard, VocabularyDashboard).
 *
 * The 18 built-in components are handled by the basic catalog included via
 * `includeBasicCatalog: true` in catalog.ts. The A2UIMiddleware on the
 * runtime side injects the component schema into the LLM context, so the
 * LLM knows the exact component definitions.
 */
export const myRenderers: CatalogRenderers<MyDefinitions> = {
  // ── Generic dashboard components (ported from AI-Demo) ────────────────
  Heading: ({ props }) => (
    <div className="flex flex-col gap-1">
      <h2 className="text-xl font-bold text-gray-900 dark:text-gray-100">
        {props.title}
      </h2>
      {props.subtitle && (
        <p className="text-sm text-gray-500 dark:text-gray-400">
          {props.subtitle}
        </p>
      )}
    </div>
  ),

  Grid: ({ props, children }) => {
    const cols = props.columns ?? 2;
    const gap = props.gap ?? 2;
    return (
      <div
        className="grid w-full"
        style={{
          gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`,
          gap: `${gap * 0.25}rem`,
        }}
      >
        {(props.children ?? []).map((id: string, i: number) => (
          <React.Fragment key={`${id}-${i}`}>{children(id)}</React.Fragment>
        ))}
      </div>
    );
  },

  Table: ({ props }) => (
    <div className="w-full overflow-hidden rounded-xl border border-gray-200 bg-white shadow-sm dark:border-gray-700 dark:bg-gray-800">
      {props.caption && (
        <div className="border-b border-gray-100 px-5 py-3 dark:border-gray-700">
          <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100">
            {props.caption}
          </h3>
        </div>
      )}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-gray-200 bg-gray-50 dark:border-gray-700 dark:bg-gray-900/50">
              {props.headers.map((h: string, i: number) => (
                <th
                  key={i}
                  className="px-4 py-2.5 text-left text-xs font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400"
                >
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {props.rows.map((row: Record<string, string>, ri: number) => (
              <tr
                key={ri}
                className="border-b border-gray-100 last:border-b-0 hover:bg-gray-50/50 dark:border-gray-700 dark:hover:bg-gray-700/30"
              >
                {props.headers.map((h: string, ci: number) => (
                  <td
                    key={ci}
                    className={`px-4 py-2.5 text-gray-900 dark:text-gray-100 ${ci === 0 ? "font-medium" : ""}`}
                  >
                    {row[h]}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  ),

  KeyValueList: ({ props }) => (
    <div className="divide-y divide-gray-100 dark:divide-gray-700">
      {(props.items ?? []).map((item: any, i: number) => (
        <div
          key={i}
          className="flex items-baseline justify-between gap-4 px-5 py-2.5 hover:bg-gray-50/50 dark:hover:bg-gray-700/30"
        >
          <span className="shrink-0 text-sm text-gray-500 dark:text-gray-400">
            {item.label ?? item.key}
          </span>
          <span className="text-right text-sm font-medium text-gray-900 dark:text-gray-100">
            {item.value ?? item.description}
          </span>
        </div>
      ))}
    </div>
  ),

  Metric: ({ props }) => {
    const trend = props.trend ?? "neutral";
    const arrow = trend === "up" ? "↑" : trend === "down" ? "↓" : "";
    const trendColor =
      trend === "up"
        ? "text-emerald-600"
        : trend === "down"
          ? "text-red-600"
          : "text-gray-900 dark:text-gray-100";
    return (
      <div className="flex flex-col gap-1">
        <div className="text-xs font-medium uppercase tracking-wider text-gray-400">
          {props.label}
        </div>
        <div
          className={`flex items-baseline gap-1.5 text-2xl font-semibold tabular-nums ${trendColor}`}
        >
          <span>{props.value}</span>
          {arrow && <span className="text-base">{arrow}</span>}
        </div>
      </div>
    );
  },

  InfoRow: ({ props }) => (
    <div className="flex items-baseline justify-between gap-4 border-b border-gray-100 py-2 last:border-b-0 dark:border-gray-700">
      <span className="text-sm text-gray-500 dark:text-gray-400">
        {props.label}
      </span>
      <span className="text-sm font-medium text-gray-900 dark:text-gray-100">
        {props.value}
      </span>
    </div>
  ),

  StatusBadge: ({ props }) => {
    const variantStyles: Record<string, string> = {
      success: "bg-emerald-50 text-emerald-700 border-emerald-200 dark:bg-emerald-900/30 dark:text-emerald-300 dark:border-emerald-700",
      warning: "bg-amber-50 text-amber-700 border-amber-200 dark:bg-amber-900/30 dark:text-amber-300 dark:border-amber-700",
      error: "bg-red-50 text-red-700 border-red-200 dark:bg-red-900/30 dark:text-red-300 dark:border-red-700",
      info: "bg-blue-50 text-blue-700 border-blue-200 dark:bg-blue-900/30 dark:text-blue-300 dark:border-blue-700",
    };
    const style = variantStyles[props.variant ?? "info"] ?? variantStyles.info;
    return (
      <span
        className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium ${style}`}
      >
        {props.text}
      </span>
    );
  },

  BarChart: ({ props }) => {
    const maxVal = Math.max(
      ...(props.bars?.map((b: any) => b.maxValue ?? b.value ?? 0) ?? [1]),
    );
    const colorMap: Record<string, string> = {
      blue: "bg-blue-500",
      emerald: "bg-emerald-500",
      amber: "bg-amber-500",
      red: "bg-red-500",
      purple: "bg-purple-500",
      gray: "bg-gray-400",
    };
    return (
      <div className="flex w-full flex-col gap-2">
        {props.title && (
          <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {props.title}
          </h3>
        )}
        {(props.bars ?? []).map((bar: any, i: number) => {
          const pct = maxVal > 0 ? ((bar.value ?? 0) / maxVal) * 100 : 0;
          return (
            <div key={i} className="flex items-center gap-3">
              <span className="w-24 truncate text-xs text-gray-500 dark:text-gray-400">
                {bar.label}
              </span>
              <div className="h-4 flex-1 overflow-hidden rounded-full bg-gray-100 dark:bg-gray-700">
                <div
                  className={`h-full rounded-full ${colorMap[bar.color ?? "blue"] ?? "bg-blue-500"}`}
                  style={{ width: `${pct}%` }}
                />
              </div>
              <span className="text-xs font-medium tabular-nums text-gray-700 dark:text-gray-300">
                {bar.value}
              </span>
            </div>
          );
        })}
      </div>
    );
  },

  PieChart: ({ props }) => {
    const data = props.data ?? [];
    const total = data.reduce((sum: number, d: any) => sum + (d.value ?? 0), 0);
    const colors = [
      "bg-blue-500",
      "bg-emerald-500",
      "bg-amber-500",
      "bg-red-500",
      "bg-purple-500",
      "bg-gray-400",
    ];
    return (
      <div className="flex w-full flex-col gap-3">
        {props.title && (
          <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {props.title}
          </h3>
        )}
        {props.description && (
          <p className="text-xs text-gray-500 dark:text-gray-400">
            {props.description}
          </p>
        )}
        <div className="flex flex-col gap-1.5">
          {data.map((item: any, i: number) => (
            <div key={i} className="flex items-center gap-2 text-sm">
              <span
                className={`inline-block h-3 w-3 rounded-sm ${colors[i % colors.length]}`}
              />
              <span className="flex-1 text-gray-700 dark:text-gray-300">
                {item.label}
              </span>
              <span className="tabular-nums text-gray-500 dark:text-gray-400">
                {item.value}
              </span>
              {total > 0 && (
                <span className="text-xs tabular-nums text-gray-400">
                  {((item.value / total) * 100).toFixed(0)}%
                </span>
              )}
            </div>
          ))}
        </div>
      </div>
    );
  },

  Spacer: ({ props }) => (
    <div style={{ height: props.height ? `${props.height * 4}px` : "16px" }} />
  ),

  // ── English-learning-specific components ─────────────────────────────
  WordCard: ({ props }) => {
    const familiarityVariant: Record<string, string> = {
      new: "info",
      learning: "warning",
      familiar: "info",
      mastered: "success",
    };
    const variant = familiarityVariant[props.familiarity ?? "new"] ?? "info";
    const variantStyles: Record<string, string> = {
      success: "bg-emerald-50 text-emerald-700 border-emerald-200",
      warning: "bg-amber-50 text-amber-700 border-amber-200",
      info: "bg-blue-50 text-blue-700 border-blue-200",
      error: "bg-red-50 text-red-700 border-red-200",
    };
    return (
      <div className="rounded-xl border border-gray-200 bg-white p-5 shadow-sm dark:border-gray-700 dark:bg-gray-800">
        <div className="mb-3 flex items-center justify-between">
          <h3 className="text-2xl font-bold text-indigo-700 dark:text-indigo-400">
            {props.word}
          </h3>
          <span
            className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium ${variantStyles[variant]}`}
          >
            {(props.familiarity ?? "new").charAt(0).toUpperCase() +
              (props.familiarity ?? "new").slice(1)}
          </span>
        </div>
        <p className="mb-2 text-sm text-gray-700 dark:text-gray-300">
          <span className="font-semibold">Definition: </span>
          {props.definition}
        </p>
        <p className="text-sm italic text-gray-600 dark:text-gray-400">
          <span className="not-italic font-semibold">Example: </span>
          {props.example}
        </p>
      </div>
    );
  },

  StoryCard: ({ props }) => (
    <div className="rounded-xl border border-gray-200 bg-white p-6 shadow-sm dark:border-gray-700 dark:bg-gray-800">
      <h3 className="mb-2 text-xl font-bold text-gray-900 dark:text-gray-100">
        {props.title}
      </h3>
      {props.tone && (
        <p className="mb-3 text-xs uppercase tracking-wider text-gray-400">
          Tone: {props.tone}
        </p>
      )}
      {props.words.length > 0 && (
        <div className="mb-4 flex flex-wrap gap-2">
          {props.words.map((word: string, i: number) => (
            <span
              key={i}
              className="rounded-full bg-indigo-100 px-3 py-1 text-xs font-medium text-indigo-800 dark:bg-indigo-900/40 dark:text-indigo-300"
            >
              {word}
            </span>
          ))}
        </div>
      )}
      <p className="whitespace-pre-wrap text-sm leading-relaxed text-gray-700 dark:text-gray-300">
        {props.body}
      </p>
    </div>
  ),

  GrammarCard: ({ props }) => (
    <div className="rounded-xl border border-gray-200 bg-white p-6 shadow-sm dark:border-gray-700 dark:bg-gray-800">
      <h3 className="mb-3 text-xl font-bold text-indigo-700 dark:text-indigo-400">
        {props.topic}
      </h3>
      <p className="mb-4 text-sm leading-relaxed text-gray-700 dark:text-gray-300">
        {props.explanation}
      </p>
      <div className="rounded-lg bg-gray-50 p-4 dark:bg-gray-900/50">
        <h4 className="mb-2 text-xs font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400">
          Examples
        </h4>
        <ul className="space-y-2">
          {props.examples.map((ex: string, i: number) => (
            <li
              key={i}
              className="text-sm italic text-gray-600 dark:text-gray-400"
            >
              {ex}
            </li>
          ))}
        </ul>
      </div>
    </div>
  ),

  VocabularyDashboard: ({ props }) => (
    <div className="rounded-xl border border-gray-200 bg-white p-6 shadow-sm dark:border-gray-700 dark:bg-gray-800">
      <h3 className="mb-4 text-xl font-bold text-gray-900 dark:text-gray-100">
        Vocabulary Dashboard
      </h3>
      <div className="mb-6 grid grid-cols-3 gap-4">
        <div className="rounded-lg bg-blue-50 p-4 dark:bg-blue-900/20">
          <div className="text-xs font-medium uppercase tracking-wider text-gray-400">
            Total Words
          </div>
          <div className="text-2xl font-semibold tabular-nums text-gray-900 dark:text-gray-100">
            {props.total}
          </div>
        </div>
        <div className="rounded-lg bg-amber-50 p-4 dark:bg-amber-900/20">
          <div className="text-xs font-medium uppercase tracking-wider text-gray-400">
            Learning
          </div>
          <div className="flex items-baseline gap-1.5 text-2xl font-semibold tabular-nums text-amber-600">
            <span>{props.learning}</span>
            <span className="text-base">↑</span>
          </div>
        </div>
        <div className="rounded-lg bg-emerald-50 p-4 dark:bg-emerald-900/20">
          <div className="text-xs font-medium uppercase tracking-wider text-gray-400">
            Mastered
          </div>
          <div className="flex items-baseline gap-1.5 text-2xl font-semibold tabular-nums text-emerald-600">
            <span>{props.mastered}</span>
            <span className="text-base">↑</span>
          </div>
        </div>
      </div>
      <div className="mb-6">
        <h4 className="mb-3 text-sm font-semibold text-gray-900 dark:text-gray-100">
          Words by Familiarity
        </h4>
        <BarChartRenderer bars={props.bars} />
      </div>
      <div>
        <h4 className="mb-3 text-sm font-semibold text-gray-900 dark:text-gray-100">
          Recent Words
        </h4>
        <div className="overflow-hidden rounded-lg border border-gray-200 dark:border-gray-700">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-200 bg-gray-50 dark:border-gray-700 dark:bg-gray-900/50">
                <th className="px-4 py-2 text-left text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Word
                </th>
                <th className="px-4 py-2 text-left text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Familiarity
                </th>
                <th className="px-4 py-2 text-left text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Score
                </th>
              </tr>
            </thead>
            <tbody>
              {props.recentWords.map((w: any, i: number) => (
                <tr
                  key={i}
                  className="border-b border-gray-100 last:border-b-0 dark:border-gray-700"
                >
                  <td className="px-4 py-2 font-medium text-gray-900 dark:text-gray-100">
                    {w.word}
                  </td>
                  <td className="px-4 py-2 text-gray-700 dark:text-gray-300">
                    {w.familiarity}
                  </td>
                  <td className="px-4 py-2 tabular-nums text-gray-700 dark:text-gray-300">
                    {w.score}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  ),
};

// Helper renderer for the BarChart used inside VocabularyDashboard.
function BarChartRenderer({ bars }: { bars: any[] }) {
  const maxVal = Math.max(
    ...(bars?.map((b: any) => b.value ?? 0) ?? [1]),
    1,
  );
  const colorMap: Record<string, string> = {
    blue: "bg-blue-500",
    emerald: "bg-emerald-500",
    amber: "bg-amber-500",
    red: "bg-red-500",
    purple: "bg-purple-500",
    gray: "bg-gray-400",
  };
  return (
    <div className="flex flex-col gap-2">
      {(bars ?? []).map((bar: any, i: number) => {
        const pct = maxVal > 0 ? ((bar.value ?? 0) / maxVal) * 100 : 0;
        return (
          <div key={i} className="flex items-center gap-3">
            <span className="w-24 truncate text-xs text-gray-500 dark:text-gray-400">
              {bar.label}
            </span>
            <div className="h-4 flex-1 overflow-hidden rounded-full bg-gray-100 dark:bg-gray-700">
              <div
                className={`h-full rounded-full ${colorMap[bar.color ?? "blue"] ?? "bg-blue-500"}`}
                style={{ width: `${pct}%` }}
              />
            </div>
            <span className="text-xs font-medium tabular-nums text-gray-700 dark:text-gray-300">
              {bar.value}
            </span>
          </div>
        );
      })}
    </div>
  );
}
