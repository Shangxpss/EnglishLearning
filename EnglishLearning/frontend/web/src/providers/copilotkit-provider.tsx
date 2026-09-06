import { CopilotKit, defineToolCallRenderer } from "@copilotkit/react-core/v2";
import { z } from "zod";
import { type ReactNode } from "react";
import { myCatalog } from "@/a2ui/catalog";
import "@copilotkit/react-core/v2/styles.css";

/**
 * Tool-call renderers that hide raw A2UI tool call data from the chat.
 *
 * Mirrors AI-Demo/front/src/Chat.tsx. The A2UI tools (log_a2ui_event,
 * render_a2ui) produce rich UI surfaces — showing their raw JSON args/result
 * is noise. Other tool calls get a clean one-line summary with a status dot.
 */
const cleanToolCallRenderers = [
  defineToolCallRenderer({
    name: "log_a2ui_event",
    args: z.any(),
    render: () => null as any,
  }),
  defineToolCallRenderer({
    name: "render_a2ui",
    args: z.any(),
    render: () => null as any,
  }),
  defineToolCallRenderer({
    name: "*",
    render: ({ name, status }: { name: string; status: string }) => {
      if (status === "complete") {
        return (
          <div className="flex items-center gap-1.5 px-1 py-0.5 text-xs text-gray-400">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-emerald-400" />
            <span>{name}</span>
          </div>
        );
      }
      return (
        <div className="flex items-center gap-1.5 px-1 py-0.5 text-xs text-gray-400">
          <span className="inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-gray-300" />
          <span>{name}…</span>
        </div>
      );
    },
  }),
];

interface CopilotKitProviderProps {
  children: ReactNode;
}

/**
 * Wraps the app in a CopilotKit provider configured for the EnglishPro
 * assistant.
 *
 * - `runtimeUrl="/copilotkit"` is proxied to the agent-runtime (Bun) by
 *   vite.config.ts during development.
 * - `agent="english_pro_agent"` matches the agent name registered in
 *   agent-runtime/index.ts and backend/app/api/copilotkit.py.
 * - `a2ui={{ catalog, includeSchema: true }}` enables rich UI rendering
 *   and injects the component schema into the LLM context.
 */
export function CopilotKitProvider({ children }: CopilotKitProviderProps) {
  return (
    <CopilotKit
      runtimeUrl="/copilotkit"
      agent="english_pro_agent"
      useSingleEndpoint={false}
      a2ui={{ catalog: myCatalog, includeSchema: true }}
      renderToolCalls={cleanToolCallRenderers}
    >
      {children}
    </CopilotKit>
  );
}
