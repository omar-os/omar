/**
 * A view port: same pages, no writes. The client learns which it is talking to
 * from `/health`, and reaches a run's diagram through the port's relay.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { startFakeServe } from "./fake-serve.mjs";
import { checkServeHealth, diagramUrlFor, fetchDiagram } from "../app/lib/runtime-client.ts";

const record = {
  run_id: "run/1",
  team: "ReviewFlow",
  status: "running",
  diagram_address: "127.0.0.1:4100",
  started_at: 0,
  finished_at: null,
  error: null,
};

async function healthWith(body) {
  const original = globalThis.fetch;
  globalThis.fetch = async () => Response.json(body);
  try {
    return await checkServeHealth("http://127.0.0.1:1");
  } finally {
    globalThis.fetch = original;
  }
}

test("health without an access field is full access", async () => {
  assert.deepEqual(await healthWith({ status: "ok", protocol_version: 1 }), { ok: true, access: "full" });
  assert.deepEqual(await healthWith({ status: "ok", protocol_version: 1, access: "full" }), { ok: true, access: "full" });
  assert.deepEqual(await healthWith({ status: "ok", protocol_version: 1, access: "view" }), { ok: true, access: "view" });
});

test("a run's diagram is on loopback, or relayed by a view port", () => {
  assert.equal(diagramUrlFor(record), "http://127.0.0.1:4100");
  assert.equal(
    diagramUrlFor(record, "http://viewer.example:8080/chats/abc/"),
    "http://viewer.example:8080/chats/abc/v1/runs/run%2F1/diagram",
  );
  // The relay forwards to the diagram server, so there has to be one.
  assert.throws(() => diagramUrlFor({ ...record, diagram_address: null }, "http://viewer.example"));
});

test("the fake's view port refuses writes and relays the diagram", async () => {
  const fake = await startFakeServe({ stepMs: 20, viewPort: 0 });
  try {
    assert.deepEqual(await checkServeHealth(fake.url), { ok: true, access: "full" });
    assert.deepEqual(await checkServeHealth(fake.viewUrl), { ok: true, access: "view" });
    const admitted = await fetch(`${fake.url}/v1/runs`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ program: "main ReviewFlow {}", inputs: {} }),
    });
    const run = await admitted.json();
    for (const [path, init] of [
      ["/v1/runs", { method: "POST", body: "{}" }],
      ["/v1/chats", { method: "POST", body: "{}" }],
      [`/v1/runs/${run.run_id}/panel`, {}],
      ["/v1/workspaces/anything", {}],
    ]) {
      const response = await fetch(`${fake.viewUrl}${path}`, init);
      assert.equal(response.status, 403, path);
      assert.deepEqual(await response.json(), { error: "read-only view" });
    }
    const relayed = await fetchDiagram(diagramUrlFor(run, fake.viewUrl));
    assert.equal(relayed.team, run.team);
  } finally {
    await fake.close();
  }
});
