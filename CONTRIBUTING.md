# Contributing

Rules for every contributor, human or agent. Agents also follow [AGENT.md](AGENT.md).

## Repository

1. No documentation in the repo: no `docs/` folder, no new Markdown files. Docs drift out of date; user documentation lives on [omar.rs](https://omar.rs), built separately. Exceptions: top-level `README.md`, `lang/spec.md`, `prompts/`, and these contributor files. Explain code in comments, `--help` text and test names, which change with the code. Explain changes in PR descriptions.
2. No binary files (images, GIFs, videos, archives, builds). Site assets go in the [omar.rs repo](https://github.com/omar-os/omar.rs); UI screenshots go in the PR description.
3. Never commit secrets. Keys go in GitHub repository secrets or `sbx secret`, never in files.
4. Justify each new dependency in the PR description. Keep the minimum supported Rust version (1.89, checked in CI).

CI's `Repository Rules` job enforces 1 and 2 on files a change adds.

## Branches and PRs

5. Never push to `main`. Branch off `main` for each PR; don't pile unrelated changes onto an existing branch.
6. For a major feature, push a branch and open a PR so CI runs.
7. Stacked PR: target the parent PR's branch and start the description with "Stacked on #N".
8. When pushing new commits to an open PR, update its description to match.
9. Format code before pushing.
10. Squash commits when merging.
11. Keep writing concise, to the point where grammar can be sacrificed: README, comments, PR descriptions. Human-readable, not essays.

## Tests and CI

12. Every feature comes with a test, and every test runs in CI.
13. When CI fails, find out why and iterate until it passes.
14. Flaky tests: if a re-run passes, say so in the PR and open an issue for the flake. Don't just re-run.
15. Once CI passes, request a review from GitHub Copilot and address it judiciously.
