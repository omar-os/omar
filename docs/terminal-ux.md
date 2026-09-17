# Codex terminal UX regression checks

New Codex commands let Codex choose its default screen mode. Explicit custom
`--no-alt-screen` commands retain their requested behavior. To try the new
behavior, remove that flag from an existing `default_command` and launch a new
session. OMAR does not rewrite commands of already running agents.

In the web terminal, plain Escape goes to Codex (cancel/back). The Close button,
Ctrl+Shift+Escape, and the backdrop detach the viewer without killing the agent.

## Try the local build

From the `fix/codex-ux` checkout (Node 22+, Rust, tmux and Codex installed):

```sh
git switch fix/codex-ux
(cd web && npm ci && npm run build:spa)
cargo build --features ui --bin omar
./target/debug/omar -a codex --ea CodexUXTry serve --ui --address 127.0.0.1:7341
```

This creates a separate test EA and opens its bundled web UI. If that test EA
already exists, add `--restart-ea` to the last command to replace its session
with the new build. Existing sessions retain their original launch command.

Open **Inspect on terminal**, type an unsent draft, and resize the window
narrow→wide→narrow. Check the right and bottom edges, press Escape to cancel
inside Codex, then close with the button or Ctrl+Shift+Escape. Open a second
browser viewer and close them in either order. Ask the test EA to schedule an
OMAR event, then leave a draft in the terminal while the event arrives: the
agent should wake without submitting or overwriting the draft.

## Geometry and concurrent viewers

Detached sessions use the launch terminal's size when available and 120×40
otherwise. The web client fits xterm before opening its WebSocket, passing
`?cols=N&rows=N` so tmux's first redraw and xterm use the same geometry. Control
frames report **client cells including tmux status rows**, not pane rows.
Requests without geometry retain the existing session's size on attachment.

A daemon shares one original geometry across overlapping viewers of the same
window. Only its last viewer can restore that geometry. Restoration targets
immutable tmux IDs, checks server identity, preserves local/inherited
`window-size`, and skips restoration if another client remains attached or an
operator changed the policy. Native tmux sizing rules still arbitrate multiple
clients: `smallest`, `largest`, `latest`, and `manual` are respected. A manually
sized window can therefore be larger than a viewer. Mouse support is enabled
as before to allow tmux scrollback.

Restoration is best effort on orderly disconnect. A daemon crash cannot run
cleanup. Separate daemon processes do not share their baseline registry; an
external client takes ownership rather than being resized on another viewer's
close. Operator changes to size alone (without a policy change) cannot always
be distinguished from tmux's automatic resize.

## Automated reproduction

The Rust suite needs tmux and Python 3, but no Codex credentials or model calls:

```sh
cargo test -p omar terminal -- --test-threads=1
cargo test -p omar config::tests -- --test-threads=1
cargo test -p omar manager::tests -- --test-threads=1
```

`python3 tests/ci/codex_event_wake.py` checks idle wake and active-turn queuing
against the installed Codex app-server, using an isolated home and a local
mock provider without user credentials or external model calls.

`tests/ci/terminal_reflow_fixture.py` enters the alternate screen, redraws on
SIGWINCH, fills a row to the exact right edge, puts a sentinel on the last row,
and records received Escape. Real PTY/tmux tests resize narrow→wide→narrow and
assert no lost edge, clipped bottom row, or duplicate frame. Other regressions
cover both concurrent close orders, external client ownership, replacement
sessions, and explicit/manual policies. These run in the normal Rust CI job.

The browser suite drives the built app and a fake WebSocket TUI with the same
alternate-screen/edge-sentinel behavior:

```sh
cd web
npm ci
npx playwright install chromium
npm run build
npx playwright test --grep 'terminal|a drag resizes'
```

This verifies the initial geometry handshake, repeated width changes, real
Escape bytes on the socket, the explicit close chord, and the Close button.
It runs in the existing browser CI job without tmux or a model.
