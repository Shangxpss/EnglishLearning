import {
  CopilotRuntime,
  createCopilotRuntimeHandler,
  InMemoryAgentRunner,
} from "@copilotkit/runtime/v2";
import { HttpAgent } from "@ag-ui/client";

const AGENT_URL = process.env.AGENT_URL || "http://localhost:8000";

const agent = new HttpAgent({ url: `${AGENT_URL}/` });

const runtime = new CopilotRuntime({
  agents: { sample_agent: agent },
  a2ui: {
    injectA2UITool: true,
    defaultCatalogId: "generative-agent-catalog",
  },
  runner: new InMemoryAgentRunner(),
});

const handler = createCopilotRuntimeHandler({
  runtime,
  basePath: "/copilotkit",
});

const PORT = parseInt(process.env.PORT || "4000", 10);

Bun.serve({
  port: PORT,
  async fetch(req) {
    console.log(`${req.method} ${new URL(req.url).pathname}`);

    const res = await handler(req);
    // Add CORS headers to all responses
    res.headers.set("Access-Control-Allow-Origin", "*");
    res.headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    res.headers.set(
      "Access-Control-Allow-Headers",
      "Content-Type, Authorization",
    );
    return res;
  },
});

console.log(`CopilotKit Runtime running on http://localhost:${PORT}`);
