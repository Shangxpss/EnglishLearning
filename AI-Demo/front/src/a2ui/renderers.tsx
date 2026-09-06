import React, { useRef } from "react";
import { type CatalogRenderers, useA2UIActions } from "@copilotkit/a2ui-renderer";

import type { MyDefinitions } from "./definitions";

// ─── Custom Component Renderers ───────────────────────────────────────
// Only renderers for components defined in definitions.ts.
// The 18 built-in components (Text, Row, Column, Card, List, Button,
// Divider, Tabs, Modal, Image, Icon, Video, AudioPlayer, TextField,
// CheckBox, ChoicePicker, Slider, DateTimeInput) are handled by the
// basic catalog included via `includeBasicCatalog: true` in createCatalog.
//
// The A2UIMiddleware on the runtime side injects the component schema into
// the LLM context (via includeSchema + injectA2UITool), so the LLM knows
// the exact component definitions. Invalid schemas will fail to render
// rather than silently producing broken UI.

export const myRenderers: CatalogRenderers<MyDefinitions> = {
  Heading: ({ props }) => (
    <div className="flex flex-col gap-1">
      <h2 className="text-xl font-bold text-gray-900">{props.title}</h2>
      {props.subtitle && (
        <p className="text-sm text-gray-500">{props.subtitle}</p>
      )}
    </div>
  ),

  Grid: ({ props, children }) => {
    const cols = props.columns ?? 2;
    const gap = props.gap ?? 2;
    return (
      <div className={`grid grid-cols-${cols} gap-${gap} w-full`}>
        {(props.children ?? []).map((id: string, i: number) => (
          <React.Fragment key={`${id}-${i}`}>{children(id)}</React.Fragment>
        ))}
      </div>
    );
  },

  Table: ({ props }) => (
    <div className="w-full overflow-hidden rounded-xl border border-gray-200 bg-white shadow-sm">
      {props.caption && (
        <div className="border-b border-gray-100 px-5 py-3">
          <h3 className="text-base font-semibold text-gray-900">{props.caption}</h3>
        </div>
      )}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-gray-200 bg-gray-50">
              {props.headers.map((h: string, i: number) => (
                <th key={i} className="px-4 py-2.5 text-left text-xs font-semibold uppercase tracking-wider text-gray-500">{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {props.rows.map((row: Record<string, string>, ri: number) => (
              <tr key={ri} className="border-b border-gray-100 last:border-b-0 hover:bg-gray-50/50">
                {props.headers.map((h: string, ci: number) => (
                  <td key={ci} className={`px-4 py-2.5 text-gray-900 ${ci === 0 ? "font-medium" : ""}`}>{row[h]}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  ),

  KeyValueList: ({ props }) => (
    <div className="divide-y divide-gray-100">
      {(props.items ?? []).map((item: any, i: number) => (
        <div key={i} className="flex items-baseline justify-between gap-4 px-5 py-2.5 hover:bg-gray-50/50">
          <span className="text-sm text-gray-500 shrink-0">{item.label ?? item.key}</span>
          <span className="text-sm font-medium text-gray-900 text-right">{item.value ?? item.description}</span>
        </div>
      ))}
    </div>
  ),

  Metric: ({ props }) => {
    const trend = props.trend ?? "neutral";
    const arrow = trend === "up" ? "↑" : trend === "down" ? "↓" : "";
    const trendColor = trend === "up" ? "text-emerald-600" : trend === "down" ? "text-red-600" : "text-gray-900";
    return (
      <div className="flex flex-col gap-1">
        <div className="text-xs font-medium uppercase tracking-wider text-gray-400">{props.label}</div>
        <div className={`flex items-baseline gap-1.5 text-2xl font-semibold tabular-nums ${trendColor}`}>
          <span>{props.value}</span>
          {arrow && <span className="text-base">{arrow}</span>}
        </div>
      </div>
    );
  },

  InfoRow: ({ props }) => (
    <div className="flex items-baseline justify-between gap-4 py-2 border-b border-gray-100 last:border-b-0">
      <span className="text-sm text-gray-500">{props.label}</span>
      <span className="text-sm font-medium text-gray-900">{props.value}</span>
    </div>
  ),

  StatusBadge: ({ props }) => {
    const variantStyles: Record<string, string> = {
      success: "bg-emerald-50 text-emerald-700 border-emerald-200",
      warning: "bg-amber-50 text-amber-700 border-amber-200",
      error: "bg-red-50 text-red-700 border-red-200",
      info: "bg-blue-50 text-blue-700 border-blue-200",
    };
    const style = variantStyles[props.variant ?? "info"] ?? variantStyles.info;
    return (
      <span className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium ${style}`}>
        {props.text}
      </span>
    );
  },

  BarChart: ({ props }) => {
    const maxVal = Math.max(...(props.bars?.map((b: any) => b.maxValue ?? b.value ?? 0) ?? [1]));
    const colorMap: Record<string, string> = {
      blue: "bg-blue-500", emerald: "bg-emerald-500", amber: "bg-amber-500",
      red: "bg-red-500", purple: "bg-purple-500", gray: "bg-gray-400",
    };
    return (
      <div className="flex flex-col gap-2 w-full">
        {props.title && <h3 className="text-sm font-semibold text-gray-900">{props.title}</h3>}
        {(props.bars ?? []).map((bar: any, i: number) => {
          const pct = maxVal > 0 ? ((bar.value ?? 0) / maxVal) * 100 : 0;
          return (
            <div key={i} className="flex items-center gap-3">
              <span className="w-24 text-xs text-gray-500 truncate">{bar.label}</span>
              <div className="flex-1 bg-gray-100 rounded-full h-4 overflow-hidden">
                <div className={`h-full rounded-full ${colorMap[bar.color ?? "blue"] ?? "bg-blue-500"}`} style={{ width: `${pct}%` }} />
              </div>
              <span className="text-xs font-medium text-gray-700 tabular-nums">{bar.value}</span>
            </div>
          );
        })}
      </div>
    );
  },

  PieChart: ({ props }) => {
    const data = props.data ?? [];
    const total = data.reduce((sum: number, d: any) => sum + (d.value ?? 0), 0);
    const colors = ["bg-blue-500", "bg-emerald-500", "bg-amber-500", "bg-red-500", "bg-purple-500", "bg-gray-400"];
    return (
      <div className="flex flex-col gap-3 w-full">
        {props.title && <h3 className="text-sm font-semibold text-gray-900">{props.title}</h3>}
        {props.description && <p className="text-xs text-gray-500">{props.description}</p>}
        <div className="flex flex-col gap-1.5">
          {data.map((item: any, i: number) => (
            <div key={i} className="flex items-center gap-2 text-sm">
              <span className={`inline-block h-3 w-3 rounded-sm ${colors[i % colors.length]}`} />
              <span className="flex-1 text-gray-700">{item.label}</span>
              <span className="text-gray-500 tabular-nums">{item.value}</span>
              {total > 0 && <span className="text-xs text-gray-400 tabular-nums">{((item.value / total) * 100).toFixed(0)}%</span>}
            </div>
          ))}
        </div>
      </div>
    );
  },

  UserCard: ({ props }) => (
    <div className="flex items-center gap-3 p-4 rounded-xl border border-gray-200 bg-white shadow-sm">
      {props.avatarUrl ? (
        <img src={props.avatarUrl} alt={props.name} className="w-10 h-10 rounded-full object-cover" />
      ) : (
        <div className="w-10 h-10 rounded-full bg-blue-100 flex items-center justify-center text-blue-600 font-semibold text-sm">
          {props.name?.charAt(0)?.toUpperCase()}
        </div>
      )}
      <div className="flex flex-col">
        <span className="text-sm font-semibold text-gray-900">{props.name}</span>
        <span className="text-xs text-gray-500">{props.email}</span>
        <span className="text-xs text-blue-600">{props.role}</span>
      </div>
    </div>
  ),

  // PrimaryButton: ({ props, dispatch }) => (
  //   <button
  //     onClick={() => { if (props.action && dispatch) dispatch(props.action); }}
  //     className="inline-flex items-center justify-center rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white shadow-sm hover:bg-blue-700 transition-colors"
  //   >
  //     {props.label}
  //   </button>
  // ),

  ResetButton: ({ props }) => {
    const { processMessages } = useA2UIActions();
    // Capture the initial data on first render, before DataModel.set('/', value)
    // can store it by reference and later mutate it in-place via user input.
    const initialDataRef = useRef(JSON.parse(JSON.stringify(props.data)));
    const handleClick = () => {
      processMessages([{
        version: "v0.9",
        updateDataModel: {
          surfaceId: props.surfaceId,
          path: "/",
          value: JSON.parse(JSON.stringify(initialDataRef.current)),
        },
      }]);
    };
    return (
      <button
        onClick={handleClick}
        className="inline-flex items-center justify-center rounded-lg border border-gray-300 bg-white px-4 py-2 text-sm font-medium text-gray-700 shadow-sm hover:bg-gray-50 transition-colors"
      >
        {props.label}
      </button>
    );
  },

  Spacer: ({ props }) => <div style={{ height: props.height ? `${props.height * 4}px` : "16px" }} />,

  WeatherCard: ({ props }) => (
    <div className="flex items-center gap-4 p-4 rounded-xl border border-gray-200 bg-white shadow-sm">
      {props.icon && <span className="text-3xl">{props.icon}</span>}
      <div className="flex flex-col">
        <span className="text-lg font-semibold text-gray-900">{props.temperature}</span>
        <span className="text-sm text-gray-500">{props.condition}</span>
        <span className="text-xs text-gray-400">{props.location}</span>
      </div>
    </div>
  ),
};
