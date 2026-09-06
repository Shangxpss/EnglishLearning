import { CopilotKit, defineToolCallRenderer, CopilotPopup } from '@copilotkit/react-core/v2'
import { z } from 'zod'
import { myCatalog } from './a2ui/catalog'
import '@copilotkit/react-core/v2/styles.css'

// Hide raw A2UI tool call data (log_a2ui_event, render_a2ui) from chat.
// These tools produce rich UI surfaces — showing their raw JSON args/result
// is noise. Other tool calls get a clean one-line summary.
const cleanToolCallRenderers = [
  defineToolCallRenderer({
    name: 'log_a2ui_event',
    args: z.any(),
    render: () => null as any,
  }),
  defineToolCallRenderer({
    name: 'render_a2ui',
    args: z.any(),
    render: () => null as any,
  }),
  defineToolCallRenderer({
    name: '*',
    render: ({ name, status }: { name: string; status: string }) => {
      if (status === 'complete') {
        return (
          <div className="flex items-center gap-1.5 px-1 py-0.5 text-xs text-gray-400">
            <span className="inline-block h-1.5 w-1.5 rounded-full bg-emerald-400" />
            <span>{name}</span>
          </div>
        )
      }
      return (
        <div className="flex items-center gap-1.5 px-1 py-0.5 text-xs text-gray-400">
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-gray-300 animate-pulse" />
          <span>{name}…</span>
        </div>
      )
    },
  }),
]

function App() {
  return (
    <CopilotKit
      runtimeUrl="/copilotkit"
      agent="sample_agent"
      useSingleEndpoint={false}
      a2ui={{ catalog: myCatalog, includeSchema: true }}
      renderToolCalls={cleanToolCallRenderers}
    >
      <main className="h-screen w-screen">
        <div className="flex items-center justify-center h-full">
          <p className="text-gray-400 text-lg">Your main content here</p>
        </div>
        <CopilotPopup
          agentId="sample_agent"
          defaultOpen={true}
          labels={{
            modalHeaderTitle: "Assistant",
            welcomeMessageText: "Need any help?",
          }}
        />
      </main>
    </CopilotKit>
  )
}

export default App
