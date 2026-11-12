import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { restartRows, restartText } from "../app/lib/attempts.ts";

async function render() {
  const workerUrl = new URL("../dist/server/index.js", import.meta.url);
  workerUrl.searchParams.set("test", `${process.pid}-${Date.now()}`);
  const { default: worker } = await import(workerUrl.href);

  return worker.fetch(
    new Request("http://localhost/", {
      headers: { accept: "text/html" },
    }),
    {
      ASSETS: {
        fetch: async () => new Response("Not found", { status: 404 }),
      },
    },
    {
      waitUntil() {},
      passThroughOnException() {},
    },
  );
}

test("renders the OMAR Mission Control application shell", async () => {
  const response = await render();
  assert.equal(response.status, 200);
  assert.match(response.headers.get("content-type") ?? "", /^text\/html\b/i);

  const html = await response.text();
  assert.match(html, /<title>OMAR Mission Control<\/title>/i);
  assert.doesNotMatch(html, /WORKFLOW BUILDER/);
  assert.doesNotMatch(html, /class="topbar"/);
  assert.match(html, /alt="Omar"/);
  assert.match(html, /WORKFLOW TOPOLOGY/);
  assert.match(html, /ReviewFlow\.omar/);
  // Mode is a launch flag, so with OMAR_SERVE_URL unset the shell renders the
  // offline demo topology rather than any connect affordance.
  assert.match(html, /demo topology/);
  assert.doesNotMatch(html, /aria-label="omar serve URL"/);
  assert.doesNotMatch(html, /site is taking shape|loading skeleton/i);
});

test("keeps the runtime protocol explicit and versioned", async () => {
  const [protocol, client, diagram] = await Promise.all([
    readFile(new URL("../app/lib/protocol.ts", import.meta.url), "utf8"),
    readFile(new URL("../app/lib/runtime-client.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../app/diagram/diagram-canvas.tsx", import.meta.url),
      "utf8",
    ),
  ]);

  assert.match(protocol, /DIAGRAM_PROTOCOL_VERSION = 1/);
  assert.match(protocol, /reaction_started/);
  assert.match(client, /\/v1\/diagram/);
  assert.match(client, /\/v1\/events/);
  assert.match(client, /new EventSource/);
  // Run admission is a separate versioned surface from the diagram protocol.
  assert.match(protocol, /SERVE_PROTOCOL_VERSION = 1/);
  assert.match(client, /\/v1\/runs/);
  assert.match(client, /export async function startRun/);
  assert.match(diagram, /elkjs\/lib\/elk\.bundled\.js/);
  assert.match(diagram, /"elk\.algorithm": "layered"/);
});

test("renders every team as a container holding its own ports and reactions", async () => {
  const diagram = await readFile(
    new URL("../app/diagram/diagram-canvas.tsx", import.meta.url),
    "utf8",
  );

  // One container per instantiation, each owning its own boundary ports, with
  // everything else laid out inside it — never as siblings of the box. A team
  // can instantiate a team, so a container's children include containers.
  assert.match(diagram, /function containerNode\(container: ContainerView\)/);
  assert.match(diagram, /\.\.\.childrenOf\(container\.id\)/);
  assert.match(diagram, /ports: ports\.map/);
  assert.match(diagram, /children: \[/);
  assert.match(diagram, /\.\.\.actions\.map/);
  assert.match(diagram, /\.\.\.timers\.map/);
  assert.match(diagram, /\.\.\.reactions\.map/);
  assert.match(diagram, /"elk\.portConstraints": "FIXED_SIDE"/);
  assert.match(diagram, /"elk\.port\.side": port\.kind === "input" \? "WEST" : "EAST"/);
  // Ports are snapped onto their own container's boundary, not a shared one.
  assert.match(diagram, /x: port\.kind === "input" \? owner\.x : owner\.x \+ owner\.width/);
  // Grouping is read from the runtime, never inferred by splitting a name.
  assert.match(diagram, /snapshot\.instances/);
});

test("uses the LF-inspired visual grammar without KIELER", async () => {
  const [diagram, styles, manifest] = await Promise.all([
    readFile(
      new URL("../app/diagram/diagram-canvas.tsx", import.meta.url),
      "utf8",
    ),
    readFile(new URL("../app/globals.css", import.meta.url), "utf8"),
    readFile(new URL("../package.json", import.meta.url), "utf8"),
  ]);

  // Real SVG geometry, not rectangles clipped into shape by CSS.
  assert.match(diagram, /<rect className="omar-reaction-body"/);
  assert.match(diagram, /function portTriangle/);
  assert.match(diagram, /function diamondPoints/);
  assert.match(diagram, /"elk\.edgeRouting": "ORTHOGONAL"/);
  // Direction is carried by the port triangles. An arrowhead on the line as
  // well says the same thing twice, so edges deliberately have none.
  assert.doesNotMatch(diagram, /markerEnd=/);
  assert.doesNotMatch(diagram, /<marker\b/);
  assert.doesNotMatch(styles, /clip-path: polygon/);

  assert.match(styles, /\.omar-team-body/);
  assert.match(styles, /\.omar-port\b/);
  assert.match(styles, /\.omar-action\b/);
  assert.match(styles, /\.omar-reaction-body/);
  assert.match(styles, /\.omar-edge\b/);

  // The LF look is reproduced with the web stack; KIELER/KLighD is never pulled in.
  assert.doesNotMatch(manifest, /kieler|klighd/i);
  assert.doesNotMatch(diagram, /(?:import|require)\s*\(?[^\n)]*(?:kieler|klighd)/i);
});


test("restarted tasks have readable attempt rows in order and concise badge text", () => {
  const record = (id, task, retry, outcome, extra = {}) => ({
    attempt_id: id, native_session_id: "01a1cdfa-full-native-session", retry, outcome,
    invocations: [{ reaction: "b.reaction.0", invocation_id: task }], ...extra,
  });
  const rows = restartRows([
    record("older-task", 7, 0, "completed"),
    record("dead-attempt", 8, 0, "died", { death_reason: "worker process exited", started_at: "2026-11-11T12:30:00Z" }),
    record("retry-attempt", 8, 1, "completed", { conversation_mode: "resumed native conversation" }),
  ], "b.reaction.0");
  assert.deepEqual(rows.map((row) => row.summary), [
    "Attempt 1 · died — worker process exited",
    "Attempt 2 · retry 1 of 2 · resumed its conversation · completed",
  ]);
  assert.equal(rows[0].time, "2026-11-11T12:30:00.000Z");
  assert.equal(rows[1].time, null);
  assert.equal(rows[0].session.slice(0, 8), "01a1cdfa");
  assert.ok(rows.every((row) => !row.summary.includes(row.id) && !row.summary.includes(row.session)));
  assert.deepEqual(restartText(1), { headline: "1 restart", badge: "restarted ×1" });
  assert.deepEqual(restartText(2), { headline: "2 restarts", badge: "restarted ×2" });
  const fresh = restartRows([record("fresh", 9, 2, "open", { lifetime: { count: 1, unit: "task" } })], "b.reaction.0");
  assert.match(fresh[0].summary, /retry 2 of 2 · new conversation · in progress/);
  assert.deepEqual(restartRows([], "b.reaction.0"), []);
  const reminded = restartRows([record("reminded", 10, 0, "completed", {
    reminders: [{ task_id: 10, reaction: "b.reaction.0", reason: "asked the user a question" }],
  })], "b.reaction.0");
  assert.equal(reminded[0].summary, "Attempt 1 · completed · reminded to finish");
  const later = record("shared-window", 11, 0, "open", {
    reminders: [{ task_id: 10, reaction: "b.reaction.0" }],
  });
  later.invocations.unshift({ reaction: "b.reaction.0", invocation_id: 10 });
  assert.equal(restartRows([later], "b.reaction.0")[0].reminded, false);
});

test("the inspector and diagram render the restart rows and badge with id tooltips", async () => {
  const [inspector, diagram] = await Promise.all([
    readFile(new URL("../app/step-inspector.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/diagram/diagram-canvas.tsx", import.meta.url), "utf8"),
  ]);
  assert.match(inspector, /restartText\(reaction.restarts\).headline/);
  assert.match(inspector, /restartRows\(attempts, reaction.name\)/);
  assert.match(inspector, /title=\{`Attempt \$\{attempt.id\}`\}/);
  assert.match(inspector, /title=\{attempt.session\}.*attempt.session.slice\(0, 8\)/);
  assert.match(diagram, /className="omar-restart-badge"/);
  assert.match(diagram, /restartText\(reaction.restarts\).badge/);
});
