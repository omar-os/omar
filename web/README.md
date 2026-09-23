# OMAR Mission Control

OMAR Mission Control is the first web client for authoring and observing
principled OMAR topology programs. It pairs a conversational workflow-builder
surface with an LF-inspired live diagram of ports, reactions, and connections.

Includes:

- a workflow builder that drafts an OMAR program for you to confirm;
- run admission against `omar serve`, then live observation of that run;
- an LF-inspired diagram: ELK layout drawn as SVG, no KIELER/KLighD;
- live tag, reaction, value, and execution status;
- a fixed-viewport three-panel workspace;
- CI that lints, builds, contract-tests the protocol, and drives the whole
  flow end to end in a browser without invoking a model.

## Development

Requires Node.js 22.13 or newer. Everything below runs from `web/`.

```bash
npm install
npm run dev
```

Open `http://localhost:3000`. With no flag it runs the offline demo topology:
you can draft and inspect, but not run.

## Two builds of the same application

`npm run build` produces the Cloudflare Worker: server-rendered, deployed, and
told the daemon's address at launch through `OMAR_SERVE_URL`.

`npm run build:spa` produces a static bundle entered from the browser, which
`omar serve --ui` embeds and hands out from its own port. There the page is
same-origin with the API, so it reads the address from the page it was served
by and there is no mode to choose. `spa/main.tsx` is that entry; it renders the
same `Studio` component.

The bundle is gzipped by `build/compress-spa.mjs` and embedded under the
runtime's `ui` cargo feature, so a plain `cargo build` needs no Node.

## Running it

Mode is chosen at launch, not in the UI. To run for real, use `make dev` from
the repository root — it builds the runtime, starts the daemon, starts Mission
Control pointed at it, and opens a browser. Ctrl-C stops both.

```bash
make dev
```

`OMAR_SERVE_ADDRESS`, `OMAR_WEB_PORT` and `OMAR_DEV_OPEN=0` adjust it. The two
halves separately, if you want them in separate terminals:

```bash
cargo run --bin omar -- serve --address 127.0.0.1:7340   # from the repository root
OMAR_SERVE_URL=http://127.0.0.1:7340 npm run dev         # from web/
```

The sidebar holds saved chats. The chat composer offers backend selection and
**Inspect on terminal**; a connection error appears if the runtime is unreachable.
Describe a workflow, then **Confirm & run**. Mission Control posts the
program to `/v1/runs`; `omar serve` compiles it, starts the run, and returns
that run's `diagram_address`, which the client observes over `/v1/diagram` and
`/v1/events`.

Nothing executes before the confirmation step, and the confirm button stays
disabled unless the daemon is live. Both runtime surfaces are loopback-only.

## Saved conversations

Each chat owns an independent EA workspace using the runtime's multi-EA support.
**Recent chats** lists saved conversations with **Thinking** and **Running**
indicators. Switch chats while an assistant replies or a topology runs; returning
to a chat reconnects its conversation, live diagram, terminals and pending port
panel. Browser tabs select chats independently. Switching reconnects a live EA or resumes its saved backend session; it never
deploys a proposal.

Messages, selections, proposals and each chat's EA ID are saved atomically in
`~/.omar/ea/<starting-ea-id>/chats.json` under the configured state directory.
Existing #245 histories remain readable; additional chats receive their own EAs
when opened. These EAs also appear in the native EA registry. Each workspace has
its own sessions, callbacks, runs and panels; identical topology names can run
in different chats simultaneously.

Reloading the browser reconnects to live work. The deploy confirmation opens a
modal explaining that a running topology keeps the runtime and assistants alive
after every Mission Control window closes. Reopening the same URL reconnects to
the live run and saved chat. `omar serve --ui` also reuses an existing runtime.

With no active topologies (including starting or stopping runs) and no connected
Mission Control windows, a 10-second grace period precedes shutdown of the owned
EA processes, their child processes, and the runtime. Reconnecting during the
grace period cancels shutdown. When a background run finishes, the grace period
starts then. API-only daemons that have never had a chat stream stay running.

After idle shutdown, launch `omar serve --ui` again to start the runtime and
restore the highlighted chat. Its native backend session is resumed when an ID
is available: Claude and Codex use native resume, Cursor loads its ACP session,
Antigravity restores its conversation, and OpenCode reselects its saved session.
IDs and the selected backend are scoped to each chat's EA, never the provider's
global "last" conversation. OMAR's durable transcript remains available as
context when native history is unavailable. Restarting the runtime does not
recover topology execution; running topologies prevent automatic shutdown.
Unsent drafts, manual source edits and terminal scrollback are not archived.
Reopened proposals still require deployment confirmation.

The catalog routes remain `/v1/chats` and `/v1/chats/<id>/activate`. Activation
only remembers a default selection. Clients pin a workspace by prefixing its
API and terminal routes with `/chats/<id>` (for example,
`/chats/<id>/v1/chat/events` and `/chats/<id>/v1/runs`). Unprefixed routes use the
remembered selection. Agent callbacks are routed by their EA's private token,
not by the dashboard's selection. Chat streams replay the transcript followed
by a `chat_state` event containing the current busy state and latest run.

## Architecture

`app/lib/protocol.ts` holds both contracts: diagram protocol v1 and the
`omar serve` run-admission API. `app/lib/runtime-client.ts` admits runs, polls
run records, loads snapshots, and subscribes to server-sent events.
`app/diagram/diagram-canvas.tsx` maps topology nodes into an ELK graph and draws
the result as SVG.

`app/lib/design-agent.ts` is the drafting seam. `scriptedDesignAgent` returns a
fixed program, and a model-backed agent replaces it without touching the
confirm/spawn state machine — which is why the end-to-end test can drive the
code that ships.

## Testing

```bash
npm test              # unit + protocol contract tests, incl. a build
npm run test:e2e      # Playwright: prompt -> design -> confirm -> deploy -> finished
npm run test:conformance   # the fake vs. the real daemon (needs the runtime)
```

The browser tests drive a fake `omar serve`, which is what makes them fast and
model-free — and only as truthful as the fake is. `test:conformance` is what
keeps it honest: it starts the **real** daemon alongside the fake and asserts
they answer identically, so a changed status code, error shape, or field name
in the runtime fails here rather than leaving the browser suite green over a
broken integration.

It invokes no model. Two tricks cover the whole path:

- a program naming a backend the runtime cannot resolve fails before any agent
  starts, exercising admission, compilation and failure reporting;
- a team of `Stub` agents runs to completion, so `run_started` through
  `run_completed` is observed on a real stream.

It runs against the runtime built from this same commit, which is the point of
keeping them in one repository:

```bash
(cd .. && cargo build --bin omar) && (cd ../lang && lake build omarc)
npm run test:conformance
```

It skips itself when those binaries are absent, so a Node-only checkout is
unaffected. `OMAR_BIN` and `OMARC_BIN` override the paths.

`tests/fixtures/diagram-snapshot.v1.json` was captured from a real `omar serve`
run. `tests/protocol-contract.test.mjs` asserts the client's and renderer's
requirements against it, so a runtime field rename fails here rather than
silently. `tests/fake-serve.mjs` replays that snapshot over the real wire
protocol, which is how the browser test runs without a model, a Lean toolchain,
or tmux.
