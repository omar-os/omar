# OMAR Pi extension

This Pi package exposes the tools advertised by `omar mcp-server` as Pi custom
tools named `omar_<tool>`. It uses Pi's `ExtensionAPI.registerTool` and Node's
built-in child-process/JSONL APIs; there is no MCP SDK dependency.

The supported and tested host is `@earendil-works/pi-coding-agent@0.85.1`.
Older `@mariozechner/pi-coding-agent` releases and other Pi versions are not
supported: dynamic registration during `session_start` and the tool execution
signature depend on the host version. The exact optional peer dependency records
this contract without installing a second host for a globally installed Pi.
Loading an extension directly with `-e` bypasses npm peer checks, so use the pinned
host below. Other versions require rerunning the RPC and tool-call tests before
changing the compatibility declaration.

Install the checkout for local development:

```sh
npx --yes --package=@earendil-works/pi-coding-agent@0.85.1 -- pi install ./src/backend/pi
```

The extension starts one session-scoped `omar mcp-server` child on
`session_start` and closes it on `session_shutdown`. `OMAR_BINARY` selects the
binary when `omar` is not on `PATH`; `OMAR_MCP_CONTEXT_FILE` adds
`--context-file <path>` for an exact per-EA/topology context. `OMAR_DIR` and
`OMAR_EA_ID` are also passed through for the usual OMAR context selection.

Shutdown sends SIGTERM, waits up to one second, then sends SIGKILL if necessary;
the shutdown handler waits for the child to close. Failed discovery attempts
discard their discovery state so a later `session_start` retries every tool.
Pi replaces tools by name when registering again; any tools registered before a
failure reject execution while the MCP session is inactive.

Names keep ASCII letters, digits, underscores, and hyphens; other characters
become underscores. Discovery rejects duplicate names or sanitization collisions
(for example, `tool/name` and `tool_name`) before registering any tools. The
`omar_` prefix is reserved for this extension and must not be reused by another
extension.

Run the Node unit tests and pinned Pi RPC smoke test with:

```sh
npm --prefix src/backend/pi test
```

The RPC test uses `npx` to obtain the pinned host unless `PI_BINARY` points to a
Pi 0.85.1 executable. First use may require network access.

The Rust half is `src/backend/pi.rs`, beside this directory. Launched panes use
`OMAR_PI_SOCKET`, a private per-launch Unix socket published after successful
tool discovery. Delivery uses `sendMessage` with `triggerTurn` and `followUp`,
so an idle Pi wakes and a busy Pi queues the event without touching the composer.
Discovery failure leaves delivery unavailable; there is no terminal-input fallback.
Shutdown closes the socket and its clients before stopping the MCP child.

Mission Control saves Pi's native session file per chat. Reopening the chat uses
`--session` when that file still exists; otherwise Pi starts a new conversation
with OMAR's saved transcript. Explicit session flags take precedence.
