# Shared messaging core (Rust)

`src/messaging/` owns configuration, the local runtime HTTP client, authorization,
command matching, bounded in-memory deduplication, busy/uncertain state, monitoring,
output selection, and failure policy. Slack is the first provider implementation.
There is no Node dependency for messaging.

## Run

```sh
cargo build --bin omar
target/debug/omar messaging slack login
target/debug/omar messaging slack integrations/slack/config.local.json
```

The connector runs in the foreground and calls an already running, dedicated OMAR
runtime. It does not start a second runtime or send work through the executive
assistant. Ctrl+C/SIGTERM stops the connector, not an admitted workflow.

## Adapter contract

`Incoming` carries platform, account ID, sender ID, event ID, command and a
`Destination` (platform/account/conversation, optional thread). IDs are opaque and
scoped to an installation. Slack uses one configured workspace/app; future
adapters must distinguish installations correctly.

`Gateway<R, M>` accepts an implementation of `Runtime` and `Messenger`. A messenger
sends a `Reply` (full text and optional run ID) to the authorized destination and
can customize usage hints. Formatting, length limits, provider authentication,
acknowledgements and reconnects belong to the adapter, not the gateway.

The core checks platform/account/user/conversation and destination identity before
replying, deduplicating or executing. Dedup keys include platform/account/event.
No mutex spans network I/O. Admission is marked busy before starting a workflow;
other requests receive a busy reply rather than entering a queue. Reconnection
retains the same gateway and its dedup state.

Slack config `workspaceId`/`allowedChannels` remain accepted. Neutral callers use
`platform`/`accountId`/`allowedConversations`. Source/schema paths are relative to
the config file. Existing `.omar`, schemas, fixed inputs and output selection are
unchanged. No chat message can select arbitrary files or inputs.

HTTP clients disable redirects and request retries. Unknown admission/monitoring
outcomes block new starts pending operator reconciliation. Failed sends never
rerun a workflow. Logs omit tokens, secret socket URLs and provider bodies.

## Tests

```sh
cargo test --locked --bin omar messaging::
python3 tests/ci/slack_workflow.py
python3 tests/ci/slack_credentials.py
```

Rust tests use mock platforms and local HTTP/WebSocket servers. The Python CI
harness invokes Rust tests against an isolated actual OMAR runtime: duplicate
Slack event -> one demo run; research graph with Stub agents and imported enum;
neutral mock caller -> threadless reply. No Slack credentials, external network,
Node, or live models are needed. Both commands run in CI.

## MVP limits

Approved workflow registry per connector (legacy single-workflow configs supported); one invocation at a time; dedup for up to
one hour/2,000 events, lost on process restart; no durable inbox/outbox or global
cross-process queue. Early ACK plus a crash can lose an event. The language port
does not provide exactly-once execution or delivery. Separate gateways targeting
the same team can still receive a runtime conflict. Discord/WhatsApp, persistent recovery and conversational memory are outside this change.

## Slack credentials

`omar messaging slack login` asks for both tokens with hidden input and saves them
in the user config directory, outside workflow/session state. On Unix the file is
0600 and its directory 0700; it is not encrypted. Tokens are replaced atomically.
The connector loads them automatically across terminals. Environment values win
per token. `omar messaging slack logout` removes only the saved copy; it does not
revoke tokens or stop running connectors. No token is accepted as a CLI argument.
There is currently one saved Slack installation per user configuration directory.

## Workflow registry

`Config.workflows` maps approved command names to source/bundle/input/output/timeout
settings. The common gateway implements `list` and exact `run <name>` matching;
Slack only normalizes the mention and sends replies. Registry entries share the
configured runtime and one busy/uncertain state. A second run is refused, never
queued. `list` stays available while busy. Authorization and dedup apply to both
commands, and uncertainty blocks all workflow names. Selected output and timeout
come from the chosen entry. No arbitrary file paths or chat-provided inputs.

See `integrations/slack/config.workflows.example.json`. Register another workflow
by editing config and restarting, without changing adapter code. Existing legacy
configs map to one available workflow and retain their `run <command>` behavior.

## Workflow catalog follow-up

Web UI workflows are not automatically registered for messaging. The current
operator-owned registry is explicit. Follow-up: one local workflow catalog shared
by UI, CLI and messaging, with source/schema locations and execution settings.
Decide between registration on save and an explicit UI action; messaging access
should have a clear enablement policy. This PR does not implement that UI/catalog.
