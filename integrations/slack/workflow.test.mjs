import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { loadConfig, OmarRuntime, RuntimeError, WorkflowGateway } from "./workflow.mjs";
import { handleEnvelope, normalizeMention, SlackApi, socketSession, formatSlackReply } from "./slack.mjs";

const config = { platform: "slack", accountId: "T1", allowedUsers: ["U1"], allowedConversations: ["C1"],
  command: "demo", outputPort: "run.result", timeoutSeconds: 1,
  request: { program: "fixed source", inputs: { "run.request": "fixed input" } } };
function payload(overrides = {}, event = {}) {
  return { type: "event_callback", team_id: "T1", event_id: "Ev1", ...overrides,
    event: { type: "app_mention", user: "U1", channel: "C1", ts: "1.1",
      text: "<@BOT> run demo", ...event } };
}
function harness({ start, wait, sendMessage, ...options } = {}) {
  const starts = [], replies = [], logs = [];
  const runtime = {
    async start(request) { starts.push(request); return start ? start(request) : { run_id: "R1", status: "starting" }; },
    async wait(record) { return wait ? wait(record) : { run_id: "R1", status: "completed", outputs: { "run.result": "report" } }; },
  };
  const messenger = { usageHint: (command) => `Use @Omar run ${command}`,
    async sendMessage(destination, reply) { const text = formatSlackReply(reply); replies.push({ destination, text }); if (sendMessage) await sendMessage(destination, text); } };
  return { starts, replies, logs, gateway: new WorkflowGateway(config,
    { runtime, messenger, log: (value) => logs.push(JSON.parse(value)), ...options }) };
}
const incoming = (overrides, event) => normalizeMention(payload(overrides, event), "BOT");

test("fixed workflow runs once and replies in the originating thread", async () => {
  const h = harness();
  await h.gateway.handle(incoming({}, { thread_ts: "parent" }));
  assert.deepEqual(h.starts, [config.request]);
  assert.equal(h.replies.length, 2);
  assert.deepEqual(h.replies[1].destination, { platform: "slack", accountId: "T1", conversationId: "C1", threadId: "parent" });
  assert.match(h.replies[1].text, /completed.*\nreport/);
});

test("unauthorized workspace, sender, and channel cannot execute or receive results", async () => {
  const h = harness();
  for (const message of [incoming({ team_id: "T2" }), incoming({}, { user: "U2" }), incoming({}, { channel: "C2" })]) {
    await h.gateway.handle(message);
  }
  assert.equal(h.starts.length, 0);
  assert.equal(h.replies.length, 0);
});

test("only the explicit bot mention command is accepted; chat cannot supply inputs", async () => {
  const h = harness();
  for (const [index, text] of ["<@BOT> run other", "<@BOT> run demo --input x=malicious", "<@BOT> hello"].entries()) {
    await h.gateway.handle(incoming({ event_id: `Ev${index}` }, { text }));
  }
  assert.equal(h.starts.length, 0);
  assert.equal(h.replies.length, 3);
  for (const event of [{ type: "message" }, { bot_id: "B2" }, { user: "BOT" },
    { subtype: "message_changed" }, { text: "please <@BOT> run demo" }]) {
    assert.equal(incoming({}, event), null);
  }
  assert.equal(normalizeMention(payload({ event_id: undefined }), "BOT"), null);
});

test("redeliveries do not execute twice; distinct requests receive busy while one is pending", async () => {
  let release;
  const held = new Promise((done) => { release = done; });
  const h = harness({ start: async () => { await held; return { run_id: "R1" }; } });
  const first = h.gateway.handle(incoming());
  await h.gateway.handle(incoming());
  await h.gateway.handle(incoming({ event_id: "Ev2" }));
  assert.equal(h.starts.length, 1);
  assert.match(h.replies[0].text, /busy/);
  release(); await first;
  await h.gateway.handle(incoming());
  assert.equal(h.starts.length, 1);
});

test("dedup cache is bounded and expires without evicting unexpired identities", async () => {
  let now = 0;
  const h = harness({ now: () => now, dedupLimit: 1, dedupMs: 10 });
  await h.gateway.handle(incoming());
  await h.gateway.handle(incoming({ event_id: "Ev2" }));
  assert.equal(h.starts.length, 1);
  assert.equal(h.gateway.seen.size, 1);
  assert.equal(h.logs[0].kind, "dedup_capacity");
  now = 11;
  await h.gateway.handle(incoming({ event_id: "Ev2" }));
  assert.equal(h.starts.length, 2);
});

test("failed Slack delivery never retries execution and logs only routing metadata", async () => {
  const h = harness({ sendMessage: async () => { throw new Error("secret token"); } });
  await h.gateway.handle(incoming());
  await h.gateway.handle(incoming());
  assert.equal(h.starts.length, 1);
  assert.equal(h.logs.length, 2);
  assert.equal(h.logs[1].runId, "R1");
  assert.equal(JSON.stringify(h.logs).includes("secret token"), false);
});

test("ambiguous start and monitoring failures block new starts pending operator checking", async () => {
  for (const options of [
    { start: async () => { throw new Error("timeout"); } },
    { start: async () => { throw new RuntimeError(500, "R1"); } },
    { wait: async () => { throw new Error("runtime restarted"); } },
  ]) {
    const h = harness(options);
    await h.gateway.handle(incoming());
    await h.gateway.handle(incoming({ event_id: "Ev2" }));
    assert.equal(h.starts.length, 1);
    assert.equal(h.gateway.uncertain, true);
    assert.match(h.replies.at(-1).text, /operator checking/);
  }
});

test("busy, rejected, failed, paused, and stopped runs never send a success report", async () => {
  const busy = harness({ start: async () => { throw new RuntimeError(409, "existing-run"); } });
  await busy.gateway.handle(incoming());
  assert.match(busy.replies[0].text, /already active or paused/);
  // A 409 identifies someone else's held run, not an accepted invocation.
  assert.equal(busy.gateway.uncertain, false);
  for (const status of ["failed", "paused", "stopped"]) {
    const h = harness({ wait: async () => ({ run_id: "R1", status, error: "private host path" }) });
    await h.gateway.handle(incoming());
    assert.match(h.replies.at(-1).text, new RegExp(status));
    assert.equal(h.replies.at(-1).text.includes("private host path"), false);
  }
});

test("missing and long output is handled without sending every output port", async () => {
  for (const outputs of [{ secret: "private" }, { "run.result": "x".repeat(10_000), secret: "private" }]) {
    const h = harness({ wait: async () => ({ run_id: "R1", status: "completed", outputs }) });
    await h.gateway.handle(incoming());
    const reply = h.replies.at(-1).text;
    assert.equal(reply.includes("private"), false);
    assert.ok(reply.length < 3500);
    assert.match(reply, /missing|Truncated/);
  }
});

test("Socket Mode acknowledges promptly while workflow execution is pending", async () => {
  const sent = [];
  let release;
  const pending = new Promise((done) => { release = done; });
  const socket = { send: (text) => sent.push(JSON.parse(text)) };
  const gateway = { handle: async () => pending };
  const work = handleEnvelope(JSON.stringify({ envelope_id: "env", type: "events_api", payload: payload() }), socket, "BOT", gateway);
  assert.deepEqual(sent, [{ envelope_id: "env" }]);
  release(); await work;
});

test("socket refresh opens a new connection without resetting gateway deduplication", async () => {
  const replies = [];
  const h = harness();
  class FakeSocket {
    constructor() {
      this.events = new EventEmitter();
      queueMicrotask(() => {
        this.events.emit("open");
        this.events.emit("message", { data: JSON.stringify({ envelope_id: "env", type: "events_api", payload: payload() }) });
        this.events.emit("message", { data: JSON.stringify({ type: "disconnect" }) });
      });
    }
    addEventListener(name, fn) { this.events.on(name, fn); }
    send(text) { replies.push(JSON.parse(text)); }
    close() { this.events.emit("close"); }
  }
  const api = { call: async () => ({ url: "wss://example.test/socket" }) };
  for (let i = 0; i < 2; i++) await socketSession(api, "BOT", h.gateway,
    { WebSocketImpl: FakeSocket, log: () => {} });
  assert.equal(replies.length, 2);
  assert.equal(h.starts.length, 1);
});

test("Slack sends to the fixed thread, escapes mentions, and never retries API errors", async () => {
  const calls = [];
  const api = new SlackApi("bot-secret", "app-secret", { fetchImpl: async (url, options) => {
    calls.push({ url, options }); return { ok: true, json: async () => ({ ok: true }) };
  } });
  await api.sendMessage({ accountId: "T1", conversationId: "C1", threadId: "1.1" }, { text: "<@U1> & <!channel>" });
  const body = JSON.parse(calls[0].options.body);
  assert.equal(body.channel, "C1"); assert.equal(body.thread_ts, "1.1");
  assert.equal(body.text, "&lt;@U1&gt; &amp; &lt;!channel&gt;");
  assert.equal(body.mrkdwn, false);
  const failed = new SlackApi("secret", "app", { fetchImpl: async () => ({ ok: false, json: async () => ({ error: "private response" }) }) });
  await assert.rejects(failed.sendMessage({}, { text: "text" }), /^Error: Slack API request failed$/);
});

test("runtime start uses fixed JSON and refuses redirects without automatic retry", async () => {
  let count = 0;
  const runtime = new OmarRuntime("http://127.0.0.1:123/chats/id", { fetchImpl: async (url, options) => {
    count++;
    assert.match(url, /\/chats\/id\/v1\/runs$/);
    assert.equal(options.redirect, "error");
    assert.deepEqual(JSON.parse(options.body), config.request);
    return { ok: false, status: 500, json: async () => ({ run_id: "R1" }) };
  } });
  await assert.rejects(runtime.start(config.request), (error) => error.runId === "R1");
  assert.equal(count, 1);
});

test("configuration loads the approved program and rejects unscoped/remote runtime URLs", async () => {
  const folder = await mkdtemp(join(tmpdir(), "omar-slack-config-"));
  try {
    await writeFile(join(folder, "demo.omar"), "fixed source");
    await writeFile(join(folder, "decision.json"), '{"type":"string","enum":["ok"]}');
    const file = join(folder, "config.json");
    const base = { ...config, workspaceId: "T1", allowedChannels: ["C1"], runtimeUrl: "http://127.0.0.1:123/chats/id", programPath: "demo.omar", files: { "schemas/decision.json": "decision.json" } };
    await writeFile(file, JSON.stringify(base));
    const loaded = await loadConfig(file);
    assert.equal(loaded.platform, "slack");
    assert.equal(loaded.accountId, "T1");
    assert.deepEqual(loaded.allowedConversations, ["C1"]);
    assert.equal(loaded.request.program, "fixed source");
    assert.equal(loaded.request.replace, false);
    assert.equal(loaded.request.files["schemas/decision.json"], '{"type":"string","enum":["ok"]}');
    for (const change of [{ runtimeUrl: "https://external.example/chats/id" }, { runtimeUrl: "http://127.0.0.1:123" },
      { allowedUsers: [] }, { allowedChannels: [] }, { timeoutSeconds: -1 }]) {
      await writeFile(file, JSON.stringify({ ...base, ...change }));
      await assert.rejects(loadConfig(file));
    }
  } finally { await rm(folder, { recursive: true, force: true }); }
});

test("research launcher scopes the update setting and forwards backend arguments", () => {
  const launcher = fileURLToPath(new URL("./research-bin/codex", import.meta.url));
  const result = spawnSync("sh", [launcher, "app-server", "--listen", "unix:///tmp/test.sock"], {
    env: { ...process.env, SLACK_RESEARCH_CODEX_BIN: "/bin/echo" }, encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), "-c check_for_update_on_startup=false app-server --listen unix:///tmp/test.sock");
});

test("Slack API applies escaped-text limits locally and preserves the run reference", async () => {
  let posted;
  const api = new SlackApi("fixture", "fixture", { fetchImpl: async (_url, options) => {
    posted = JSON.parse(options.body);
    return { ok: true, json: async () => ({ ok: true }) };
  } });
  await api.sendMessage({ platform: "slack", accountId: "T1", conversationId: "C1", threadId: "parent" },
    { text: "<@U1>&".repeat(1000), runId: "run-123" });
  assert.ok(posted.text.length <= 3500);
  assert.equal(posted.text.includes("<@"), false);
  assert.match(posted.text, /Truncated; full output in OMAR run run-123/);
  assert.equal(posted.thread_ts, "parent");
  assert.equal(api.usageHint("research"), "Use @Omar run research");
});
