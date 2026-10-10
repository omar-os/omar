# Independent runtime sessions

A session owns one background runtime, its EAs, chats, topology timelines, event
scheduler, workspaces, and agent processes. An EA is a namespace **inside** a
session. TUI/browser clients can disconnect without stopping any workload.

## Commands

| Command | Behavior |
| --- | --- |
| `omar up [--name NAME] [--web] [--tui] [--checkpoint]` | Create a new session; return after readiness; `--web` opens Mission Control, `--tui` attaches the terminal dashboard; `--checkpoint` keeps its state after it stops |
| `omar up --workdir PATH --no-ea` | Start without launching an assistant |
| `omar ls [--json]` | List sessions, health, URL, and exact executable build; a stopped session appears only if it was started with `--checkpoint` |
| `omar rm -s SESSION [--force]` | Remove a stopped session's record and state; `--force` takes a running one down first, like `docker rm -f` |
| `omar info -s SESSION` | Inspect session, EAs, agents, and runs |
| `omar logs -s SESSION [--follow] [--tail N]` | Read runtime log |
| `omar attach -s SESSION --tui [--ea EA]` | Terminal dashboard as a client of that session, inside its tmux server; z detaches, Q stops the session |
| `omar attach -s SESSION --web [--print-url]` | Open or print Mission Control URL |
| `omar down -s SESSION [--timeout SECONDS]` | Reject new work, finish current tags, clean owned processes |
| `omar down -s SESSION --force` | Terminate owned workloads without waiting for tag boundaries |
| `omar up --name SESSION` (stopped session) | Start a stopped session again over its kept state, on this build, at its previous URL; its runs are listed again, paused ones resumable |
| `omar upgrade -s SESSION [--executable PATH] [--timeout SECONDS]` | Replace the runtime with this build (or `PATH`), keeping name, URL, state and workloads: runs pause at a tag boundary, the old runtime hands over and stops, the new one starts and resumes what the upgrade paused |
| `omar serve [--name NAME] [--address 127.0.0.1:PORT]` | New independent foreground runtime; SIGINT/SIGTERM requests graceful shutdown |
| `omar run FILE [-s SESSION] [--ea EA] [--input NAME=VALUE] [--wait]` | Run a topology in the selected runtime, or in a new session when none is selected; `--wait` prints its outputs and final state, and shuts down a session it created. `start` is an alias |
| `omar -s SESSION [--ea EA] runs [--all-eas]` | List topology runs |
| `omar -s SESSION [--ea EA] status RUN-OR-TEAM` | Inspect one run; ambiguous names require a run ID |
| `omar -s SESSION [--ea EA] stop RUN-OR-TEAM` | Stop one topology at a tag boundary |
| `omar -s SESSION [--ea EA] pause RUN-OR-TEAM [--wait]` | Checkpoint at the next tag boundary and park the run; see [team-checkpoints.md](team-checkpoints.md) |
| `omar -s SESSION [--ea EA] resume RUN-OR-TEAM` | Continue a paused run under its run id |
| `omar -s SESSION [--ea EA] rollback RUN-OR-TEAM --checkpoint ID` | Move a paused run's resume point to an older checkpoint |
| `omar -s SESSION [--ea EA] checkpoint {create,list,show,verify,configure,retry} ...` | Request, inspect, verify and configure a run's checkpoints |
| `omar -s SESSION ea list` | List EAs |
| `omar -s SESSION ea create --name NAME [--agent BACKEND]` | Allocate an independent EA namespace |
| `omar -s SESSION ea start EA` | Launch or relaunch that EA's assistant |
| `omar -s SESSION [--ea EA] list` | List agent panes (different from `ls`) |
| `omar -s SESSION [--ea EA] spawn ...` | Spawn an agent through the owning runtime |
| `omar -s SESSION [--ea EA] kill NAME` | Kill a standalone agent; use `stop` for daemon-owned topologies |
| `omar -s SESSION [--ea EA] ea event {schedule,list,cancel} ...` | Manage the EA's scheduled events |
| `omar -s SESSION [--ea EA] workspace {list,show,snapshot,restore} ...` | Inspect/version team files |

Every group has hierarchical help: `omar ea --help` lists its subcommands;
`omar ea event schedule --help` explains scheduling and targeting. `omar --help`
lists commands by section; the last section is what OMAR launches inside agent
panes and hooks, never typed by a person. Help never starts
a runtime or writes configuration. `--json` produces structured session results;
forwarded operations return a stdout/stderr envelope.

Every command that targets a session takes it as `-s`/`--session` by name.
A session's name is its only identity: `up` gives it a memorable two-word
name unless `--name` sets one, and the name must be unused among every
listed session (`omar rm` frees a stopped one). Explicit `--session` wins over inherited `OMAR_SESSION_ID`. Without
either, the command fails with selection guidance. No persisted global selection.
Explicit `--ea` wins; inherited `OMAR_EA_ID` applies only with inherited session
routing. Otherwise EA 0 is selected. `up` **always creates a new session**,
even from an agent inside an existing session; bare `omar` prints the command list. New-session configuration
uses `--config`/`-a`, the shared configuration template, or defaults; it never
copies the parent's private state. `--ea` is for existing-session commands.

`down` is shutdown, not resumable pause. A timeout leaves shutdown pending; it
never silently escalates to force. Use `info`/`logs`, or explicitly force. A
session started without `--checkpoint` leaves nothing behind once it stops, like
a tmux session; `ls` no longer lists it. With `--checkpoint` its state directory
and record stay, `ls` shows it `stopped`, and `info`/`logs` still work, like a
stopped Docker container, and `omar up --name NAME` starts it again in place.
A session whose startup failed keeps its directory either way, so
its log explains why. A stale/unreachable record is never
permission to signal a PID: control must verify the session's incarnation.

## Storage and build ownership

Discovery: `$OMAR_HOME/registry`, default `~/.omar/registry`.
Private state: `$OMAR_HOME/sessions/<name>/` (owner-only directory).
Each session has its own config, EA registry, chats, event store, workspace
metadata, credentials, logs, HTTP port, control socket, and dedicated tmux server.
The private local control protocol verifies protocol version, session name, and
incarnation before executing operations.

The runtime and available `omarc` compiler are copied into the session's `bin/`
at launch. Helpers use that pinned runtime, so rebuilding/replacing an installed
binary does not change existing sessions. `ls`/`info` expose the source path and
SHA-256 build identity. Do not delete a live session directory.

## Restart and upgrade

A runtime that starts over an existing state directory offers the runs the
previous runtime left unfinished (`ea/<id>/serve/<run>/run.json` plus each
team's deployment record): paused ones paused, under their own ids. A run
the old runtime was still executing is interrupted: with a checkpoint it is
paused there and `resume` continues from it, the tags since are lost;
without one it has failed. Nothing resumes on its own.

`omar upgrade -s NAME` replaces the runtime with the build running the
command (`--executable PATH` names another; its sibling `omarc` comes along).
Steps, each recorded in the session's `upgrade.json` and shown by `info`:

1. **Check.** The new build runs `upgrade-check` on the session directory:
   it must read the launch and registry records, speak the same control
   protocol, and `verify` every checkpoint a run would continue from (the
   checkpoint format it understands against what is on disk). Anything it
   cannot read stops the upgrade before anything changes. A build that
   bumps the checkpoint format must keep reading older ones; refusal is
   reported here, never discovered mid-upgrade.
2. **Pause.** The session stops admitting work (`ls` shows `upgrading`);
   every active run pauses at its next tag boundary. Runs the operator had
   paused already are left alone. A run that does not pause within
   `--timeout` calls the upgrade off: admission reopens and what did pause
   resumes, on the old build.
3. **Handoff.** The old runtime stops, keeping the state directory whether
   or not the session was started with `--checkpoint`.
4. **Start.** The previous `bin/omar` becomes `bin/omar.prev`; the new build
   is pinned and started over the same state at the same address, so
   Mission Control's URL and `omar attach` keep working. A replacement that
   does not start is rolled back: the previous build is pinned again and
   started (`upgrade.json` says `rolled_back`).
5. **Resume.** The runs the upgrade paused continue under their run ids,
   each from the checkpoint its pause took, in new workspaces.

Other sessions are never touched. External effects between the pause
checkpoint and the handoff are not undone; a run paused by the upgrade loses
nothing, since the pause checkpoint is the boundary it stopped at.

## Layout

The session layout is the only layout: every runtime lives under
`$OMAR_HOME/sessions/<name>/` and nothing targets a shared `~/.omar` state
directory any more. `$OMAR_HOME/config.toml` is only the template a new
session copies. `omar run` (alias `start`) submits a topology to a runtime; the
foreground runner is gone.

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
