import assert from "node:assert/strict";
import { EventEmitter, once, getEventListeners } from "node:events";
import { chmod } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { PassThrough } from "node:stream";
import { spawn } from "node:child_process";
import test from "node:test";
import extension from "../index.js";

const fakeOmarPath = fileURLToPath(new URL("./fixtures/fake-omar.mjs", import.meta.url));
const extensionPath = fileURLToPath(new URL("../index.js", import.meta.url));

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

  kill(signal) {
    this.killed = true;
    this.emit("close", null, signal);
  }
}

test("prefixes and sanitizes Pi tool names (discovery checks collisions)", () => {
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
  await client.close();
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

function extensionHarness(tools, register = () => {}) {
  const handlers = new Map();
  const registered = new Map();
  const notices = [];
  const clients = [];
  extension({
    on: (event, handler) => handlers.set(event, handler),
    registerTool: (tool) => {
      register(tool);
      registered.set(tool.name, tool);
    },
  }, {
    createClient: () => {
      const client = {
        closed: false,
        listTools: async () => tools,
        close: async () => { client.closed = true; },
        callTool: async (name) => ({ name }),
      };
      clients.push(client);
      return client;
    },
  });
  return {
    registered, notices, clients,
    start: () => handlers.get("session_start")({}, { ui: { notify: (text) => notices.push(text) } }),
    stop: () => handlers.get("session_shutdown")(),
  };
}

test("retries every tool after partial registration failure and session shutdown", async () => {
  let fail = true;
  const calls = [];
  const harness = extensionHarness([{ name: "first" }, { name: "second" }], (tool) => {
    calls.push(tool.name);
    if (fail && tool.name === "omar_second") throw new Error("registration failed");
  });
  await harness.start();
  assert.equal(harness.clients[0].closed, true);
  assert.match(harness.notices[0], /registration failed/);
  await assert.rejects(harness.registered.get("omar_first").execute("id", {}), /not active/);
  fail = false;
  await harness.start();
  assert.deepEqual(calls, ["omar_first", "omar_second", "omar_first", "omar_second"]);
  assert.deepEqual(await harness.registered.get("omar_second").execute("id", {}), { name: "second" });
  await harness.start();
  assert.equal(harness.clients.length, 2);
  await harness.stop();
  await harness.start();
  assert.equal(harness.clients.length, 3);
  assert.equal(calls.length, 6);
  await harness.stop();
});

test("rejects name collisions before any registration, regardless of order", async () => {
  const messages = [];
  for (const names of [["tool/name", "tool_name"], ["tool_name", "tool/name"]]) {
    const harness = extensionHarness(names.map((name) => ({ name })));
    await harness.start();
    assert.equal(harness.registered.size, 0);
    assert.equal(harness.clients[0].closed, true);
    assert.match(harness.notices[0], /collision.*tool\/name.*tool_name.*omar_tool_name/);
    messages.push(harness.notices[0]);
  }
  assert.equal(messages[0], messages[1]);
});

test("disables registered tools before asynchronous shutdown finishes", async () => {
  const harness = extensionHarness([{ name: "first" }]);
  await harness.start();
  let finishClose;
  harness.clients[0].close = () => new Promise((resolve) => { finishClose = resolve; });
  const stopping = harness.stop();
  await assert.rejects(harness.registered.get("omar_first").execute("id", {}), /not active/);
  finishClose();
  await stopping;
});

test("rejects duplicate or invalid names and empty discovery, then allows retry", async () => {
  for (const tools of [[], [{ name: "" }], [{ name: 42 }], [{ name: "a" }, { name: "a" }]]) {
    const harness = extensionHarness(tools);
    await harness.start();
    assert.equal(harness.registered.size, 0);
    assert.equal(harness.clients[0].closed, true);
    tools.splice(0, tools.length, { name: "valid" });
    await harness.start();
    assert.ok(harness.registered.has("omar_valid"));
    await harness.stop();
  }
});

test("close escalates once, waits for exit, and clears its timer", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const child = new FakeChild();
  const signals = [];
  child.kill = (signal) => {
    child.killed = true;
    signals.push(signal);
    if (signal === "SIGKILL") child.emit("close", null, signal);
  };
  const client = new OmarMcpClient({ spawnProcess: () => child, closeGraceMs: 50 });
  await client.start();
  const closed = client.close();
  assert.equal(client.close(), closed);
  assert.equal(client.child, child);
  assert.deepEqual(signals, ["SIGTERM"]);
  t.mock.timers.tick(49);
  assert.equal(client.child, child);
  t.mock.timers.tick(1);
  await closed;
  assert.deepEqual(signals, ["SIGTERM", "SIGKILL"]);
  assert.equal(client.child, undefined);
  assert.equal(client.closePromise, undefined);
  t.mock.timers.tick(1000);
  await client.close();
  assert.deepEqual(signals, ["SIGTERM", "SIGKILL"]);
});

test("graceful close cancels escalation and permits a clean restart", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const signals = [];
  const children = [];
  const client = new OmarMcpClient({ spawnProcess: () => {
    const child = new FakeChild();
    const kill = child.kill.bind(child);
    child.kill = (signal) => { signals.push(signal); kill(signal); };
    children.push(child);
    return child;
  }, closeGraceMs: 50 });
  await client.start();
  await client.close();
  await client.start();
  t.mock.timers.tick(100);
  assert.deepEqual(signals, ["SIGTERM"]);
  assert.equal(client.child, children[1]);
  assert.equal((await client.listTools())[0].name, "list_agents");
  await client.close();
});

test("restart waits for the previous child to be reaped", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const children = [];
  const client = new OmarMcpClient({ closeGraceMs: 50, spawnProcess: () => {
    const child = new FakeChild();
    child.kill = (signal) => {
      if (signal === "SIGKILL") child.emit("close", null, signal);
    };
    children.push(child);
    return child;
  } });
  await client.start();
  const closing = client.close();
  const restarting = client.start();
  assert.equal(children.length, 1);
  t.mock.timers.tick(50);
  await closing;
  await restarting;
  assert.equal(children.length, 2);
  assert.equal(client.child, children[1]);
  const closed = client.close();
  t.mock.timers.tick(50);
  await closed;
});

test("failed initialization closes its child and allows retry", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const children = [];
  const client = new OmarMcpClient({ timeoutMs: 10, spawnProcess: () => {
    const child = new FakeChild();
    if (children.length === 0) child.stdin.write = () => {};
    children.push(child);
    return child;
  } });
  const failed = assert.rejects(client.start(), /initialize timed out/);
  t.mock.timers.tick(10);
  await failed;
  assert.equal(children[0].killed, true);
  assert.equal(client.pending.size, 0);
  assert.equal(client.child, undefined);
  assert.equal(client.closePromise, undefined);
  await client.start();
  assert.equal(client.child, children[1]);
  await client.close();
});

test("close rejects pending requests and removes abort listeners", async () => {
  const client = new OmarMcpClient({ spawnProcess: () => new FakeChild() });
  await client.start();
  const controller = new AbortController();
  const pending = client.request("never-respond", {}, controller.signal);
  const rejected = assert.rejects(pending, /client closed/);
  await client.close();
  await rejected;
  assert.equal(client.pending.size, 0);
  assert.equal(getEventListeners(controller.signal, "abort").length, 0);
});

test("request timeout removes abort listeners", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const client = new OmarMcpClient({ spawnProcess: () => new FakeChild(), timeoutMs: 10 });
  await client.start();
  const controller = new AbortController();
  const rejected = assert.rejects(client.request("never-respond", {}, controller.signal), /timed out/);
  t.mock.timers.tick(10);
  await rejected;
  assert.equal(getEventListeners(controller.signal, "abort").length, 0);
  await client.close();
});

test("reaps an actual child that ignores SIGTERM", { timeout: 5000 }, async (t) => {
  const client = new OmarMcpClient({
    binary: process.execPath,
    args: [fakeOmarPath],
    env: { FAKE_OMAR_IGNORE_SIGTERM: "1" },
    closeGraceMs: 30,
  });
  t.after(() => client.close());
  await client.start();
  const child = client.child;
  await client.close();
  assert.equal(child.signalCode, "SIGKILL");
  assert.throws(() => process.kill(child.pid, 0), { code: "ESRCH" });
});

test("is runnable by Pi 0.85.1 in RPC mode", async (t) => {
  const fakeOmar = fakeOmarPath;
  await chmod(fakeOmar, 0o755);
  const pi = spawn(
    process.env.PI_BINARY || "npx",
    process.env.PI_BINARY
      ? ["--mode", "rpc", "--no-session", "--offline", "-e", extensionPath]
      : ["--yes", "--package=@earendil-works/pi-coding-agent@0.85.1", "--", "pi", "--mode", "rpc", "--no-session", "--offline", "-e", extensionPath],
    {
      env: { ...process.env, OMAR_BINARY: fakeOmar },
      stdio: ["pipe", "pipe", "pipe"],
      detached: process.platform !== "win32",
    },
  );
  t.after(async () => {
    const closed = once(pi, "close");
    // npx can add a wrapper process; terminate its whole test process group.
    if (process.platform !== "win32") {
      try { process.kill(-pi.pid, "SIGKILL"); } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    } else pi.kill("SIGKILL");
    if (pi.exitCode === null && pi.signalCode === null) await closed;
  });
  let output = "";
  let stderr = "";
  pi.stderr.on("data", (chunk) => { stderr += chunk; });
  pi.stdout.setEncoding("utf8");
  await new Promise((resolvePromise, reject) => {
    const finish = (error) => {
        clearTimeout(timeout);
        pi.stdout.off("data", onData);
        pi.off("error", finish);
        pi.off("exit", onExit);
        if (error) reject(error);
        else resolvePromise();
    };
    const onData = (chunk) => {
      output += chunk;
      if (output.includes('"command":"get_state"')) finish();
    };
    const onExit = () => finish(new Error(`Pi RPC exited: ${output}\n${stderr}`));
    const timeout = setTimeout(() => finish(new Error(`Pi RPC startup timed out: ${output}\n${stderr}`)), 30_000);
    pi.stdout.on("data", onData);
    pi.once("error", finish);
    pi.once("exit", onExit);
    pi.stdin.write('{"id":"state","type":"get_state"}\n');
  });
  assert.match(output, /"success":true/);
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
  process.env.OMAR_BINARY = fakeOmarPath;
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
