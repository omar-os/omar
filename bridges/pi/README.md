# OMAR Pi extension

This Pi package exposes the tools advertised by `omar mcp-server` as Pi custom
tools named `omar_<tool>`. It uses only Pi's stable `ExtensionAPI.registerTool`
and Node's built-in child-process/JSONL APIs; there is no MCP SDK dependency.

Install the checkout for local development:

```sh
pi install ./bridges/pi
```

The extension starts one session-scoped `omar mcp-server` child on
`session_start` and closes it on `session_shutdown`. `OMAR_BINARY` selects the
binary when `omar` is not on `PATH`; `OMAR_MCP_CONTEXT_FILE` adds
`--context-file <path>` for an exact per-EA/topology context. `OMAR_DIR` and
`OMAR_EA_ID` are also passed through for the usual OMAR context selection.
