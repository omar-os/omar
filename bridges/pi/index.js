import { jsonSchemaForPi, OmarMcpClient, piToolName } from "./omar-mcp.js";

function messageFor(error) {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Pi extension entrypoint. Discovery happens at session_start so loading the
 * extension itself never leaks an MCP child into `pi --list-models` or other
 * no-session invocations.
 */
export default function omarPiExtension(pi, { createClient = () => new OmarMcpClient() } = {}) {
  let client;

  const discover = async (ctx) => {
    if (client) return;
    const discoveringClient = createClient();
    client = discoveringClient;
    try {
      const tools = await discoveringClient.listTools();
      if (client !== discoveringClient) return;
      if (tools.length === 0) {
        throw new Error("OMAR MCP server returned no tools");
      }
      // Validate the complete list before registering anything. This map belongs
      // to this attempt only, so a registration failure cannot poison a retry.
      const names = new Map();
      for (const tool of tools) {
        if (typeof tool?.name !== "string" || !tool.name) {
          throw new Error("OMAR MCP server returned an invalid tool name");
        }
        const name = piToolName(tool.name);
        if (names.has(name)) {
          const originals = [names.get(name), tool.name].sort();
          throw new Error(`OMAR Pi tool name collision: ${originals.map((value) => JSON.stringify(value)).join(" and ")} map to ${name}`);
        }
        names.set(name, tool.name);
      }
      for (const tool of tools) {
        pi.registerTool({
          name: piToolName(tool.name),
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
      ctx?.ui?.notify?.(`OMAR: loaded ${tools.length} MCP tools`, "info");
    } catch (error) {
      if (client === discoveringClient) client = undefined;
      await discoveringClient.close();
      ctx?.ui?.notify?.(`OMAR MCP unavailable: ${messageFor(error)}`, "error");
    }
  };

  pi.on("session_start", async (_event, ctx) => {
    await discover(ctx);
  });

  pi.on("session_shutdown", async () => {
    const closingClient = client;
    client = undefined;
    await closingClient?.close();
  });
}

export { jsonSchemaForPi, OmarMcpClient, piToolName } from "./omar-mcp.js";
