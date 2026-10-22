import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { loadConfig, RuntimeError, WorkflowGateway } from "./workflow.mjs";

// Deliberately not a Slack implementation: no mentions, threads, escaping, or
// platform networking. Both mock transports consume the same core contract.
class MockAdapter {
  constructor(platform) { this.platform = platform; this.messages = []; }
  usageHint(workflow) { return `${this.platform}: run ${workflow}`; }
  async sendMessage(destination, reply) {
    assert.equal(destination.platform, this.platform);
    this.messages.push({ destination, reply });
  }
}
function setup(platform = "mock-chat", accountId = "account-1", options = {}) {
  const config = { platform, accountId, allowedUsers: ["user-1"], allowedConversations: ["room-1"],
    command: "research", outputPort: "final.report", timeoutSeconds: 60,
    request: { program: "fixed operator-approved workflow", raw_inputs: ["request=fixed topic"] } };
  const starts = [];
  const adapter = new MockAdapter(platform);
  const runtime = {
    async start(request) { starts.push(request); return { run_id: "run-1", status: "running" }; },
    async wait() { return { run_id: "run-1", status: "completed", outputs: { "final.report": "selected result", private: "other output" } }; },
    ...options.runtime,
  };
  const gateway = new WorkflowGateway(config, { runtime, messenger: adapter, log: () => {} });
  const message = { platform, accountId, senderId: "user-1", eventId: "event-1", command: "run research",
    destination: { platform, accountId, conversationId: "room-1" } };
  return { config, starts, adapter, gateway, message };
}

for (const platform of ["mock-chat", "mock-other"]) {
  test(`${platform}: same execution contract supports threadless replies and suppresses redelivery`, async () => {
    const h = setup(platform);
    await h.gateway.handle(h.message);
    await h.gateway.handle(h.message);
    assert.deepEqual(h.starts, [h.config.request]);
    assert.equal(h.adapter.messages.length, 2);
    assert.deepEqual(h.adapter.messages[1], { destination: h.message.destination,
      reply: { text: "OMAR run run-1: completed.\nselected result", runId: "run-1" } });
    assert.equal(h.adapter.messages[1].reply.text.includes("other output"), false);
  });

  test(`${platform}: optional thread identity is preserved without interpretation`, async () => {
    const h = setup(platform);
    h.message.destination.threadId = "provider-specific-parent";
    await h.gateway.handle(h.message);
    assert.equal(h.adapter.messages[1].destination.threadId, "provider-specific-parent");
  });

  test(`${platform}: unauthorized or malformed events cannot execute, reply, or poison dedup`, async () => {
    const h = setup(platform);
    const invalid = [null, {}, { ...h.message, platform: "wrong-platform" },
      { ...h.message, accountId: "wrong-account" }, { ...h.message, senderId: "other-user" },
      { ...h.message, eventId: "" }, { ...h.message, command: null },
      { ...h.message, destination: undefined },
      { ...h.message, destination: { ...h.message.destination, platform: "wrong-platform" } },
      { ...h.message, destination: { ...h.message.destination, accountId: "wrong-account" } },
      { ...h.message, destination: { ...h.message.destination, conversationId: "other-room" } },
      { ...h.message, destination: { ...h.message.destination, threadId: {} } }];
    for (const message of invalid) await h.gateway.handle(message);
    assert.equal(h.starts.length, 0);
    assert.equal(h.adapter.messages.length, 0);
    await h.gateway.handle(h.message);
    assert.equal(h.starts.length, 1);
  });

  test(`${platform}: the adapter controls command syntax and receives untruncated output`, async () => {
    const longReport = "<provider-text>".repeat(1000);
    const h = setup(platform, "account-1", { runtime: {
      async wait() { return { run_id: "run-1", status: "completed", outputs: { "final.report": longReport } }; },
    } });
    await h.gateway.handle({ ...h.message, command: "help" });
    assert.equal(h.adapter.messages[0].reply.text, `${platform}: run research`);
    await h.gateway.handle({ ...h.message, eventId: "event-2" });
    assert.equal(h.adapter.messages.at(-1).reply.text, `OMAR run run-1: completed.\n${longReport}`);
  });
}

test("identical provider IDs in separately bound platform/accounts do not share authorization or dedup", async () => {
  const connections = [setup("mock-chat"), setup("mock-other"), setup("mock-chat", "account-2")];
  for (const h of connections) {
    await h.gateway.handle(h.message);
    await h.gateway.handle(h.message);
    assert.equal(h.starts.length, 1);
    assert.equal(h.adapter.messages.length, 2);
  }
});

test("delivery rejection does not repeat execution and carries run metadata to the adapter", async () => {
  const h = setup();
  const attempts = [];
  h.adapter.sendMessage = async (destination, reply) => { attempts.push(reply); throw new Error("provider failure"); };
  await h.gateway.handle(h.message);
  await h.gateway.handle(h.message);
  assert.equal(h.starts.length, 1);
  assert.equal(attempts.length, 2);
  assert.equal(attempts[1].runId, "run-1");
});

test("ambiguous starts remain blocked across distinct commands on the mock adapter", async () => {
  const h = setup("mock-chat", "account-1", { runtime: {
    async start() { h.starts.push({}); throw new RuntimeError(500, "possibly-started"); },
  } });
  await h.gateway.handle(h.message);
  await h.gateway.handle({ ...h.message, eventId: "event-2" });
  assert.equal(h.starts.length, 1);
  assert.match(h.adapter.messages.at(-1).reply.text, /operator checking/);
});

test("shared workflow configuration uses neutral names and never rewrites the source config", async () => {
  const folder = await mkdtemp(join(tmpdir(), "omar-messaging-config-"));
  try {
    await writeFile(join(folder, "workflow.omar"), "fixed source");
    const path = join(folder, "config.json");
    const config = { ...setup().config, runtimeUrl: "http://127.0.0.1:123/chats/fixed-chat", programPath: "workflow.omar" };
    const original = JSON.stringify(config);
    await writeFile(path, original);
    const loaded = await loadConfig(path);
    assert.equal(loaded.platform, "mock-chat");
    assert.equal(loaded.accountId, "account-1");
    assert.deepEqual(loaded.allowedConversations, ["room-1"]);
    assert.equal(loaded.request.program, "fixed source");
    assert.equal(await readFile(path, "utf8"), original);
    for (const change of [{ platform: "" }, { accountId: "" }, { allowedConversations: [] }]) {
      await writeFile(path, JSON.stringify({ ...config, ...change }));
      await assert.rejects(loadConfig(path));
    }
  } finally { await rm(folder, { recursive: true, force: true }); }
});
