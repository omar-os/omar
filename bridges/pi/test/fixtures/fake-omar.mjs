#!/usr/bin/env node
import readline from "node:readline";

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  const request = JSON.parse(line);
  if (!request.id) return;
  let result = {};
  if (request.method === "initialize") {
    result = { protocolVersion: "2024-11-05" };
  } else if (request.method === "tools/list") {
    result = {
      tools: [{
        name: "spawn_agent",
        description: "Spawn an agent",
        inputSchema: { type: "object", properties: { name: { type: "string" } } },
      }],
    };
  } else if (request.method === "tools/call") {
    result = {
      content: [{ type: "text", text: "fake-spawn-agent" }],
      structuredContent: { agents: [] },
    };
  }
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\n`);
});
