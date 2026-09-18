# Paired legacy workflow benchmark

This benchmark compares PR #254 (`5a7085e`) with the integrated supervision branch.
It measures model-driven management under the legacy hierarchy, not Mission Control's
separate topology executor. Both branches run their real dashboard scheduler, MCP
server, native backend launches and side-channel delivery. The harness never submits
terminal input or sends a reminder after the initial task.

## Protocol

A scripted EA creates one project and assigns one real model-driven PM a task. The PM
must spawn two real workers of specified, different backends. Each worker runs a bounded
fixture in its own process, obtains a numeric answer and nonce, then uses that branch's
normal completion protocol. The PM must incorporate both results, retire both workers,
write the combined artifact and report completion. Process ancestry independently checks
that the workers, rather than the PM, performed the shard work.

The fixture waits at an I/O gate until both workers have started. The harness opens it
after ten seconds (or after the specified context intervention). This makes the manager
wait for genuinely asynchronous results without adding model instructions or reminders.
All workspace files, trusted-directory configuration and backend credentials are scoped
to private disposable homes. The existing Claude OAuth account is reused rather than an
unfunded API key. Authentication/setup failures are reported separately, never silently
counted as coordination failures. Copied credentials are destroyed after each run.

Parent/children matrix:

| Parent | Left child | Right child |
| --- | --- | --- |
| Claude | Codex | OpenCode |
| Codex | OpenCode | Claude |
| OpenCode | Claude | Codex |
| Cursor | Codex | Antigravity |
| Antigravity | Cursor | OpenCode |

Pinned models: Claude `claude-sonnet-4-6`, Codex `gpt-5.6-sol` with low effort,
Cursor `composer-2.5`, Antigravity `gemini-3.8-flash-medium`, OpenCode `opencode/big-pickle`.
The comparison uses the same model within each backend; it is not a comparison of model
families. Cases are counterbalanced between branches. Task hashes, source revisions,
backend versions, budgets and seeds are recorded. Calibration runs are kept separately
from scored runs; fixes to the harness require a new scored batch.

## Outcomes

A pass requires a correct combined artifact, worker-attributed execution, no live child
sessions and an actual PM completion report. The integrated ledger additionally exposes
result consumption/retirement; baseline results use its native completion notification.
Missing data is not a pass. Startup/delegation failure is distinct from failure after
both workers produced results. Worker-output-to-retirement latency is a measurable
coordination delay, not an inference about model idleness from a quiet terminal.

Raw case directories contain a chronological state trace, assignments, final task ledger,
terminal output and a machine-readable outcome. They stay outside the repository in a
private result directory. Only reviewed aggregate results belong in committed reports.
A small initial batch is a pilot, not a statistically reliable success-rate estimate.

## Context interventions

Native compaction must be verified by the backend, not inferred from issuing a command.
Codex exposes `thread/compact/start`; completed `contextCompaction` items or native
compaction notifications provide the evidence. OpenCode exposes
[`POST /session/:id/summarize`](https://opencode.ai/docs/server/#sessions); the benchmark
requires a new completed summary. Unsupported native compaction is recorded as unsupported.

Restarting a PM into a new native conversation tests recovery from context loss. It is
reported separately from native compaction and must preserve the original task ownership
and live children. No task restatement is sent after the restart.

## Running

Requires installed/authenticated backend CLIs, tmux, Python and `websocket-client` for
Codex compaction. Build the baseline in a separate checkout at the pinned revision.

```sh
OMAR_LIVE_BACKENDS=1 python3 tests/benchmarks/workflow.py \
  --baseline /path/to/baseline/target/debug/omar \
  --candidate target/debug/omar \
  --output /tmp/omar-workflow-results \
  --seeds 314159 --jobs 2 --budget 240
```

This invokes live models and consumes their normal account quota. There are no CI secrets
or account tokens in the result manifest. The fully offline regression suite remains a
separate gate; this benchmark adds evidence about model behavior.

## Executable and fixture isolation

The runner snapshots both binaries under the private output directory before launching
any case. Every subsequent native process and MCP server uses those exact bytes. This
prevents a concurrent development build from silently changing an ongoing trial. The
manifest records the source revisions, binary hashes and hashes of the driver and context
helpers. A nonempty output directory is rejected rather than overwritten.

Cursor's isolated workspace is explicitly trusted for both interactive and ACP launch
modes. Context-reset commands are decoded back into their original argument vector;
passing tmux's escaped display string through another shell can change dollar expansion.
The reset regression test exercises this round trip using a real, disposable tmux server.
After each trial the driver stops captured pane descendants and Codex app servers using
that trial's unique home before removing credentials and fixture files.

A respawned worker invalidates any prior retirement timestamp for that name. Reports
recompute retirement latency from saved state traces, and never use a pre-result exit as
successful result retirement. Missing result files remain explicit invalid trials in the
report. Reviewed setup exclusions are stored separately from raw outcomes.

Generate a report with:

```sh
python3 tests/benchmarks/summarize.py /tmp/omar-workflow-results \
  --json docs/benchmarks/results.json --markdown docs/benchmarks/results.md
python3 -m unittest discover -s tests/benchmarks -p 'test_*.py'
```

The first live results and release limitations are in
[the September 17 report](benchmarks/2026-09-17-analysis.md). They cover all five parent
backends on the normal task. Verified native compaction and fresh-conversation tests cover
Codex and OpenCode; they do not establish context recovery for the other three backends,
Mission Control, deep hierarchies, arbitrary graphs, or long-running projects.
