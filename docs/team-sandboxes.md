# Team Docker Sandboxes

Opt-in local execution using Docker Sandboxes (the `sbx` microVM runtime).
Requires `sbx` 0.46 or later, Docker sign-in, and a template built with this
OMAR worker protocol. Docker Engine containers are not a fallback.

## Configure

Sign in with `sbx login`. On a fresh installation, Docker also requires a
one-time global network policy, such as `sbx policy init balanced`. This affects
all local Docker Sandboxes: balanced allows typical AI/development traffic;
choose `deny-all` instead if you intend to allow destinations explicitly.

Build the template from the repository root:

```sh
docker build -f sandbox/Dockerfile -t docker.io/YOUR_NAMESPACE/omar-sandbox:v1 .
```

The supplied shell template includes OMAR, tmux, and Rust. It runs Rust reactions
and the model-free `stub` backend. Install the chosen agent CLIs in a derived
template and configure their Docker Sandbox authentication/network access before
using model-backed agents. OMAR does not copy host backend homes or credentials.
Mixed backends use the existing OMAR adapters inside the same team sandbox.

Publish the image yourself, then select its immutable registry digest:

```toml
[sandbox]
template = "docker.io/YOUR_NAMESPACE/omar-sandbox@sha256:YOUR_DIGEST"
```

Or set `omar run program.omar --sandbox-template IMAGE@sha256:DIGEST`.
Without a template, existing host execution remains unchanged. A configured
sandbox failure never falls back to host execution. Docker Engine's image store
is separate from sbx; building an image locally alone does not make it available
to the sandbox runtime.
`OMAR_SBX_BIN` selects a CLI executable for installations outside PATH.

For local development without publishing, import a canonical digest reference:

```sh
docker build -f sandbox/Dockerfile -t omar-sandbox:dev .
docker save -o /tmp/omar-image.tar omar-sandbox:dev
python3 sandbox/pin-image.py /tmp/omar-image.tar /tmp/omar-pinned.tar
sbx template load /tmp/omar-pinned.tar
```

Use the `IMAGE@sha256:...` printed by the helper in the config or CLI. Choose
unused archive filenames; the helper refuses to overwrite an existing archive.
A plain tag-only `docker save` import is insufficient for digest lookup in sbx
0.46: the helper preserves image content and adds its canonical digest name.

## Files and execution

Each exact team instance gets a distinct sandbox, including nested teams.
Only its `worktree/` and `temp/` are mounted read/write, at the same absolute
paths as on the host. Never mount the workspace parent, source checkout,
`workspace-history/`, other teams, or the host Docker socket.

Directly contained agents share the workspace. The host scheduler runs their
reactions in definition order; separate instances can run concurrently.
Rust compilation, Rust bodies, agent CLIs, MCP sidecars, tmux, and backend
delivery channels all run inside the sandbox. Host-to-worker control uses
bounded `sbx exec` stdin/stdout frames. No host invocation listener or token is
passed into the VM; returned effects and state are validated again on the host.

Relative path inputs resolve in the receiving team's worktree. Absolute inputs
inside the seeded source directory are translated to that
team's worktree. Paths outside that source and the team's own mounts are rejected;
cross-team artifact transfer requires a future explicit interface. Ordinary
strings are not interpreted as paths. Agents cannot use the external `.git`
pointer: Git versioning belongs to the host runtime.

Each VM currently requests 2 CPUs and 2 GiB RAM. All agents in that instance
share these limits. The template digest pins the environment; installing extra
tools interactively is not a portable checkpoint.

## Lifecycle and scope

Deployment records persist sandbox names and the template digest before creation. Completion, graceful
stop, force kill, and replacement stop the recorded VMs. A failed stop leaves
cleanup unconfirmed and blocks final/manual snapshots. Cleanup has a single
60-second budget for the deployment, including one inventory check. Workspaces and stopped
VMs remain available; use `sbx rm NAME` when their VM-local state is no longer
needed. OMAR does not automatically delete file history.

Sandbox workers keep agent sessions in VM-local tmux. For inspection:
`sbx exec -it NAME tmux attach`. Host Mission Control terminal attachment and
code-server execution inside the VM need the editor integration follow-up to
PR #270. Do not claim a host editor's terminals/extensions are sandboxed.

This PR retains initial/final file snapshots. End-of-tag version publication,
human-save attribution, consistent whole-topology checkpoints, and pause/resume
are subsequent work. These snapshots are not resumable execution checkpoints.
The EA/control runtime remains on the host; this feature isolates topology teams.

## Validation

`python3 tests/ci/team_sandboxes.py` exercises real OMAR workers/native stub
delivery against a CLI fixture: topology ownership, exact mounts, Rust compilation
inside workers, shared-file ordering, and cleanup failure guards. It runs in CI
but does not demonstrate microVM security. CI also builds the Linux template
and runs that same worker fixture inside it.

For the real filesystem boundary, after signing into Docker Sandboxes:

```sh
OMAR_SANDBOX_TEMPLATE=IMAGE@sha256:DIGEST python3 tests/ci/team_sandboxes_real.py
```

This launches parent/child sandboxes, tests denied host-file/history and other-team file access,
runs native stub invocations and Rust reactions, checks final snapshots, and
removes only the test's recorded VMs.

References:
- [Docker workspace storage](https://docs.docker.com/ai/sandboxes/architecture/)
- [Create shell sandboxes](https://docs.docker.com/reference/cli/sbx/create/shell/)
- [Sandbox exec](https://docs.docker.com/reference/cli/sbx/exec/)
- [Template loading](https://docs.docker.com/ai/sandboxes/usage/#load-a-template)
