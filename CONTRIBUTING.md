# Contributing

Rules for every contributor, human or agent. Agents also follow [AGENT.md](AGENT.md).

## Design principles

OMAR's philosophy, collected so PRs need less back-and-forth. When a design question comes up, check here first; when a review settles one, add it here.

- **`.omar` source is canonical; bytecode is a cache.** The source is the program of record; bytecode can always be rebuilt from it. So checkpoints keep the source and rollback restores it; editor diffs compare against the source the run executes, never bytecode; operators review `.omar` changes, not instructions.

## Repository

1. No documentation in the repo: no `docs/` folder, no new Markdown files. Docs drift out of date; user documentation lives on [omar.rs](https://omar.rs), built separately. Exceptions: top-level `README.md`, `lang/spec.md`, `prompts/`, the session guides (`docs/sessions.md` and `docs/tui.md`), and these contributor files. Explain code in comments, `--help` text and test names, which change with the code. Explain changes in PR descriptions.
2. No binary files (images, GIFs, videos, archives, builds). Site assets go in the [omar.rs repo](https://github.com/omar-os/omar.rs); PR screenshots and videos go on a `pr-assets/<branch>` branch (rule 9).
3. Never commit secrets. Keys go in GitHub repository secrets or `sbx secret`, never in files.
4. Justify each new dependency in the PR description. Keep the minimum supported Rust version (1.89, checked in CI).

CI's `Repository Rules` job enforces 1 and 2 on files a change adds.

## Branches and PRs

5. Never push to `main`. Branch off `main` for each PR; don't pile unrelated changes onto an existing branch.
6. For a major feature, push a branch and open a PR so CI runs.
7. Stacked PR: target the parent PR's branch and start the description with "Stacked on #N".
8. When pushing new commits to an open PR, update its description to match.
9. Show the change working. If a PR changes anything a person can see or run, its description has:
   - **Evidence**: a screencast for UI or multi-step flows, a screenshot or terminal output for a CLI. Record against the real system (real `omar serve`, `omarc`, assistant; not test fakes) at the PR's latest commit; re-record when a push changes what it shows.
   - **Try it by hand**: copy-pasteable steps from a clean checkout: build commands; input files in full (the complete `.omar` programs, not "a program with a timer"); each command with its expected output; one failure case where it matters (e.g. a rejected change).

   Otherwise write "No visible change" (refactors, tests only). CI shows tests pass; this shows the feature does what the PR says, checkable in minutes. Recording: videos under two minutes; speed up waits and label them "(sped up)"; captions never cover what they describe. Put assets on an orphan `pr-assets/<branch>` branch, linked via `raw.githubusercontent.com`.
10. Format code before pushing.
11. Squash commits when merging.
12. Keep writing concise, to the point where grammar can be sacrificed: README, comments, PR descriptions. Human-readable, not essays.

## Tests and CI

13. Every feature comes with a test, and every test runs in CI.
14. When CI fails, find out why and iterate until it passes.
15. Flaky tests: if a re-run passes, say so in the PR and open an issue for the flake. Don't just re-run.
16. Once CI passes, request a review from GitHub Copilot and address it judiciously.
