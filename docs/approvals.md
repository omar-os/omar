# Approval monitoring in Omar's web UI

Approval requests are independent of execution status. A blocked agent shows an
orange dot with **Waiting for approval**, while other agents retain their running
state. The web header counts pending requests; clicking it opens the request and
selects its invocation when the corresponding topology is displayed. The executive
assistant has an indicator in its chat header even before a topology exists.

The review panel shows the action, tool, scope and elapsed waiting time. **Open
agent terminal** opens the same agent session where the backend can accept the
operator's response. Opening or closing the panel/terminal never resolves a request.
Inline Allow/Deny actions are intentionally absent until an integration supports
them end to end.

## Supported sessions

Codex sessions use the per-pane app-server that Omar provisions. That observer
subscribes to the one loaded thread without overriding the session's model,
permissions or approval policy. Commands, file changes, additional permissions,
MCP elicitations and MCP tool approval questions provide structured signals.
Ordinary questions and silence do not.

An explicit backend `waitingOnApproval` flag can provide a generic request when
detailed replay is unavailable. Such a request directs the operator to the
terminal for the exact scope; its timer starts when Omar detects it. Detailed
requests use the backend's timestamp where available. MCP approval questions are
recognized by the backend's `mcp_tool_call_approval_` question marker.

Claude Code, Cursor and Antigravity expose their native permission overlay only
in their interactive terminal sessions. Their observers read the current tmux
pane and recognize the exact overlay controls used for a tool permission; they
do not send keys, install a decision hook or infer a request from inactivity.
The notification provides a bounded description and directs the operator to the
native terminal for the authoritative prompt. Claude questions and plan-mode
pickers are excluded from this detection.

Other backends, older versions with different terminal wording, command wrappers
that prevent pane capture, and ambiguous panes with multiple loaded threads
cannot provide full monitoring. The API reports connection availability
independently; the assistant header offers terminal access when monitoring is
unavailable. This feature does not change how agents are launched or relax their
permissions.

## Adding a backend

`ApprovalHub` owns shared request identity, invocation correlation, timestamps,
connection state and bounded retention. `ApprovalObserver` receives an
`ApprovalSink` for publishing display metadata and lifecycle updates; it owns
transport discovery, parsing, deduplication and authoritative reconciliation.
It must not submit approval decisions or change backend permissions.

`src/approvals/backends/mod.rs` is the capability registry. It registers Codex,
Claude Code, Cursor and Antigravity. To add another backend, implement
`ApprovalObserver` in that directory and register its factory. Neither the hub
nor the runtime/UI call sites need another backend-specific branch. Codex socket
discovery, WebSocket JSON-RPC and protocol fixtures live in `backends/codex.rs`;
the non-Codex terminal overlay observers live in `backends/terminal.rs`.

Both topology agents and the executive assistant select from this registry using
their actual backend identity. Unsupported backends report `unsupported`
immediately and do not start a Codex monitor. Existing unattended launch defaults
(e.g. Claude/Antigravity skip-permissions and Cursor yolo) remain unchanged;
lack of notification support never overrides an operator's permission settings.

## Lifecycle and wire protocol

`GET /v1/approvals` returns an `ApprovalSnapshot`. `GET /v1/approvals/events`
streams complete snapshots as SSE `approvals` events, including an initial
snapshot on every connection. Wire types are generated from Rust alongside the
rest of the UI protocol. Requests are scoped to an agent, run and invocation;
the assistant has no run or invocation requirement.

Reloading the page preserves pending requests in the daemon. Disconnects retain
them and show **Connection lost**. Reconnection replays backend requests and
reconciles authoritative thread state. Acknowledgements resolve requests; a bare
acknowledgement is never described as approval or successful completion. Explicit
declined item status is recorded as denial; run teardown cancels remaining requests.
The recent resolution list is bounded. Daemon restart persistence is not provided;
live supported sessions are rediscovered and pending requests replayed.

Only bounded display metadata is published. Private reasoning, tool results and
arbitrary argument objects are excluded. Obvious credential-bearing commands and
explanations are hidden; the terminal remains the authoritative full review view.

Protocol reference: [Codex app-server](https://learn.chatgpt.com/docs/app-server).
