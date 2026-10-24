# OMAR topology integration corpus

The `.omar` sources live in `tests/topology/src/`. Each picks its own backends;
CI checks that the bytecode spawns the backend its source declared, not that
every source agrees on one. Most use Codex, and running the whole corpus needs
every backend it names installed and authenticated. CI performs login-free
compiler and Rust VM dry-run checks on every pull request. The authenticated
suite runs from trusted branches and can also be invoked locally.

Coverage:

- `AllTypes.omar`: scalar, path/bytes, nested list/option ports; structured values.
- `BusinessLoop.omar`: independently recurring A and B→C subteams,
  offer/ack synchronization at D, and iterative final-delivery QA.
- `FanOutFanIn.omar`: parallel fan-out, tag barrier, and fan-in.
- `OrTriggers.omar`: LF-style OR triggers, different microsteps, absent values.
- `Depths.omar`: a watcher whose two inputs sit one and three hops from the
  same source. Plain connections cost nothing, so both are decided at one tag
  and it fires once. Stub.
- `EffectContracts.omar`: alternatives, optional groups, signals, constants.
- `SchemaEnum.omar`: two types imported from the JSON Schema files under
  `src/schemas/`, one with a title and description. A prompt writes both enum
  ports and a code body reads them. Stub, so the route it reports is fixed by
  the schema files.
- `SchemaNested.omar`: the same imported types inside `list` and `option`,
  written by one prompt and read by another across connections. Stub.
- `SchemaInput.omar`: an imported enum type on an operator-supplied input,
  read by a prompt and a code body. CI also runs it with a value outside the
  enum and checks the run is refused, naming the admitted values. Stub.
- `OrderedWrites.omar`: shared-effect ordering, optional writes, last writer wins.
- `Recurrence.omar`: self-triggered action flow across logical microsteps.
- `Ring.omar`: team parameters, and a `main` block instantiating one team three
  times and wiring the instances into a feedback ring.
- `SameAgentSerial.omar`: same-agent serialization and same-tag fan-in.
- `SuperdenseTime.omar`: `(timestamp, microstep)` tags, fixed action delay,
  additive connection delay, and chronological delivery.
- `HR.omar`: the heterogeneous hiring example using Claude and Codex.
- `HRCodex.omar`: the original hiring example using Codex for every role.
- `Cadence.omar`: one-shot timers opening three rounds and a deadline, with a
  chair whose four prompts share one agent memory. Claude Code.
- `Bureau.omar`: four levels of containment and 32 agents — the heaviest case
  here. Claude Code. The slowest case; its deadline is 40min.
- `Heartbeat.omar`: a periodic timer driving a program that never finishes on
  its own; its Smoke team stops it after three verdicts.
- `WorldNews.omar`: a 2-minute timer; each tick a Codex researcher fetches the
  news of that moment and a writer replaces `world-news-summary.txt` in its
  workspace. For trying checkpoints by hand: every tick differs, the file is
  state on disk, so pause, roll back to an earlier tick and resume, and the
  file is that tick's summary again while the clock continues from there.
  Its Smoke team stops it after two ticks. Reaches the internet.

Every program declares its teams and then instantiates them in a `main` block,
so ports are named `instance.port` and each program takes its source file's
name rather than a team's.

To execute one scenario with live agents, use `omar run` with the `.omar`
source, provide every declared input that no connection feeds, and use
`--replace` when reusing agent names. OMAR invokes `omarc` internally. Live execution requires every backend
declared by that topology to be installed and authenticated.

To compile and execute the complete corpus with representative inputs and local
agents:

```sh
make test-topology
```

Run one case while iterating:

```sh
make test-topology CASE=BusinessLoop
```

The suite includes both heterogeneous `HR.omar` and Codex-only `HRCodex.omar`.
It runs every case even if an earlier case fails. Each case must compile, exit
successfully after completing all effect contracts, and print its completion
marker.

Each program checks itself. Its `Smoke` team takes the outputs the program
owes, calls `request_stop()` once they have all arrived, and `assert!`s at
`shutdown`. A timer is its deadline: when it fires first, the run stops and the
asserts fail, which fails the run. So a passing case ends as soon as it has
passed, and a program with a periodic timer still ends. See `lang/spec.md`
§2.2.

`Live Topology` runs the authenticated suite on every same-repo PR, every
`main` push, and on demand (optionally one `case`). It runs on the
maintainer's self-hosted runner, label `omar-live`, using that machine's Claude
Code and Codex logins; no API keys in GitHub. Fork PRs are skipped. Each run
gets its own tmux server (`TMUX_TMPDIR`) and `OMAR_HOME`, so it never touches
the machine's interactive sessions. Set up a runner:
`.github/scripts/setup-live-runner.sh`.

`Topology Release Gate` runs the same suite on demand on a GitHub-hosted
runner, from the `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` repository secrets.

It used to gate every release. Driving real agents against real models made a
release wait on a long, paid run that could fail for reasons nothing in the
release introduced, so publishing no longer depends on it. What that costs is
worth naming: nothing between a tag and its artifacts exercises a live agent.

The runner prints per-case duration and assertion counts, followed by an
aggregate PASS/FAIL verdict. It exits nonzero if any case fails. Logs and a
machine-readable `results.tsv` summary are written under
`target/topology-tests/`.

The following environment variables adjust local execution:

- `OMAR_TEST_CASE_TIMEOUT_SECONDS`: whole-case backstop timeout; default
  `3000`. Each program's own deadline normally ends it first.
- `OMAR_TEST_INVOCATION_TIMEOUT_SECONDS`: timeout for one agent prompt;
  default `300`.
- `OMAR_TEST_CASE`: run only the named case.
- `OMAR_TEST_PEEK_AGENTS`: capture recent tmux output from every live agent once
  per minute; enabled by the CI workflow.
- `OMAR_TEST_RESULTS_DIR`: directory for logs and metrics.
- `OMAR_HOME`: OMAR state root, where agent logs are copied from; default
  `~/.omar`.
