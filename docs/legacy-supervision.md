# Legacy agent supervision

Tracked legacy agents now have a durable lifecycle in `task-lifecycle.json`.
The dashboard and one elected MCP fallback consumer reconcile this state;
models no longer own the timers that keep their children moving.

| Previous failure | Runtime behavior |
| --- | --- |
| Child exists only as terminal metadata | `spawn_agent` returns a durable `task_id` for supported agent backends. |
| Parent forgets a check-in | Runtime checks every 5 seconds; tracked tasks without OMAR tool activity receive a check-in after 60 seconds, also notifying their parent. |
| Completion notification gets lost | `finish_task` atomically persists the result and notification obligation. Notifications retry every 60 seconds until consumption and retirement. |
| Compaction loses child ownership | Every legacy MCP result carries a bounded live projection. Backend hooks restore/check it at the supported lifecycle boundaries below. |
| Stored message mistaken for wake | OpenCode uses `prompt_async`; Cursor/Antigravity use managed native protocol sessions. Custom Codex flags use native `exec`; default Codex uses app-server. No terminal submission. |
| Terminal activity mistaken for task completion | Structured task status is separate from terminal health. Health checks honor the configured idle interval. |
| Claude coding policy replaced | OMAR uses `--append-system-prompt` / `--append-system-prompt-file`. |

## Backend coverage

All five supported backends use the same durable ledger, ownership checks, result protocol,
MCP context projection, and host-owned reminders. Native adapters supplement that shared runtime:

| Backend | Fresh context | Stop / idle handling |
| --- | --- | --- |
| Claude Code | SessionStart (including compact), UserPromptSubmit | Stop blocks once for actionable obligations. |
| Codex | Every MCP result and host check-in; managed exec refreshes before each turn | Native app-server tool-output wake or managed exec inbox. |
| Cursor | Fresh ledger projection before every ACP turn and after each MCP call | Durable runner inbox starts idle turns and serializes active work. |
| Antigravity (`agy`) | Fresh ledger projection before every stream-JSON turn and after each MCP call | Durable runner inbox starts idle turns and serializes active work. |
| OpenCode | System transform before model calls; compaction context append | session.idle arms the host watchdog for actionable obligations. |

Codex uses the operator's normal `CODEX_HOME`, with a separate socket per live session.
OMAR leaves native user hooks untouched. It does not install additional Codex hooks: newly
generated hooks gate native TUI startup behind a trust review. MCP projections and host check-ins
restore ownership independently; the `agent-hook --format codex` adapter remains available for
operators who explicitly configure and trust it. Commands with profiles, search, or arbitrary
config overrides use native `codex exec` through an OMAR protocol console; Codex itself loads
its configuration layers. Every subsequent turn resumes the saved native conversation ID.
Long state-directory paths use a short private socket directory, preserving the native app-server route.

Cursor ACP and Antigravity stream-JSON launches use an OMAR protocol console in the pane.
These replace their interactive TUI launch paths. The console accepts operator lines; automated
messages arrive only over its private Unix socket. A receipt means the message is persisted,
not that the model completed the turn. Accepted work stays queued until native turn completion.
After a process restart with the same config, the runner resumes the native conversation and
replays pending messages with their original IDs and freshly read ownership state. Recovery is
at least once: a crash after tool execution can replay that turn, so receipts do not guarantee
exactly-once external effects. Cursor permissions are approved only when the launch explicitly
requests `--yolo`/`--force`; otherwise permission requests are cancelled and reported.

Existing Cursor/Antigravity hook-only sessions cannot wake while idle. Ordinary delivery reports
a relaunch requirement instead of claiming success; scheduled obligations remain pending.
The model-free topology stub keeps its actively drained spool.

Native contracts: [Codex hooks](https://learn.chatgpt.com/docs/hooks),
[Cursor ACP](https://cursor.com/docs/cli/acp), [OpenCode plugins](https://opencode.ai/docs/plugins),
and [Antigravity headless protocol](https://antigravity.google/docs/cli/headless/).

## Result protocol

1. Worker calls `finish_task(task_id, status, result)`. Status is `completed`, `failed`, or `blocked`.
2. Parent reads `get_task(task_id, offset?)`; large JSON content is paged at 8,000 characters. Reassemble pages before parsing.
3. Parent incorporates the result and calls `acknowledge_task(task_id, result_revision)`.
4. Parent retires a terminal task with `kill_agent`. For blocked work, resolve the blocker and call `resume_task` instead.

A parent cannot complete while children remain unfinished, unconsumed, or unretired.
Duplicate identical completion is idempotent. A stale acknowledgement cannot consume a newer result.
A crashed supervisor's outstanding children transfer to the nearest surviving ancestor.
Task IDs bind that transfer to a supervisor generation, so reused agent names do not inherit old ownership.

`coordination_state` restores current responsibilities. Automatic context includes at most 32 tasks,
with bounded assignment/result previews; full results remain on disk. Use `coordination_state(offset=next_offset)` to page through remaining tasks. A transport acknowledgement never consumes a result.

## Scope and recovery

- Applies to newly spawned tracked legacy agents. Existing sessions retain their old launch configuration; restart them to receive hooks, identity scoping, and the task protocol.
- Raw demo commands default to metadata only. `supervise: true` opts a custom agent command into the protocol; that command must implement `finish_task`.
- Task files use an OS-managed lock and atomic replacement. Process death releases the lock; malformed state is reported, never silently reset.
- Supervision operates while a dashboard or at least one legacy MCP server is running. Restarting either resumes persisted obligations.
- Explicit Claude `--settings` are preserved. Those sessions use MCP projections and runtime check-ins unless their custom settings install equivalent hooks.
- Stop hooks allow waiting for running children, and block once per turn for actionable obligations. The runtime handles subsequent reminders.
- Worker identities and context files are separate; shared Cursor/Antigravity configuration cannot overwrite the EA's private context.
- Mission Control's topology executor retains its existing invocation/completion protocol. These changes do not establish that topology runs are free of context issues.

## Validation

```
cargo test -p omar -- --test-threads=1
cargo test --bin omar -- --ignored --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build -p omar
python3 tests/ci/legacy_supervision.py
python3 tests/ci/backend_coordination_contracts.py
python3 tests/ci/protocol_runner.py
python3 tests/ci/codex_exec_delivery.py
python3 tests/ci/codex_event_wake.py
python3 tests/ci/claude_prompt_contract.py
python3 tests/ci/opencode_wake_contract.py
```

CI is configured to run these regressions; the integration branch has only been validated locally so far. Native Claude, Codex, and OpenCode tests pin CLI versions in CI and use local mock
providers, without paid inference. The process regression uses production MCP, scheduler, and isolated
tmux sessions; it kills every MCP process after notification acceptance and verifies autonomous redelivery
after restart. Unit regressions cover nested ownership, concurrent completion, stale acknowledgements,
blocked/resumed tasks, crashes, compaction hooks, bounded context, and lossless result paging.
These prove runtime/transport invariants, not universal model quality or an LLM success-rate benchmark.

The all-backend contract test exercises production hook output and the generated OpenCode plugin.
`protocol_runner.py` verifies idle wake, active-turn serialization, durable acceptance, process
crash/replay, native session resume, fresh ownership, and cancellation recovery against deterministic protocol peers.
`codex_exec_delivery.py` uses production MCP spawn/delivery and an installed Codex CLI with a local
provider to check profile/config compatibility and native conversation continuity.

`OMAR_LIVE_BACKENDS=1 python3 tests/ci/native_protocol_agents.py` is an opt-in authenticated test.
Cursor and Antigravity have each completed two idle-separated live turns with the same conversation
ID and remembered content; Cursor prompt-recall history remained unchanged. This is a transport
and continuity check, not a benchmark of autonomous multi-level management or native compaction.
The native Codex wake test does not establish hook trust approval.

`OMAR_LIVE_BACKENDS=1 python3 tests/ci/native_legacy_workflow.py cursor` and the same
command with `agy` each passed with the default backend launch. A live worker finishes through
OMAR MCP; the host wakes a scripted OpenCode-contract parent, which reads, acknowledges and
retires the result, then completes the project. Antigravity runs with a disposable home and a
private copy of cached authentication, leaving user plugin registration untouched. These tests
verify native tool access and the lifecycle across transports; the parent is not an autonomous model.

Local validation also passed formatting, workspace Clippy, the five required shell integration jobs,
430 OMAR binary tests, the remaining OMAR integration tests, and seven explicitly enabled ignored
tests. The 19 bridge tests passed with `/opt/X11` removed from PATH; the full workspace attempt
hung in the existing X11 screenshot test, so real X11 capture was not validated.
