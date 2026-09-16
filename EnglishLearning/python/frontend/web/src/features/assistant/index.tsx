import { CopilotChat } from "@copilotkit/react-core/v2";

/**
 * AI Assistant page.
 *
 * Mirrors AI-Demo/front/src/Chat.tsx. Renders a full-height CopilotChat
 * panel that talks to the english_pro_agent via the agent-runtime.
 *
 * The CopilotKit provider (which supplies runtimeUrl, agent id, and the
 * A2UI catalog) is mounted at the app root in providers/index.tsx, so
 * this page only needs to render the chat surface itself.
 */
export function AssistantPage() {
  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h1 className="text-4xl font-bold tracking-tight">
          AI English Tutor
        </h1>
        <p className="text-muted-foreground">
          Chat with EnglishPro Assistant to generate stories, learn words,
          and review your progress.
        </p>
      </div>

      <div className="flex justify-center items-stretch h-[70vh] w-full">
        <div className="h-full w-full max-w-4xl">
          <CopilotChat
            agentId="english_pro_agent"
            className="h-full rounded-2xl border bg-card shadow-sm"
            labels={{
              title: "EnglishPro Assistant",
              initialMessage:
                "Hi! I'm your English tutor. Ask me to generate a story from your words, explain a word, or show your vocabulary dashboard.",
              placeholder: "Ask anything about English learning...",
            }}
          />
        </div>
      </div>
    </div>
  );
}
