import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { startDelivery } from "../delivery.js";
import extension from "../index.js";

async function request(path, message) {
  return new Promise((resolve, reject) => {
    const socket = net.connect(path);
    let reply = "";
    socket.on("error", reject);
    socket.on("connect", () => socket.write(JSON.stringify(message) + "\n"));
    socket.on("data", (data) => { reply += data; });
    socket.on("end", () => resolve(JSON.parse(reply)));
  });
}

test("private delivery wakes a turn with a custom message and exposes the native session", async () => {
  const dir = await mkdtemp("/tmp/omar-pi-");
  const path = `${dir}/in.sock`;
  const sent = [];
  const stop = await startDelivery(path, { sendMessage: (...args) => sent.push(args) },
    { sessionManager: { getSessionFile: () => "/tmp/saved.jsonl" } });
  try {
    assert.equal((await stat(path)).mode & 0o777, 0o600);
    assert.deepEqual(await request(path, { text: "event\nsecond line" }), { accepted: true });
    assert.deepEqual(sent, [[{ customType: "omar_event", content: "event\nsecond line", display: true },
      { triggerTurn: true, deliverAs: "followUp" }]]);
    assert.deepEqual(await request(path, { session: true }), { session: "/tmp/saved.jsonl" });
    assert.ok((await request(path, { text: null })).error);
    assert.equal(sent.length, 1);
  } finally { await stop(); await rm(dir, { recursive: true, force: true }); }
  await assert.rejects(request(path, { text: "late" }));
});

test("failed tool discovery never publishes delivery; retry and shutdown manage the socket", async () => {
  const dir = await mkdtemp("/tmp/omar-pi-");
  const path = `${dir}/in.sock`;
  const original = process.env.OMAR_PI_SOCKET;
  process.env.OMAR_PI_SOCKET = path;
  const handlers = {};
  let attempts = 0;
  let closed = 0;
  extension({ on: (name, fn) => { handlers[name] = fn; }, registerTool() {}, sendMessage() {} },
    { createClient: () => ({ listTools: async () => ++attempts === 1 ? [] : [{ name: "test" }],
      close: async () => { closed++; } }) });
  try {
    await handlers.session_start({}, {});
    await assert.rejects(stat(path));
    await handlers.session_start({}, {});
    assert.deepEqual(await request(path, { text: "ready" }), { accepted: true });
    await handlers.session_shutdown();
    await assert.rejects(stat(path));
    assert.equal(closed, 2);
  } finally {
    if (original === undefined) delete process.env.OMAR_PI_SOCKET;
    else process.env.OMAR_PI_SOCKET = original;
    await rm(dir, { recursive: true, force: true });
  }
});
