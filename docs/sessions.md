# Independent runtime sessions

A session owns one background runtime, its EAs, chats, topology timelines, event
scheduler, workspaces, and agent processes. An EA is a namespace **inside** a
session. TUI/browser clients can disconnect without stopping any workload.

## Commands

| Command | Behavior |
| --- | --- |
| `omar up [--name NAME] [--web] [--tui]` | Create a new session; return after readiness; `--web` opens Mission Control, `--tui` attaches the terminal dashboard |
| `omar up --workdir PATH --no-ea` | Start without launching an assistant |
| `omar ls [--json]` | List sessions, health, URL, and exact executable build |
| `omar info -s SESSION` | Inspect session, EAs, agents, and runs |
| `omar logs -s SESSION [--follow] [--tail N]` | Read runtime log |
| `omar attach -s SESSION --tui [--ea EA]` | Terminal dashboard as a client of that session, inside its tmux server; z detaches, Q stops the session |
| `omar attach -s SESSION --web [--print-url]` | Open or print Mission Control URL |
| `omar down -s SESSION [--timeout SECONDS]` | Reject new work, finish current tags, clean owned processes |
| `omar down -s SESSION --force` | Terminate owned workloads without waiting for tag boundaries |
| `omar serve [--name NAME] [--address 127.0.0.1:PORT]` | New independent foreground runtime; SIGINT/SIGTERM requests graceful shutdown |
| `omar run FILE [-s SESSION] [--ea EA] [--input NAME=VALUE] [--wait]` | Run a topology in the selected runtime, or in a new session when none is selected; `--wait` prints its outputs and final state, and shuts down a session it created. `start` is an alias |
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
forwarded operations return a stdout/stderr envelope.

Every command that targets a session takes it as `-s`/`--session` (id or
name). Explicit `--session` wins over inherited `OMAR_SESSION_ID`. Without
either, the command fails with selection guidance. No persisted global selection.
Explicit `--ea` wins; inherited `OMAR_EA_ID` applies only with inherited session
routing. Otherwise EA 0 is selected. `up` **always creates a new session**,
even from an agent inside an existing session; bare `omar` prints the command list. New-session configuration
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

## Layout

The session layout is the only layout: every runtime lives under
`$OMAR_HOME/sessions/<id>/` and nothing targets a shared `~/.omar` state
directory any more. `$OMAR_HOME/config.toml` is only the template a new
session copies. `omar run` (alias `start`) submits a topology to a runtime; the
foreground runner is gone. `manager orchestrate` aliases `attach --tui`.

`attach --tui` runs the terminal dashboard as a client: it reads the
session's state directory and drives the session's tmux server, and it runs
inside a `omar-dashboard` tmux session on that server so popups and agent
attachment work unchanged. The daemon keeps the scheduler, assistant launches
(the dashboard asks it through the control protocol), and every workload.
`z` detaches and leaves everything running; reattaching joins the dashboard
that is still running there, on the EA it was showing unless `--ea` names one.
A fresh dashboard opens EA 0 unless `--ea` names one; no persisted selection.
From an agent pane on the session's tmux server, `attach` switches that client
to the dashboard session instead of running a second dashboard in the pane.
`Q` asks for confirmation, then stops the session's runtime like `omar down`.
Slack and computer bridges belong to the daemon.

## Sandbox boundary

Same-host nested sessions share discovery metadata but not mutable runtime state.
Inside a Docker Sandbox, keep `OMAR_HOME` and the registry inside that sandbox;
never mount the supervising runtime's private directory or tmux/control sockets.
A host needs an explicitly forwarded **nested runtime** HTTP endpoint to open
its dashboard, and an authenticated sandbox exec channel to run its CLI/TUI.
For an SSH-capable environment, this is a loopback SSH port forward plus
`ssh -t <environment> omar attach -s <session> --tui`; it is not a shared state mount.
Automatic Docker Sandbox endpoint forwarding and the real microVM nested-build
smoke test depend on the separate sandbox PR #277. This change provides host
session lifecycle; it does not claim that integration or checkpoint/upgrade support.
