/**
 * A stand-in for `omar serve` that speaks the same wire protocol.
 *
 * It exists so CI can drive the whole confirm-then-spawn flow without a model,
 * a Lean toolchain, or tmux. It deliberately serves the admission API *and* the
 * per-run diagram API from one port, because the only thing the client is
 * allowed to assume is that `diagram_address` is where the run can be observed.
 *
 * The golden snapshot it replays was captured from a real `omar serve` run, so
 * drift between this fake and the runtime shows up as a failing contract test.
 */

import { createServer } from "node:http";
import { WebSocketServer } from "ws";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { catalog, sourceDigest, workflowFixture, fixtureProfileFacts, fixtureBenchmarkEvidence } from "./workflow-fixture.mjs";

/** Mirrors the daemon's cap; the names reach the assistant's pane. */
const MAX_SELECTION = 64;

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, POST, OPTIONS",
  "access-control-allow-headers": "content-type",
};

/** Marker a test can put in a program to exercise the compile-failure path. */
const INVALID_MARKER = "!!invalid!!";

/** What the stand-in EA proposes. Real source, verified against omarc. */
const PROPOSED_PROGRAM = await readFile(
  new URL("./fixtures/review-flow.omar", import.meta.url),
  "utf8",
);

export async function startFakeServe({
  stepMs = 120,
  liveChatReplayDelayMs = 0,
  host = "127.0.0.1",
  port = 0,
  /** Which captured topology to replay; see tests/fixtures. */
  snapshot: snapshotFile = "diagram-snapshot.v1.json",
  /** Optional task copy over the captured execution topology. */
  taskMetadata = {},
  /** The geometry a terminal announces, as the daemon reports the agent's. */
  terminal: terminalSize = { cols: 96, rows: 28 },
  terminalReflow = false,
  workflowDesign = false,
  workflowRepositoryChecks = false,
  workflowIssues = /** @type {import("../app/lib/protocol-generated").ResearchIssue[]} */ ([]),
  workflowToolBlocked = false,
  workflowFacts = /** @type {"current" | "expired" | null} */ (null),
  workflowFactsLifetimeSeconds = 3600,
  workflowBenchmark = false,
} = {}) {
  const golden = JSON.parse(
    await readFile(new URL(`./fixtures/${snapshotFile}`, import.meta.url), "utf8"),
  );
  for (const reaction of golden.reactions) {
    const copy = taskMetadata[reaction.id];
    if (copy) Object.assign(reaction, { title: copy.title, description: copy.description });
  }

  /** @type {Map<string, {record: object, snapshot: object, subscribers: Set<import("node:http").ServerResponse>, sequence: number}>} */
  const runs = new Map();

  const server = createServer(async (request, response) => {
    const url = new URL(request.url, `http://${request.headers.host}`);
    const scoped = /^\/chats\/([^/]+)(\/.*)$/.exec(url.pathname);
    const chat = scoped ? chats.get(scoped[1]) : activeChat;
    if (!chat) return json(response, 404, { error: "unknown chat" });
    if (scoped) url.pathname = scoped[2];

    if (request.method === "OPTIONS") {
      response.writeHead(204, CORS).end();
      return;
    }
    if (url.pathname === "/v1/workflow/profiles") return json(response, 200, catalog);
    if (url.pathname === "/v1/workflow/evidence") return json(response, 200, {revision:"test-fixture",evidence:workflowBenchmark ? [...catalog.evidence,fixtureBenchmarkEvidence()] : catalog.evidence});
    if (request.method === "POST" && url.pathname.startsWith("/v1/workflow/")) {
      const body = JSON.parse(await readBody(request));
      if (url.pathname.endsWith("/import")) {
        const bundle=body.bundle;
        if(bundle.schema_version!==1 || sourceDigest(bundle.program)!==bundle.source_digest) return json(response,400,{error:"workflow bundle source digest mismatch"});
        const next=workflowFixture(bundle.program,bundle.inputs,golden,chat.id);
        next.draft=structuredClone(bundle.draft);next.draft.revises=null;next.draft.intent.message_sequences=[];next.draft.research.session_id=null;next.draft.research.usage=null;
        for(const a of next.draft.intent.assumptions){a.accepted=false;a.confirmation=null;}
        for(const q of next.draft.questions){q.answer=null;q.answer_record=null;}
        next.profiles=structuredClone(bundle.profiles);
        for(const [agent,p] of Object.entries(next.profiles)) {
          if(!catalog.profiles.some(current=>current.id===p.id && current.model===p.model && current.version===p.version)) return json(response,400,{error:"imported profile differs from destination"});
          next.draft.preferences.overrides[agent]=p.id;
          const decision=next.selections.find(s=>s.agent===agent);if(decision)decision.profile_id=p.id;
        }
        chat.plans.set(next.id,next);
        const preview=structuredClone(golden);preview.status="ready";
        publishChat("assistant","Imported workflow. Review its assumptions and model choices before running.", {plan:{id:next.id,revision:1},program:next.program,inputs:next.inputs,preview}, false, [], chat);
        return json(response,200,next);
      }
      if (url.pathname.endsWith("/receipts")) {
        const entry=chat.runs.get(body.run_id);
        if (!entry) return json(response,404,{error:"run not found"});
        return json(response,200,{schema_version:1,run_id:body.run_id,plan:entry.plan,source_digest:entry.source_digest,profiles:entry.profiles,started_at:entry.record.started_at,finished_at:entry.record.finished_at,status:entry.record.status,error:entry.record.error,verification:[],repository_checks:workflowRepositoryChecks ? [{invocation_id:"fixture-check",reaction:"flow.checker",verifier_id:"fixture-suite",verifier_digest:"a".repeat(64),image:"sha256:"+"b".repeat(64),candidate_digest:"c".repeat(64),status:"passed",test_count:15,elapsed_ms:120,artifact:"fixture"}] : [],observed_models:{},observed_efforts:{},backend_versions:{},input_tokens:null,output_tokens:null,cost_usd:null});
      }
      if (url.pathname.endsWith("/access-evidence")) {
        const entry=chat.runs.get(body.run_id);
        if(!entry || !entry.profiles?.[body.agent]) return json(response,400,{error:"agent has no saved native profile in this run"});
        return json(response,200,{run_id:body.run_id,agent:body.agent,profile_id:entry.profiles[body.agent].id,profile_digest:"fixture",connection_scope:null,backend_version:null,runtime_contract:"native-profiles-v5",status:"insufficient_evidence",observed_at:null,observed_model:null,observed_effort:null,invocation_ids:[],native_request_ids:[],artifact_digests:{},report_digest:sourceDigest("fixture"),limitations:["Synthetic fixture has no complete native model request evidence. Operator review is required."],new_model_requests:0,adapter_cost_usd:0,recorded_run_cost_usd:null});
      }
      const plan=chat.plans.get(body.id);
      if (!plan) return json(response,404,{error:"plan not found in this chat"});
      if (url.pathname.endsWith("/export")) {
        const draft=structuredClone(plan.draft);draft.revises=null;draft.intent.message_sequences=[];draft.research.session_id=null;draft.research.usage=null;
        return json(response,200,{schema_version:1,program:plan.program,source_digest:plan.source_digest,inputs:plan.inputs,draft,profiles:plan.profiles,evidence_revision:plan.evidence_revision,routing_policy_revision:plan.routing_policy_revision});
      }
      if (url.pathname.endsWith("/plan")) return json(response,200,plan);
      if (url.pathname.endsWith("/plans")) {
        if (plan.revision!==body.expected_revision) return json(response,409,{error:"revision conflict"});
        const next=structuredClone(plan);next.revision++;next.program=body.program;next.inputs=body.inputs;next.source_digest=sourceDigest(body.program);
        for(const q of next.draft.questions) if(body.answers[q.id]) {
          if(q.answer!==body.answers[q.id] || !q.answer_record) q.answer_record={id:next.id,revision:next.revision};
          q.answer=body.answers[q.id];
        }
        for(const a of next.draft.intent.assumptions) {
          a.accepted=body.accepted_assumptions.includes(a.id);
          a.confirmation=a.accepted ? (a.confirmation ?? {kind:"assumption",plan:{id:next.id,revision:next.revision},assumption_id:a.id,text_digest:sourceDigest(a.text)}) : null;
        }
        next.draft.preferences.overrides=body.overrides;
        for(const [agent,id] of Object.entries(body.overrides)) {const p=catalog.profiles.find(p=>p.id===id);if(!p)return json(response,400,{error:"unknown profile"});next.profiles[agent]=p;Object.assign(next.selections.find(s=>s.agent===agent), {profile_id:p.id,reason:`Operator selected ${p.label}. Task qualification remains provisional.`});}
        if(workflowFacts) fixtureProfileFacts(next,workflowFacts,workflowFactsLifetimeSeconds);
        chat.plans.set(next.id,next);return json(response,200,next);
      }
    }
    if (request.method === "GET" && url.pathname === "/health") {
      return json(response, 200, { status: "ok", protocol_version: 1 });
    }
    if (request.method === "POST" && url.pathname === "/v1/programs/check") {
      return readBody(request).then((body) => checkProgram(body, response));
    }
    if (request.method === "POST" && url.pathname === "/v1/runs") {
      return readBody(request).then((body) => admit(body, response, chat));
    }
    if (request.method === "GET" && url.pathname === "/v1/runs") {
      return json(response, 200, {
        runs: [...chat.runs.values()].map((entry) => entry.record),
      });
    }
    // Before the run-record route, which matches any suffix — the same order
    // the daemon's own arms are in, and for the same reason.
    if (
      request.method === "POST" &&
      url.pathname.startsWith("/v1/runs/") &&
      url.pathname.endsWith("/stop")
    ) {
      const id = url.pathname.slice("/v1/runs/".length, -"/stop".length);
      const entry = chat.runs.get(id);
      if (!entry) return json(response, 404, { error: "unknown run" });
      // A stopping run is still active, as on the daemon: it holds its sessions
      // until the tag closes.
      const active = ["starting", "running", "stopping"].includes(entry.record.status);
      if (!active) return json(response, 200, entry.record);
      // Accepted, not done: the daemon answers before the run has ended, and
      // records `stopping` so a client that reloads still sees it. Held briefly
      // so that waiting state is one a test can observe.
      entry.record.status = "stopping";
      setTimeout(() => {
        entry.record.status = "stopped";
        entry.record.finished_at = Math.floor(Date.now() / 1000);
        publish(entry, "run_completed", { status: "stopped" });
      }, 400);
      return json(response, 202, entry.record);
    }
    if (url.pathname.startsWith("/v1/runs/") && url.pathname.endsWith("/panel")) {
      const id = url.pathname.slice("/v1/runs/".length, -"/panel".length);
      const entry = chat.runs.get(id);
      if (!entry) return json(response, 404, { error: "unknown run" });
      if (request.method === "GET") {
        return json(response, 200, {
          run_id: id,
          team: entry.snapshot.team,
          pending: entry.pending ?? [],
        });
      }
      if (request.method === "POST") {
        return readBody(request).then((body) => {
          const answer = JSON.parse(body);
          const owed = (entry.pending ?? []).find(
            (item) => item.invocation_id === answer.invocation_id,
          );
          if (!owed) return json(response, 400, { error: "unknown invocation" });
          // The runtime refuses a port outside the invocation's effects, so
          // the fake has to as well or the client's contract goes untested.
          for (const port of Object.keys(answer.values)) {
            if (!(port in owed.allowed_effects)) {
              return json(response, 400, {
                error: `port '${port}' is not an effect of this invocation`,
              });
            }
          }
          entry.pending = (entry.pending ?? []).filter(
            (item) => item.invocation_id !== answer.invocation_id,
          );
          entry.answered = { ...answer };
          return json(response, 202, {
            run_id: id,
            invocation_id: answer.invocation_id,
            sent: Object.keys(answer.values),
          });
        });
      }
    }
    if (request.method === "GET" && url.pathname.startsWith("/v1/runs/")) {
      const entry = chat.runs.get(url.pathname.slice("/v1/runs/".length));
      return entry
        ? json(response, 200, entry.record)
        : json(response, 404, { error: "unknown run" });
    }
    // Diagram surface for the single active run, mirroring the per-run server.
    if (request.method === "GET" && url.pathname === "/v1/diagram") {
      const entry = latest(chat);
      return entry
        ? json(response, 200, entry.snapshot)
        : json(response, 404, { error: "no run" });
    }
    if (request.method === "GET" && url.pathname === "/v1/events") {
      return subscribe(response, latest(chat));
    }
    // Agent-only endpoints. The stand-in assistant is internal, so these exist
    // to mirror the real daemon's surface rather than to be used.
    // Which assistant is answering, and moving it to another one. The real
    // daemon relaunches the process; the fake only has to agree on the wire.
    if (request.method === "GET" && url.pathname === "/v1/agent") {
      return json(response, 200, {
        backend,
        available: ["claude", "codex", "cursor", "opencode", "agy"],
      });
    }
    if (request.method === "POST" && url.pathname === "/v1/agent/backend") {
      return readBody(request).then((body) => {
        let chosen;
        try {
          chosen = JSON.parse(body).backend;
        } catch {
          return json(response, 400, { error: "invalid request: not JSON" });
        }
        if (!["claude", "codex", "cursor", "opencode", "agy"].includes(chosen)) {
          return json(response, 400, {
            error: `Unknown backend '${chosen}'.`,
          });
        }
        backend = chosen;
        return json(response, 200, { backend });
      });
    }
    if (request.method === "POST" && url.pathname.startsWith("/v1/agent/")) {
      return readBody(request).then((body) => {
        let payload;
        try {
          payload = JSON.parse(body);
        } catch {
          return json(response, 400, { error: "invalid request: not JSON" });
        }
        if (payload.token !== agentToken) {
          return json(response, 403, { error: "forbidden" });
        }
        if (url.pathname === "/v1/agent/reply") {
          publishChat("assistant", String(payload.text ?? ""), null, payload.progress === true, [], chat);
        } else if (url.pathname === "/v1/agent/proposals") {
          const preview = structuredClone(golden);
          preview.status = "ready";
          publishChat("assistant", String(payload.summary ?? ""), {
            program: String(payload.program ?? ""),
            inputs: payload.inputs ?? {},
            preview,
          }, false, [], chat);
        } else {
          return json(response, 404, { error: "not found" });
        }
        return json(response, 202, { status: "delivered" });
      });
    }
    // Operator/EA conversation.
    if (request.method === "GET" && url.pathname === "/v1/chats") {
      return json(response, 200, { active_id: activeChat.id, conversations: [...chats.values()].map(chatSummary).sort((a, b) => b.updated_at - a.updated_at) });
    }
    if (request.method === "POST" && (url.pathname === "/v1/chats" || /^\/v1\/chats\/[^/]+\/activate$/.test(url.pathname))) {
      await readBody(request);
      const id = url.pathname === "/v1/chats" ? null : url.pathname.split("/")[3];
      const next = id ? chats.get(id) : newChat();
      if (!next) return json(response, 404, { error: "unknown chat" });
      activeChat = next;
      chats.set(next.id, next);
      return json(response, 200, chatSummary(next));
    }
    if (request.method === "GET" && url.pathname === "/v1/chat") {
      return json(response, 200, { ...chatSummary(chat), messages: chat.messages });
    }
    if (request.method === "POST" && url.pathname === "/v1/chat") {
      return readBody(request).then((body) => converse(body, response, chat));
    }
    if (request.method === "GET" && url.pathname === "/v1/chat/events") {
      return subscribeChat(response, chat);
    }
    json(response, 404, { error: "not found" });
  });

  // A stand-in for the agent's tmux session: it announces a size the way the
  // daemon does, then echoes what is typed so a test can prove keystrokes
  // travelled up the same socket the screen comes down.
  const terminals = new WebSocketServer({ noServer: true });
  server.on("upgrade", (request, socket, head) => {
    const url = new URL(request.url, "http://127.0.0.1");
    url.pathname = url.pathname.replace(/^\/chats\/[^/]+/, "");
    // The assistant has a route of its own: its tmux session is not named
    // like an agent's, so no agent name reaches it.
    const assistant = url.pathname === "/v1/agent/terminal";
    const match = assistant
      ? ["", "the assistant"]
      : /^\/v1\/agents\/(.+)\/terminal$/.exec(url.pathname);
    // CORS never reaches a WebSocket, so the daemon refuses foreign origins at
    // the handshake and the fake has to behave the same way.
    const origin = request.headers.origin;
    const local =
      !origin || /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\])(:\d+)?$/.test(origin);
    if (!match || !local) {
      socket.write(`HTTP/1.1 ${match ? 403 : 404} Refused\r\n\r\n`);
      socket.destroy();
      return;
    }
    terminals.handleUpgrade(request, socket, head, (client) => {
      const agent = decodeURIComponent(match[1]);
      let size = url.searchParams.has("cols")
        ? { cols: Number(url.searchParams.get("cols")), rows: Number(url.searchParams.get("rows")) }
        : terminalSize;
      const draw = () => {
        if (!terminalReflow) return;
        // Full-screen fixture: right/bottom edge sentinels expose clipping,
        // and a cleared redraw exposes duplicate output after a width change.
        client.send(Buffer.from(`\x1b[2J\x1b[HFRAME ${size.cols}x${size.rows}` +
          `\x1b[2;1H${"x".repeat(size.cols - 1)}R` +
          `\x1b[${size.rows};1HBOTTOM`));
      };
      client.send(JSON.stringify(size));
      if (terminalReflow) client.send(Buffer.from("\x1b[?1049h"));
      draw();
      if (!terminalReflow) client.send(Buffer.from(`${agent} $ `));
      client.on("message", (data, isBinary) => {
        // Text is a resize control frame, binary is keystrokes. The daemon
        // reflows the session and answers with the shape it settled on;
        // echoing a control frame back would type it at the shell instead.
        if (!isBinary) {
          size = JSON.parse(data.toString());
          client.send(JSON.stringify(size));
          draw();
          return;
        }
        client.send(Buffer.from(data));
      });
    });
  });

  let backend = "codex";
  function newChat() {
    return { id: randomUUID(), title: "New chat", created_at: Date.now(), updated_at: Date.now(), messages: [], subscribers: new Set(), sequence: 0, busy: false, runs: new Map(), plans: new Map() };
  }
  let activeChat = newChat();
  const chats = new Map([[activeChat.id, activeChat]]);
  function chatSummary(value = activeChat) {
    return { id: value.id, title: value.title, created_at: value.created_at, updated_at: value.updated_at, message_count: value.messages.length, ea_id: [...chats.keys()].indexOf(value.id), busy: value.busy, run: value.run ?? null };
  }
  const agentToken = randomUUID();

  function publishChat(role, text, design, progress = false, selection = [], chat = activeChat) {
    chat.sequence += 1;
    const message = {
      sequence: chat.sequence,
      role,
      text,
      progress,
      design: design ?? null,
      selection,
    };
    if (role === "operator" && !chat.messages.some((m) => m.role === "operator")) chat.title = text.slice(0, 80);
    chat.messages.push(message);
    chat.updated_at = Date.now();
    if (role === "operator") chat.busy = true;
    else if (!progress) chat.busy = false;
    const kind = design ? "design_proposed" : "message";
    const frame = `id: ${message.sequence}\nevent: ${kind}\ndata: ${JSON.stringify(message)}\n\n`;
    for (const subscriber of chat.subscribers) subscriber.write(frame);
    return message;
  }

  function subscribeChat(response, chat) {
    response.writeHead(200, {
      ...CORS,
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    response.write(`event: conversation\ndata: ${JSON.stringify(chatSummary(chat))}\n\n`);
    response.write(": connected\n\n");
    // Replay, matching the real server, so a reload rejoins the conversation.
    for (const message of chat.messages) {
      const kind = message.design ? "design_proposed" : "message";
      response.write(
        `id: ${message.sequence}\nevent: ${kind}\ndata: ${JSON.stringify(message)}\n\n`,
      );
    }
    const stateFrame = `event: chat_state\ndata: ${JSON.stringify(chatSummary(chat))}\n\n`;
    if (chat.run && liveChatReplayDelayMs) setTimeout(() => response.write(stateFrame), liveChatReplayDelayMs);
    else response.write(stateFrame);
    const subscribedChat = chat;
    subscribedChat.subscribers.add(response);
    response.on("close", () => subscribedChat.subscribers.delete(response));
  }

  /**
   * Stands in for the EA. Asks one clarifying question first, so the studio's
   * multi-turn handling is exercised, then proposes on the next message.
   */
  function converse(body, response, chat) {
    let request;
    try {
      request = JSON.parse(body);
    } catch {
      return json(response, 400, { error: "invalid request: not JSON" });
    }
    if (typeof request.text !== "string" || request.text.trim().length === 0) {
      return json(response, 400, { error: "message is empty" });
    }
    const selection = Array.isArray(request.selection) ? request.selection : [];
    if (selection.length > MAX_SELECTION) {
      return json(response, 400, {
        error: `selection names more than ${MAX_SELECTION} components`,
      });
    }
    if (request.conversation_id && request.conversation_id !== chat.id) return json(response, 409, { error: "The active chat changed." });
    const publish = (role, text, design, progress = false) => publishChat(role, text, design, progress, [], chat);
    const operator = publishChat("operator", request.text, null, false, selection, chat);
    json(response, 202, operator);

    const asked = chat.messages.some(
      (message) => message.role === "assistant" && !message.progress,
    );
    // Real assistants narrate while they work; that must not end the wait.
    setTimeout(
      () => publish("assistant", "Reading the ports and reactions…", null, true),
      Math.max(10, stepMs / 3),
    );
    setTimeout(() => {
      if (!asked) {
        publish(
          "assistant",
          "Which agent should own the final write, the planner or the reviewer?",
        );
        return;
      }
      // The real daemon compiles the proposal and attaches the topology, so
      // the operator sees what they are approving before any run exists.
      const preview = structuredClone(golden);
      preview.status = "ready";
      const plan = workflowDesign ? workflowFixture(PROPOSED_PROGRAM, {"flow.request":chat.messages[0]?.text ?? ""}, preview, chat.id, !!request.workflow_plan) : null;
      if(plan) plan.draft.research.issues=structuredClone(workflowIssues);
      if(plan && workflowBenchmark) for(const selection of plan.selections) { selection.evidence_ids=["synthetic-extraction-v1"]; selection.evidence_assessments={"synthetic-extraction-v1":{status:"candidate",reasons:["Synthetic display fixture only"]}}; }
      if(plan && workflowFacts) fixtureProfileFacts(plan,workflowFacts,workflowFactsLifetimeSeconds);
      if(plan && workflowToolBlocked) {
        plan.draft.steps[0].required_tools=["web"];
        const decision=plan.selections.find(s=>s.agent===plan.draft.steps[0].agent);
        decision.tool_access=[{tool:"web",status:"unknown",reason:"Declared support only; no reviewed connection attempt."}];
        decision.capability_blockers=["Required tool 'web': Declared support only; no reviewed connection attempt."];
      }
      if(plan && request.workflow_plan) {
        const previous=chat.plans.get(request.workflow_plan.id);
        if(previous) {plan.id=previous.id;plan.revision=previous.revision+1;}
      }
      if(plan) chat.plans.set(plan.id,plan);
      // Markdown, because that is what the EA actually writes.
      publish(
        "assistant",
        "Drafted a **three-reaction** review loop.\n\n" +
          "- `planner` drafts the plan\n" +
          "- `reviewer` critiques it\n\n" +
          "```\nrequest -> planner -> reviewer -> result\n```",
        {
          program: PROPOSED_PROGRAM,
          ...(plan ? {plan:{id:plan.id,revision:plan.revision}} : {}),
          inputs: { "flow.request": chat.messages[0]?.text ?? "" },
          preview,
        },
      );
      // Real assistants comment straight after proposing. That must not look
      // like a withdrawal of the proposal.
      setTimeout(
        () => publish("assistant", "It is in your queue for approval."),
        stepMs,
      );
    }, stepMs);
  }

  function latest(chat = activeChat) {
    return [...chat.runs.values()].at(-1);
  }

  /**
   * What the real daemon does with `POST /v1/programs/check`.
   *
   * The real one runs omarc; this recognises the same shapes the browser tests
   * need — a name that is not a program file, and the marker the suite already
   * uses to mean "omarc rejects this".
   */
  function checkProgram(body, response) {
    let request;
    try {
      request = JSON.parse(body);
    } catch {
      return json(response, 400, { error: "invalid request: not JSON" });
    }
    const filename = request.filename ?? "program.omar";
    if (!filename.endsWith(".omar") || filename.includes("/")) {
      return json(response, 200, {
        ok: false,
        errors: [
          `'${filename}' is not a program file name: it must be a plain name ending in .omar`,
        ],
      });
    }
    if (typeof request.program !== "string" || request.program.trim() === "") {
      return json(response, 200, { ok: false, errors: [`${filename}: empty program`] });
    }
    // Not a compiler: two shapes it recognises, so a test can write source that
    // reads like source instead of a marker string. Anything else is accepted,
    // and the conformance suite is what checks the real compiler's verdicts.
    const opens = (request.program.match(/{/g) ?? []).length;
    const closes = (request.program.match(/}/g) ?? []).length;
    if (request.program.includes(INVALID_MARKER) || opens !== closes) {
      return json(response, 200, {
        ok: false,
        errors: [
          `omarc failed: ${filename}: expected identifier, found some (Omar.Token.sym "{")`,
        ],
      });
    }
    // The daemon reports the team the program declares: the name on `main` when
    // it carries one, and the file's own stem otherwise. Not a compiler, but
    // enough of one that the answer depends on the text -- which is the whole
    // of what a drawing following the editor relies on.
    const declared =
      request.program.match(/\bmain\s+([A-Za-z_]\w*)/)?.[1] ??
      filename.replace(/\.omar$/, "");
    const answer = {
      ok: true,
      team: declared,
      open_inputs: openInputsOf(golden).map((port) => port.name),
      preview: { ...structuredClone(golden), team: declared },
    };
    // Only when asked, as the daemon does: a caller that wants validity alone
    // gets no timeline, and a client that reads one it did not ask for would
    // pass here and fail against the real thing.
    if (request.timeline) {
      // A timeline the shape of the real one: one tag per reaction, in the
      // order the captured topology has them. The real scheduling is the
      // runtime's, and the conformance suite is what checks it.
      const steps = golden.reactions.map((reaction, index) => ({
        timestamp: 0,
        microstep: index,
        events: reaction.triggers.map((id) => id.replace(/^(port|timer)::/, "")),
        reactions: [reaction.name],
      }));
      // Nothing set means nothing moves, which is the state the timeline has
      // to be able to show.
      answer.steps = (request.present ?? []).length > 0 ? steps : [];
      answer.truncated = false;
    }
    return json(response, 200, answer);
  }

  /** Inputs nothing in the topology writes to: a diagnostic, as on the daemon. */
  function openInputsOf(snapshot) {
    const fed = new Set(
      snapshot.edges.filter((edge) => edge.kind === "connection").map((edge) => edge.target),
    );
    return snapshot.ports.filter((port) => port.kind === "input" && !fed.has(port.id));
  }

  /** One pending invocation per reaction whose agent is on the `web` backend. */
  function webPending(snapshot) {
    const web = new Set(
      snapshot.agents
        .filter((agent) => agent.backend.toLowerCase() === "web")
        .map((agent) => agent.id),
    );
    const port = (id) => snapshot.ports.find((p) => p.id === id);
    return snapshot.reactions
      .filter((reaction) => web.has(reaction.agent))
      .map((reaction) => ({
        invocation_id: `inv-${reaction.id}`,
        // The name, not the id: that is what the daemon sends, because the
        // invocation protocol addresses an agent by the name the program
        // gave it.
        agent: snapshot.agents.find((a) => a.id === reaction.agent)?.name ?? reaction.agent,
        reaction: reaction.id,
        contract: reaction.contract,
        prompt: `Answer for ${reaction.name}.`,
        trigger_values: Object.fromEntries(
          reaction.triggers.map((id) => [port(id)?.name ?? id, "upstream"]),
        ),
        allowed_effects: Object.fromEntries(
          reaction.effects.map((id) => [port(id)?.name ?? id, port(id)?.type ?? "string"]),
        ),
      }));
  }

  async function admit(body, response, chat) {
    let request;
    try {
      request = JSON.parse(body);
    } catch {
      return json(response, 400, { error: "invalid request: not JSON" });
    }
    if (typeof request.program !== "string" || request.program.length === 0) {
      return json(response, 400, { error: "invalid request: missing program" });
    }
    if (request.program.includes(INVALID_MARKER)) {
      // What the real daemon does with a program omarc rejects.
      return json(response, 400, {
        error: "omarc failed: expected identifier, found some (Omar.Token.sym \"{\")",
      });
    }

    const runId = randomUUID();
    const plan=request.plan ? chat.plans.get(request.plan.id) : null;
    if(request.plan && (!plan || plan.revision!==request.plan.revision || plan.program!==request.program || JSON.stringify(plan.inputs)!==JSON.stringify(request.inputs))) return json(response,409,{error:"revision conflict"});
    if(plan && (plan.draft.intent.assumptions.some(a=>!a.accepted) || plan.draft.questions.some(q=>q.required || q.answer))) return json(response,409,{error:"review incomplete"});
    const address = `${host}:${server.address().port}`;
    const snapshot = structuredClone(golden);
    snapshot.status = "running";
    const requested = request.inputs?.["flow.request"];
    for (const port of snapshot.ports) {
      if (port.name === "flow.request" && typeof requested === "string") {
        port.value = requested;
      }
    }
    const entry = {
      plan:request.plan??null,profiles:plan?.profiles??{},source_digest:sourceDigest(request.program),
      record: {
        run_id: runId,
        team: snapshot.team,
        status: "running",
        diagram_address: address,
        started_at: Math.floor(Date.now() / 1000),
        finished_at: null,
        error: null,
      },
      snapshot,
      subscribers: new Set(),
      sequence: 0,
      driving: false,
      // What the run's web agents owe. The real daemon builds this from live
      // invocations; here it is seeded for the reaction whose agent the
      // captured topology puts on the `Web` backend, so a client has something
      // to answer.
      pending: webPending(snapshot),
      answered: null,
    };
    // Each live run has an independent diagram server, just like the runtime.
    const diagramServer = createServer((request, response) => {
      if (request.url === "/v1/diagram") return json(response, 200, entry.snapshot);
      if (request.url === "/v1/events") return subscribe(response, entry);
      return json(response, 404, { error: "not found" });
    });
    await new Promise((resolve) => diagramServer.listen(0, host, resolve));
    entry.record.diagram_address = `${host}:${diagramServer.address().port}`;
    entry.diagramServer = diagramServer;
    runs.set(runId, entry);
    chat.runs.set(runId, entry);
    chat.run = entry.record;
    json(response, 201, entry.record);
  }

  function subscribe(response, entry = latest()) {
    if (!entry) return json(response, 404, { error: "no run" });
    response.writeHead(200, {
      ...CORS,
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    response.write(": connected\n\n");
    entry.subscribers.add(response);
    response.on("close", () => entry.subscribers.delete(response));

    // The real diagram server does not replay history, and a real run lasts far
    // longer than a subscriber takes to attach. Only start driving once someone
    // is listening, so the test can never lose the race and hang.
    if (!entry.driving) {
      entry.driving = true;
      void driveRun(entry);
    }
  }

  function publish(entry, kind, payload, tag = null) {
    entry.sequence += 1;
    entry.snapshot.sequence = entry.sequence;
    const event = {
      protocol_version: 1,
      sequence: entry.sequence,
      team: entry.snapshot.team,
      tag,
      kind,
      payload,
    };
    const frame = `id: ${event.sequence}\nevent: ${kind}\ndata: ${JSON.stringify(event)}\n\n`;
    for (const subscriber of entry.subscribers) subscriber.write(frame);
  }

  /** Walk the captured topology the way a real run would. */
  async function driveRun(entry) {
    const wait = () => new Promise((resolve) => setTimeout(resolve, stepMs));
    await wait();
    publish(entry, "run_started", {});

    for (const [index, reaction] of entry.snapshot.reactions.entries()) {
      // Nanoseconds, as the runtime sends them: a tag a second apart, each one
      // a fixed 250ms behind its logical time so the readout has something to
      // show that is neither zero nor a round second.
      const tag = { timestamp: (index + 1) * 1_000_000_000, microstep: 0 };
      const lag = 250_000_000;
      entry.snapshot.current_tag = tag;
      entry.snapshot.lag = lag;
      publish(entry, "tag_advanced", { lag }, tag);
      await wait();

      reaction.status = "running";
      reaction.invocation_id = `inv-${index}`;
      publish(entry, "reaction_started", { reaction: reaction.id }, tag);
      await wait();

      // A web-backed reaction parks until a client answers it, which is the
      // whole point of the backend. Driving straight past would let the run
      // finish while a panel still showed work outstanding — and would make a
      // test of "answer it and the run moves" test nothing.
      while ((entry.pending ?? []).some((item) => item.reaction === reaction.id)) {
        await wait();
      }

      reaction.status = "completed";
      for (const port of entry.snapshot.ports) {
        if (reaction.effects.includes(port.id)) {
          port.value = `${reaction.contract} value`;
          port.last_tag = tag;
        }
      }
      publish(entry, "reaction_completed", { reaction: reaction.id }, tag);
      await wait();
    }

    entry.snapshot.status = "completed";
    entry.record.status = "completed";
    entry.record.finished_at = Math.floor(Date.now() / 1000);
    publish(entry, "run_completed", { outputs: { result: "final answer" } });
  }

  await new Promise((resolve) => server.listen(port, host, resolve));
  return {
    url: `http://${host}:${server.address().port}`,
    agentToken,
    async close() {
      for (const client of terminals.clients) client.terminate();
      terminals.close();
      for (const entry of runs.values()) {
        for (const subscriber of entry.subscribers) subscriber.end();
        entry.diagramServer.closeAllConnections();
        await new Promise((resolve) => entry.diagramServer.close(resolve));
      }
      for (const chat of chats.values()) for (const subscriber of chat.subscribers) subscriber.end();
      // Pooled keep-alive sockets would otherwise hold `close` open until they
      // idle out, adding seconds to every test.
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.on("data", (chunk) => {
      body += chunk;
    });
    request.on("end", () => resolve(body));
    request.on("error", reject);
  });
}

function json(response, status, body) {
  const payload = JSON.stringify(body);
  response.writeHead(status, {
    ...CORS,
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  response.end(payload);
}

export { INVALID_MARKER };

// `node tests/fake-serve.mjs` for manual poking against `npm run dev`.
if (import.meta.url === `file://${process.argv[1]}`) {
  const fake = await startFakeServe();
  console.log(`fake omar serve: ${fake.url}`);
}
