import { jsonSchemaForPi, OmarMcpClient, piToolName } from "./omar-mcp.js";

function messageFor(error) {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Pi extension entrypoint. Discovery happens at session_start so loading the
 * extension itself never leaks an MCP child into `pi --list-models` or other
 * no-session invocations.
 */
export default function omarPiExtension(pi) {
  let client;
  let discovered = new Map();

  const discover = async (ctx) => {
    if (client) return;
    client = new OmarMcpClient();
    try {
      const tools = await client.listTools();
      for (const tool of tools) {
        if (!tool?.name || discovered.has(tool.name)) continue;
        discovered.set(tool.name, tool);
        const name = piToolName(tool.name);
        pi.registerTool({
          name,
          label: `OMAR ${tool.name}`,
          description: tool.description || `Call OMAR MCP tool ${tool.name}`,
          promptSnippet: `Call OMAR MCP tool ${tool.name}`,
          parameters: jsonSchemaForPi(tool.inputSchema),
          async execute(_toolCallId, params, signal) {
            if (!client) throw new Error("OMAR MCP session is not active");
            return client.callTool(tool.name, params, signal);
          },
        });
      }
      if (tools.length === 0) {
        throw new Error("OMAR MCP server returned no tools");
      }
      ctx?.ui?.notify?.(`OMAR: loaded ${tools.length} MCP tools`, "info");
    } catch (error) {
      client?.close();
      client = undefined;
      ctx?.ui?.notify?.(`OMAR MCP unavailable: ${messageFor(error)}`, "error");
    }
  };

  pi.on("session_start", async (_event, ctx) => {
    await discover(ctx);
  });

  pi.on("session_shutdown", async () => {
    client?.close();
    client = undefined;
    discovered = new Map();
  });
}

export { jsonSchemaForPi, OmarMcpClient, piToolName } from "./omar-mcp.js";
