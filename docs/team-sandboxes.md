# Team Docker Sandboxes

Opt-in: each topology team instance runs in its own Docker Sandboxes (`sbx`)
microVM. Off by default; teams run on the host. Once opted in, sandboxes are
required: a run whose host is not ready fails instead of running on the host.

Docker Engine containers are not a fallback. The EA/control runtime stays on
the host.

## Choosing

| Setting | Effect |
|---|---|
| nothing | host |
| `omar run --sandbox-template REF` | sandboxes from `REF` |
| `omar run --no-sandbox` | host, overriding the config |
| `[sandbox] template = "REF"` in `~/.omar/config.toml` | as the flag |
| `[sandbox] enabled = true` | OMAR's published kit |
| `[sandbox] enabled = false` | host |

`REF` is a kit image pinned by digest (`IMAGE@sha256:...`) or a local kit
directory (built by sbx on first use). Config is copied into a session when it is created.
Resume reuses the paused run's choice.

## Ready means

- `sbx` installed and signed in (`sbx login`), global network policy set
  (`sbx policy init deny-all` recommended), `/dev/kvm` usable on Linux (`kvm`
  group, then `sbx daemon stop` from a new login). `sbx diagnose` checks.
- A Linux `omar` for the worker: this binary on Linux; on macOS the release's
  `omar-linux` (installed beside `omar`), or `OMAR_SANDBOX_WORKER_BIN`.
- A model key for every agent backend in the program, stored in sbx and
  approved for third-party kits:

| Backend | sbx service |
|---|---|
| claude | `anthropic` (API key; Claude subscriptions cannot be shared into sandboxes) |
| codex | `openai` (`sbx secret set openai --oauth` for ChatGPT sign-in, or a key) |
| cursor | `cursor` |
| agy | `google` (Gemini API key) |
| opencode, pi | any of anthropic, openai, google, openrouter, xai, mistral, groq |
| stub, Rust reactions | none |
| jev | not supported in sandboxes yet |

```sh
sbx secret set anthropic          # key stays on the host; the VM sees a placeholder
```

Approve it for kits once, in `~/.config/sbx/credentials.yaml` (or let an
interactive `sbx create` prompt):

```yaml
bindings:
  anthropic:
    apiKey:
      domains: [api.anthropic.com]
```

## The kit

`sandbox/omar-sandbox/` (published as `docker.io/lsk567/omar-sandbox`, pinned
in `sandbox::DEFAULT_TEMPLATE`): every backend CLI, Rust, tmux. Network: only
model API hosts, plus whatever the global policy allows; under deny-all
nothing else. Keys are injected by the sbx proxy, never enter the VM. 2 CPUs,
2 GiB per VM, shared by the instance's agents.

OMAR is not in the image: each new VM gets the host runtime's own binary as
`/usr/local/bin/omar` (root-owned), so worker and runtime always match.
Changing the kit: edit, push, take the digest `Sandbox Kit` prints, update
`DEFAULT_TEMPLATE`.

## Files and execution

Only the instance's `worktree/` and `temp/` are mounted, read/write, at the
same absolute paths as on the host. Never the workspace parent, source
checkout, `workspace-history/`, other teams, or the Docker socket.

Rust compilation, Rust bodies, agent CLIs, MCP sidecars and tmux run inside
the VM. Control: newline JSON frames over `sbx exec` stdin/stdout, bounded; no
host listener or token enters the VM; returned writes are re-validated on the
host. Relative path inputs resolve in the team's worktree; absolute paths
outside its mounts are rejected. Agents cannot use the external `.git`
pointer.

Inspect: `sbx exec -it NAME tmux attach`. Host Mission Control terminals and
code-server are not sandboxed.

## Lifecycle

VM names and the kit are recorded before creation. Completion, stop, kill and
replacement stop the VMs; a failed stop blocks final/manual snapshots. Cleanup
has one 60 s budget. Stopped VMs remain: `sbx rm NAME`. Pause stops VMs;
resume starts new ones from the same kit over the restored workspaces; agents
start fresh conversations and are told so.

## Tests

- `tests/ci/team_sandboxes.py`: fake sbx; opt-in, default kit under `enabled = true`,
  required template, opt-out, worker install, lifecycle. Also run inside the
  kit image. Not VM isolation.
- `tests/ci/team_sandboxes_real.py`: real VMs from this checkout's kit; file
  isolation, worker, egress (`OMAR_EXPECT_DENY_ALL=1`). CI: `Sandbox Live` on
  the self-hosted `omar-sandbox` runner (`.github/scripts/setup-sandbox-runner.sh`).
