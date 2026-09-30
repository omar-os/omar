# Independent runtime sessions

A session owns one background runtime, its EAs, chats, topology timelines, event
scheduler, workspaces, and agent processes. An EA is a namespace **inside** a
session. TUI/browser clients can disconnect without stopping any workload.

## Commands

| Command | Behavior |
| --- | --- |
| `omar` / `omar up [--name NAME]` | Create a new session; return after readiness |
| `omar up --workdir PATH --no-ea` | Start without launching an assistant |
| `omar ls [--json]` | List sessions, health, URL, and exact executable build |
| `omar info SESSION` | Inspect session, EAs, agents, and runs |
| `omar logs SESSION [--follow] [--tail N]` | Read runtime log |
| `omar attach SESSION` | TUI client: q detach, s switch, Tab EA, arrows/Enter inspect agent |
| `omar web SESSION [--print-url]` | Open or print Mission Control URL |
| `omar down SESSION [--timeout SECONDS]` | Reject new work, finish current tags, clean owned processes |
| `omar down SESSION --force` | Terminate owned workloads without waiting for tag boundaries |
| `omar serve [--name NAME] [--address 127.0.0.1:PORT]` | New independent foreground runtime; SIGINT/SIGTERM requests graceful shutdown |
| `omar -s SESSION [--ea EA] start FILE [--input NAME=VALUE] [--wait]` | Admit a topology; optionally wait as a client |
| `omar -s SESSION [--ea EA] runs [--all-eas]` | List topology runs |
| `omar -s SESSION [--ea EA] status RUN-OR-TEAM` | Inspect one run; ambiguous names require a run ID |
| `omar -s SESSION [--ea EA] stop RUN-OR-TEAM` | Stop one topology at a tag boundary |
| `omar -s SESSION ea list` | List EAs |
| `omar -s SESSION ea create --name NAME [--agent BACKEND]` | Allocate an independent EA namespace |
| `omar -s SESSION --ea EA manager start` | Launch that EA's assistant |
| `omar -s SESSION [--ea EA] list` | List agent panes (different from `ls`) |
| `omar -s SESSION [--ea EA] spawn ...` | Spawn an agent through the owning runtime |
| `omar -s SESSION [--ea EA] kill NAME` | Kill a standalone agent; use `stop` for daemon-owned topologies |
| `omar -s SESSION [--ea EA] event {schedule,list,cancel} ...` | Manage scheduled events |
| `omar -s SESSION [--ea EA] workspace {list,show,snapshot,restore} ...` | Inspect/version team files |

Every group has hierarchical help: `omar event --help` lists its subcommands;
`omar event schedule --help` explains scheduling and targeting. Help never starts
a runtime or writes configuration. `--json` produces structured session results;
forwarded legacy operations return a stdout/stderr envelope.

Explicit `--session` wins over inherited `OMAR_SESSION_ID`. Without either,
runtime commands fail with selection guidance. No persisted global selection.
Explicit `--ea` wins; inherited `OMAR_EA_ID` applies only with inherited session
routing. Otherwise EA 0 is selected. `up` and bare `omar` **always create a new
session**, even from an agent inside an existing session. New-session configuration
uses `--config`/`-a`, the shared configuration template, or defaults; it never
copies the parent's private state. `--ea` is for existing-session commands.

`down` is shutdown, not resumable pause. A timeout leaves shutdown pending; it
never silently escalates to force. Use `info`/`logs`, or explicitly force. State
and logs remain on disk after shutdown, but restarting running topologies from
checkpoints belongs to a later milestone. A stale/unreachable record is never
permission to signal a PID: control must verify the session's incarnation.

## Storage and build ownership

Discovery: `$OMAR_HOME/registry`, default `~/.omar/registry`.
Private state: `$OMAR_HOME/sessions/<id>/` (owner-only directory).
Each session has its own config, EA registry, chats, event store, workspace
metadata, credentials, logs, HTTP port, control socket, and dedicated tmux server.
The private local control protocol verifies protocol version, session ID, and
incarnation before executing operations.

The runtime and available `omarc` compiler are copied into the session's `bin/`
at launch. Helpers use that pinned runtime, so rebuilding/replacing an installed
binary does not change existing sessions. `ls`/`info` expose the source path and
SHA-256 build identity. Do not delete a live session directory.

## Migration

Normal launches no longer reuse `omar-dashboard` or implicitly target shared
`~/.omar` state. Existing state is preserved. `--legacy` explicitly retains the
old shared layout and foreground `run` / `serve --ui` / terminal dashboard for
migration and legacy regression tests. It cannot target or inherit a managed
session. Use `start` for daemon-owned topologies; legacy `run` requires
`--legacy`. `manager orchestrate` now aliases terminal attachment.

## Sandbox boundary

Same-host nested sessions share discovery metadata but not mutable runtime state.
Inside a Docker Sandbox, keep `OMAR_HOME` and the registry inside that sandbox;
never mount the supervising runtime's private directory or tmux/control sockets.
A host needs an explicitly forwarded **nested runtime** HTTP endpoint to open
its dashboard, and an authenticated sandbox exec channel to run its CLI/TUI.
For an SSH-capable environment, this is a loopback SSH port forward plus
`ssh -t <environment> omar attach <session>`; it is not a shared state mount.
Automatic Docker Sandbox endpoint forwarding and the real microVM nested-build
smoke test depend on the separate sandbox PR #277. This change provides host
session lifecycle; it does not claim that integration or checkpoint/upgrade support.
