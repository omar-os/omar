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
| Stored message mistaken for wake | OpenCode uses `prompt_async`; passive spools fall back to verified, draft-preserving input. Claude peer and Codex app-server paths stay active. |
| Terminal activity mistaken for task completion | Structured task status is separate from terminal health. Health checks honor the configured idle interval. |
| Claude coding policy replaced | OMAR uses `--append-system-prompt` / `--append-system-prompt-file`. |

## Backend coverage

All five supported backends use the same durable ledger, ownership checks, result protocol,
MCP context projection, and host-owned reminders. Native adapters supplement that shared runtime:

| Backend | Fresh context | Stop / idle handling |
| --- | --- | --- |
| Claude Code | SessionStart (including compact), UserPromptSubmit | Stop blocks once for actionable obligations. |
| Codex | SessionStart (including compact), UserPromptSubmit, PostToolUse | Stop blocks once; native hooks require Codex hook trust. |
| Cursor | sessionStart, postToolUse, postToolUseFailure | Completed stop emits one followup; aborted/error/repeated stops do not. |
| Antigravity (`agy`) | PreInvocation injects an ephemeral message before model calls | Shared host watchdog handles outstanding obligations. |
| OpenCode | System transform before model calls; compaction context append | session.idle arms the host watchdog for actionable obligations. |

Codex per-pane homes retain personal developer instructions and user hooks. OMAR does not bypass Codex's hook
trust checks: review generated hooks through `/hooks` before relying on native lifecycle injection.
MCP projections and the host watchdog work independently of that approval.
Cursor's `beforeSubmitPrompt` cannot inject context; old OMAR entries are migrated to supported events.
Cursor has no immediate post-compaction injection in this adapter; the next supported hook, MCP result,
or runtime check-in restores state. OpenCode appends to system/compaction context instead of replacing defaults.
Antigravity uses its `injectSteps[].ephemeralMessage` hook response.

Hook contracts: [Codex](https://learn.chatgpt.com/docs/hooks),
[Cursor](https://cursor.com/docs/hooks), [OpenCode](https://opencode.ai/docs/plugins).
Antigravity's adapter follows the hook protobuf exposed by the installed CLI; it lacks equivalent
public documentation and is tested at the adapter boundary.

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
python3 tests/ci/codex_event_wake.py
python3 tests/ci/claude_prompt_contract.py
python3 tests/ci/opencode_wake_contract.py
```

CI runs these regressions. Native Claude, Codex, and OpenCode tests pin CLI versions in CI and use local mock
providers, without paid inference. The process regression uses production MCP, scheduler, and isolated
tmux sessions; it kills every MCP process after notification acceptance and verifies autonomous redelivery
after restart. Unit regressions cover nested ownership, concurrent completion, stale acknowledgements,
blocked/resumed tasks, crashes, compaction hooks, bounded context, and lossless result paging.
These prove runtime/transport invariants, not universal model quality or an LLM success-rate benchmark.

The all-backend contract test exercises production hook output and the generated OpenCode plugin.
Native Cursor and Antigravity model turns have not been validated end to end; their adapter tests
do not establish model compliance. The native Codex test validates wake delivery, not hook trust approval.
