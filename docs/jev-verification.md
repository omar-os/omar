# Jev implementation verification — 2026-09-28

Branch: `dan/jev-workflow-assistance`, based on upstream main
`b8a33594d4a0535c5d0ff9a4004f99b2e7c375d8`, with Jev PR #256 integrated locally.
Artifact/editor PR #270 was not merged; this implementation reads existing
workspace snapshots through a narrow text adapter.

| Check | Result |
|---|---|
| Jev regression/profile/queue/disablement/ownership/evaluation tests | 33 passed; live and real-compiler tests separately gated |
| New API ownership and Unicode-selection tests | 2 passed |
| Immutable artifact reader (symlinks, traversal, oversized files, revisions) | Passed |
| Real compiler: all three templates and omitted-handoff variants | Passed using checksum-verified official v0.4.2 `omarc` |
| Feature-disabled protocol generation | 6 matching-filter tests passed |
| Strict feature-enabled Clippy and Rust formatting | Passed |
| Frontend application type-check and full ESLint | Passed |
| SPA production build | Passed; existing large-bundle advisory remains |
| Frontend protocol contract tests | 11 passed |
| Browser workflows | 3 passed: desktop end-to-end, provider failure/shadow, mobile |
| Live Jev quality | Skipped: no `TYPESAFE_API_KEY`; 0 provider requests |
| Corpus | 90 cases; all labels synthetic/provisional; 20 development + 10 held-out per profile |

Browser verification used the built SPA on `http://127.0.0.1:3100`, installed
Chrome through Playwright, API fixtures, and 1440×1000 / 390×844 viewports.
The Browser skill was unavailable. Page identity, meaningful rendering,
absence of framework overlays, app console health, evidence inspection and
interaction results were checked. Opening/enabling/shadow made no evaluation
request; explicit evaluation was counted; no run/chat/permission mutation was
sent. Template rejection and selection, scenario checking before a run, artifact
revision invalidation and provider unavailability were exercised. Screenshots
were inspected; a mobile clipping issue was fixed and the same tests rerun.
These fixture results validate software behavior, not model quality.

## Known baseline checks

The final complete feature-enabled backend run had **497 passed, 1 failed,
10 ignored**. The failure is the unchanged
`reaction::tests::reaction_descendants_stop_on_return_timeout_and_error`, at
`src/reaction.rs:929`, where a return case unexpectedly times out. It passes
alone. The same full-suite failure was reproduced on untouched upstream main:
**461 passed, 1 failed, 8 ignored**, at the same line. An earlier tmux startup
timing failure passed in isolation and in the final serial feature-enabled run.
No unrelated process-control code was changed to mask these failures.

The repository-wide standalone TypeScript command reports missing `Fetcher`
and `D1Database` globals in unchanged `web/worker/index.ts`. The application and
new browser tests type-check when isolated from those Worker globals. The
production SPA build and project ESLint pass. The Worker deployment build and
hosted execution were not validated or published.

## Reproduce

```sh
cargo test --bin omar --features decision-support decisions:: -- --test-threads=1
cargo test --bin omar --features decision-support serve::assistance::tests
cargo test --bin omar --features decision-support advisory_reader
cargo test --bin omar --no-default-features protocol::
OMARC_BIN=/path/to/omarc cargo test --bin omar --features decision-support starter_workflows_compile -- --ignored
cargo clippy --bin omar --features decision-support -- -D warnings
cargo fmt --all -- --check
cd web
npm run lint
npm run build:spa
node --test --experimental-strip-types --disable-warning=ExperimentalWarning tests/protocol-contract.test.mjs
npm run test:e2e -- workflow-advice.spec.ts
```

The committed CI workflow runs the new backend checks with a real Lean compiler;
the existing Web E2E job discovers the new browser suite. CI was configured but
not run remotely. The local browser run used a temporary static SPA server and
proxy to the existing fake daemon, avoiding the unrelated Worker build.

See [implementation and demo](jev-workflow-assistance.md) and the
[machine-readable evaluation status](../eval/jev-v1/evaluation-status.json).
Implementation can be reviewed now. Product enablement remains off pending
human-reviewed labels and measured live performance; no merge, provider-backed
demo, hosted deployment or public launch is claimed.
