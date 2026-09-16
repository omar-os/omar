import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { chmod } from "node:fs/promises";
import { resolve } from "node:path";
import { PassThrough } from "node:stream";
import { spawn } from "node:child_process";
import test from "node:test";

import {
  jsonSchemaForPi,
  OmarMcpClient,
  piResultFromMcp,
  piToolName,
} from "../omar-mcp.js";

class FakeChild extends EventEmitter {
  constructor() {
    super();
    this.stdout = new PassThrough();
    this.stdin = {
      writes: [],
      write: (payload) => {
        this.stdin.writes.push(JSON.parse(payload));
        const request = this.stdin.writes.at(-1);
        if (request.method === "initialize") {
          this.reply(request.id, { protocolVersion: "2024-11-05" });
        } else if (request.method === "tools/list") {
          this.reply(request.id, {
            tools: [{ name: "list_agents", description: "List agents", inputSchema: { type: "object" } }],
          });
        } else if (request.method === "tools/call") {
          this.reply(request.id, {
            content: [{ type: "text", text: JSON.stringify(request.params.arguments) }],
            structuredContent: { ok: true },
          });
        }
      },
    };
    this.killed = false;
  }

  reply(id, result) {
    this.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
  }

  kill() {
    this.killed = true;
    this.emit("close", 0, null);
  }
}

test("uses an unambiguous prefixed Pi tool name", () => {
  assert.equal(piToolName("list_agents"), "omar_list_agents");
  assert.equal(piToolName("tool/name"), "omar_tool_name");
});

test("keeps MCP JSON Schema without a TypeBox runtime dependency", () => {
  const schema = { type: "object", properties: { limit: { type: "integer" } } };
  assert.equal(jsonSchemaForPi(schema), schema);
  assert.deepEqual(jsonSchemaForPi(undefined), {
    type: "object",
    properties: {},
    additionalProperties: false,
  });
});

test("initializes, lists, and calls tools over OMAR JSONL MCP", async () => {
  let child;
  const client = new OmarMcpClient({
    timeoutMs: 1000,
    spawnProcess: () => {
      child = new FakeChild();
      return child;
    },
  });

  const tools = await client.listTools();
  assert.equal(tools[0].name, "list_agents");
  const result = await client.callTool("list_agents", { limit: 2 });
  assert.deepEqual(result.details.structuredContent, { ok: true });
  assert.equal(result.content[0].text, '{"limit":2}');
  assert.equal(child.stdin.writes[0].method, "initialize");
  assert.equal(child.stdin.writes[1].method, "notifications/initialized");
  client.close();
  assert.equal(child.killed, true);
});

test("uses an exact per-context OMAR MCP command", () => {
  const client = new OmarMcpClient({
    binary: "/custom/omar",
    env: { OMAR_MCP_CONTEXT_FILE: "/tmp/context.json" },
  });
  assert.deepEqual(client.args, ["mcp-server", "--context-file", "/tmp/context.json"]);

  const previous = process.env.OMAR_MCP_CONTEXT_FILE;
  process.env.OMAR_MCP_CONTEXT_FILE = "/tmp/context.json";
  try {
    const contextual = new OmarMcpClient({ binary: "/custom/omar" });
    assert.deepEqual(contextual.args, ["mcp-server", "--context-file", "/tmp/context.json"]);
  } finally {
    if (previous === undefined) delete process.env.OMAR_MCP_CONTEXT_FILE;
    else process.env.OMAR_MCP_CONTEXT_FILE = previous;
  }
});

test("turns MCP tool errors into Pi tool errors", () => {
  assert.throws(
    () => piResultFromMcp({ isError: true, content: [{ type: "text", text: "nope" }] }, "x"),
    /nope/,
  );
});

test("is runnable by Pi 0.85.1 in RPC mode", async () => {
  const fakeOmar = resolve("bridges/pi/test/fixtures/fake-omar.mjs");
  await chmod(fakeOmar, 0o755);
  const pi = spawn(
    process.env.PI_BINARY || "npx",
    process.env.PI_BINARY
      ? ["--mode", "rpc", "--no-session", "--offline", "-e", resolve("bridges/pi/index.js")]
      : ["--yes", "--package=@earendil-works/pi-coding-agent@0.85.1", "--", "pi", "--mode", "rpc", "--no-session", "--offline", "-e", resolve("bridges/pi/index.js")],
    {
      cwd: resolve("."),
      env: { ...process.env, OMAR_BINARY: fakeOmar },
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  let output = "";
  pi.stdout.setEncoding("utf8");
  pi.stdout.on("data", (chunk) => { output += chunk; });
  pi.stdin.write('{"id":"state","type":"get_state"}\n');
  await new Promise((resolvePromise, reject) => {
    const timeout = setTimeout(() => reject(new Error(`Pi RPC startup timed out: ${output}`)), 30_000);
    const check = () => {
      if (output.includes('"command":"get_state"')) {
        clearTimeout(timeout);
        resolvePromise();
      } else setTimeout(check, 25);
    };
    check();
  });
  assert.match(output, /"success":true/);
  pi.kill();
});

test("registers and calls the discovered omar_spawn_agent Pi tool", async () => {
  const { default: extension } = await import("../index.js");
  const handlers = new Map();
  const pi = {
    on(event, handler) {
      handlers.set(event, handler);
    },
    registerTool(tool) {
      handlers.set(tool.name, tool);
    },
  };
  const previousBinary = process.env.OMAR_BINARY;
  const previousContext = process.env.OMAR_MCP_CONTEXT_FILE;
  process.env.OMAR_BINARY = resolve("bridges/pi/test/fixtures/fake-omar.mjs");
  delete process.env.OMAR_MCP_CONTEXT_FILE;
  try {
    extension(pi);
    await handlers.get("session_start")({}, { ui: { notify() {} } });
    assert.ok(handlers.has("omar_spawn_agent"));
    const result = await handlers.get("omar_spawn_agent").execute("call-1", { name: "worker" });
    assert.equal(result.content[0].text, "fake-spawn-agent");
    await handlers.get("session_shutdown")();
  } finally {
    if (previousBinary === undefined) delete process.env.OMAR_BINARY;
    else process.env.OMAR_BINARY = previousBinary;
    if (previousContext === undefined) delete process.env.OMAR_MCP_CONTEXT_FILE;
    else process.env.OMAR_MCP_CONTEXT_FILE = previousContext;
  }
});
