# Slack workflow MVP

Node 22.13+; no npm dependencies. Uses Slack Socket Mode and OMAR's existing
loopback run API. It does not send requests through the executive assistant.

`@Omar run demo` -> configured `.omar` + fixed inputs -> configured output port
-> original Slack thread. The included demo echoes a fixed message without
models, tools, network requests, or application side effects. It is a connectivity
test, not a market-research workflow.

## Setup

1. Build OMAR and its compiler (`make omarc`, `cargo build --bin omar`).
2. Start a dedicated runtime: `target/debug/omar up --name slack-demo --no-ea --json`.
   Keep its returned `url`. GET `<url>/v1/chat` to find the chat's `id`.
3. Copy `config.example.json` to a local config beside it. Set `runtimeUrl` to
   `<url>/chats/<id>`, and set the actual workspace, allowed user, and allowed
   channel IDs. Do not use an unscoped runtime URL. Paths are relative to the
   config file. For another operator-reviewed, read-only workflow, configure
   `programPath`, optional `files` (import path -> disk path), fixed `inputs`,
   `command`, and one `outputPort`. Use `rawInputs: ["port=text"]` for plain text,
   as the CLI does; `inputs` is also available for typed JSON values. The current
   runtime JSON-input encoder adds quotes to ordinary strings, so the demo uses
   `rawInputs`. Chat text cannot select source files or inputs.
4. Create a Slack app with Socket Mode enabled. Create an app-level token with
   `connections:write`. Add bot scopes `app_mentions:read` and `chat:write`,
   subscribe to the `app_mention` bot event, install/reinstall the app, and invite
   it to the allowed channel. This is a mention command, not a slash command.
5. Set `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` in the adapter's shell. Keep secrets
   out of config, command arguments, workflow inputs, and Git. Then run:

   ```sh
   node integrations/slack/slack.mjs integrations/slack/config.local.json
   ```

6. In the allowed Slack channel, mention your bot: `@Omar run demo`.
   It replies with the run ID, then the selected output in the same thread.
   No Slack message is sent until the adapter runs with real credentials and
   receives a command. Stop it with Ctrl+C; it does not stop an accepted OMAR run.

Only run one adapter instance for this configuration. Use a dedicated demo runtime;
sharing it with Mission Control can introduce team conflicts and UI lifecycle
shutdowns. The runtime is never exposed publicly.

## Stock-market research command

`research.omar` runs two Codex agents in sequence: an analyst retrieves dated
public sources, then an editor checks key claims and writes the report. Its fixed
scope is U.S. market news with NVIDIA, Microsoft, Apple and macroeconomic news.
It omits live prices and recommendations. This uses your configured Codex account;
model calls may consume subscription quota. Internet retrieval must be available.

The installed Codex CLI may show an update dialog before accepting work. For a
new dedicated research runtime, resolve the real CLI first and use the included
research-only launcher (it disables update checks, not permission controls):

```sh
export SLACK_RESEARCH_CODEX_BIN="$(command -v codex)"
export PATH="$PWD/integrations/slack/research-bin:$PATH"
target/debug/omar up --name slack-research --no-ea --json
```

Run these commands from the repository root, before creating that runtime. Do not
set `SLACK_RESEARCH_CODEX_BIN` to the launcher itself. Keep the returned URL and
get its chat ID as in Setup. An existing runtime retains its original environment.

Use `config.research.example.json` with your runtime/chat and allowlists, saving
it as `config.research.local.json`. It includes the companion enum schema and
selects `edit.report` for delivery. The enum constrains status labels, not the
truth of the news. Both agents can report complete, partial, or unavailable;
limited/old coverage must be visible in the report. The workflow has a 480-second
budget, so it can take several minutes. The existing runtime inherits the PATH
and backend environment from when it was started.

To switch from the demo, stop the running adapter with Ctrl+C, then in the same
shell (where your Slack tokens are exported):

```sh
node integrations/slack/slack.mjs integrations/slack/config.research.local.json
```

Send `@Omar run research` in the allowed channel. This replaces the one configured
command for that adapter instance; it does not add a command registry. Do not run
the demo adapter and research adapter together for the same Slack app. Topic and
inputs are fixed by config; arbitrary Slack research topics are not supported.

## Behavior and limits

- Exact workspace/user/channel allowlists. Only leading explicit bot mentions and
  the configured `run <command>` are accepted. Bot/subtype events are ignored.
- One run at a time. A different command event while busy receives a busy reply;
  no queue. OMAR also rejects active/paused runs of the same team.
- In-memory event deduplication: up to 2,000 identities for one hour. Reconnects
  retain the cache; process restarts lose it. When full, new events are ignored
  and a capacity notice is logged. An event received after expiry may execute again.
- No automatic start retries. Ambiguous starts/monitoring failures block new starts
  in this process until the operator checks OMAR and restarts the adapter. Inspect
  logs for the run ID if available. Reconcile before restarting; dedup resets.
- Replies are attempted once. Failures log destination/run ID, without tokens,
  message text, provider bodies, or socket URLs. The workflow is never rerun to
  retry a reply. There is no durable inbox, outbox, or crash recovery.
- Completed, failed, stopped, and paused are reported separately. A missing output
  is reported rather than leaking all ports. Replies are plain text and long
  outputs are truncated; full results stay in OMAR while its run record is available.
- Workflow and runtime configuration are trusted operator inputs. Allowlisting
  does not sandbox a workflow. Use this MVP for controlled read-only demonstrations.

## Shared core

The gateway and runtime client live in `../messaging/workflow.mjs`; see its README
for the platform-neutral contract. This adapter owns Slack mentions, Socket Mode,
authentication, thread mapping, escaping and length limits. Existing Slack config
fields and `node integrations/slack/slack.mjs <config>` remain unchanged.

## Tests

```sh
node --test integrations/messaging/*.test.mjs integrations/slack/*.test.mjs
python3 tests/ci/slack_workflow.py
```

The second command needs freshly built `omar`/`omarc`, tmux and Node on PATH. It
creates an isolated runtime, feeds duplicate Socket Mode fixtures through the
adapter, checks exactly one demo run and its captured thread reply, then executes
the research graph with deterministic fixture agents and its imported enum. It
removes the test session. The harness also runs the same core against a mock transport with no Slack imports.
Neither test uses Slack credentials or live models.
Both run in the existing CI Test job.

References: [Socket Mode](https://docs.slack.dev/apis/events-api/using-socket-mode/),
[app_mention](https://docs.slack.dev/reference/events/app_mention/),
[chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/).
