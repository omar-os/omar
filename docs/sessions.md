# Independent runtime sessions

A session owns one background runtime, its EAs, chats, topology timelines, event
scheduler, workspaces, and agent processes. An EA is a namespace **inside** a
session: a new session starts with EA 0, and `omar ea create` adds more. TUI and
browser clients are attachable and detachable; they never own anything. One
dashboard (TUI or Mission Control) shows exactly one session.

## Commands

| Command | Behavior |
| --- | --- |
| `omar up [--name NAME] [--workdir PATH] [--no-ea] [--web] [--tui]` | Create a new session (or restart a stopped one by name); return after readiness; `--web` opens Mission Control, `--tui` attaches the terminal dashboard |
| `omar ls [--json]` | List sessions: name, status, workflows, URL, exact executable build |
| `omar rm -s SESSION` | Remove a session and its state; a running one is stopped first |
| `omar info -s SESSION` | Inspect session, EAs, agents, and runs |
| `omar logs -s SESSION [--follow] [--tail N]` | Read runtime log |
| `omar attach -s SESSION --tui [--ea EA]` | Terminal dashboard as a client of that session, inside its tmux server; `z` detaches, `Q` stops the session; see [tui.md](tui.md) |
| `omar attach -s SESSION --web [--print-url]` | Open or print Mission Control URL |
| `omar down -s SESSION [--force] [--timeout SECONDS]` | Reject new work; running workflows pause at their next tag boundary; the session keeps its state |
| `omar serve [--name NAME] [--address ADDR] [--no-ea] [--ui]` | New independent foreground runtime; SIGINT/SIGTERM requests graceful shutdown; `--ui` insists on the bundled Mission Control |
| `omar run FILE [-s SESSION] [--ea EA] [--input NAME=VALUE] [--fg]` | Run a topology in the selected runtime, or in a throwaway session when none is selected; `--fg` follows the run to its end and prints outputs; `start` is an alias |
| `omar -s SESSION [--ea EA] runs [--all-eas]` | List topology runs |
| `omar -s SESSION [--ea EA] status RUN-OR-TEAM` | Inspect one run; ambiguous names require a run ID |
| `omar -s SESSION [--ea EA] stop RUN-OR-TEAM` | Stop one topology |
| `omar -s SESSION [--ea EA] pause RUN-OR-TEAM [--wait]` | Checkpoint at the next tag boundary, then stop the run's agents; `resume` continues it |
| `omar -s SESSION [--ea EA] resume RUN-OR-TEAM` | Continue a paused run from its resume point |
| `omar -s SESSION [--ea EA] rollback RUN-OR-TEAM --checkpoint ID` | Move a paused run's resume point back; nothing is deleted |
| `omar -s SESSION checkpoint {create,list,show,verify,configure,retry} ...` | Request, inspect, verify, and configure run checkpoints |
| `omar -s SESSION ea list` | List EAs |
| `omar -s SESSION ea create --name NAME [--agent BACKEND]` | Allocate an independent EA namespace, optionally on another backend |
| `omar -s SESSION ea start EA` | Launch or relaunch that EA's assistant |
| `omar -s SESSION [--ea EA] ea event {schedule,list,cancel} ...` | Manage the EA's scheduled events |
| `omar -s SESSION [--ea EA] list [--all-eas]` | List agent panes (different from `ls`) |
| `omar -s SESSION [--ea EA] spawn -n NAME [-c CMD] [-d DIR]` | Spawn an agent through the owning runtime |
| `omar -s SESSION [--ea EA] kill NAME` | Kill a standalone agent |
| `omar -s SESSION [--ea EA] workspace {list,show,snapshot,restore} ...` | Inspect/version team files |

Every group has hierarchical help: `omar ea --help` lists its subcommands;
`omar run --help` explains its flags. `omar --help` lists commands by section;
the last section is what OMAR launches inside agent panes and hooks, never typed
by a person. Help never starts a runtime or writes configuration. `--json`
produces structured session results; forwarded operations return a stdout/stderr
envelope.

## Selecting sessions and EAs

Every command that targets a session takes it as `-s`/`--session` by name or id.
A session's name is its only identity: `up` gives it a memorable two-word name
unless `--name` sets one, and the name must be unused among every listed
session (`omar rm` frees a stopped one). Explicit `--session` wins over
inherited `OMAR_SESSION_ID`. Without either, the command fails with selection
guidance. There is no persisted global selection.

Inside the selected session, `--ea NAME-OR-ID` picks the EA; default is EA 0.
`OMAR_EA_ID` applies only with inherited session routing (no explicit
`--session`); an explicit `--session` resets EA selection to EA 0 unless
`--ea` also says otherwise.

Global flags compose by when they are allowed, not by position:

- `-a`/`--agent`, `-c`/`--config`, `--spawn-metrics` configure a **new**
  session only: on `up`/`serve`, or on a `run` that creates its own session.
  Targeting an existing session with them errors instead of half-applying.
  A restart (`omar up --name` of a stopped session) keeps the configuration it
  was created with; `-a`/`-c` are rejected there too.
- `up` rejects `--session` (it always creates) and `--ea` (a new session
  starts with EA 0; use `omar -s SESSION ea create` for more).
- `-a codex` therefore does **not** mean "run this command on Codex". It means
  "a session created by this command uses Codex for its first EA":
  `omar -a codex up --name dev`, or extra EAs afterwards with
  `omar -s dev ea create --name review --agent codex`.

`up` **always creates a new session**, even from an agent inside an existing
session. New-session configuration uses `--config`/`-a`, the shared
configuration template, or defaults; it never copies the parent's private state.

## Stopping, checkpoints, restart

`down` keeps the session like a stopped container: record, logs, chats, and
workflow state stay under `~/.omar/sessions/<name>/` until `omar rm`.
`omar up --name <name>` starts it again in place, and paused workflows can then
`resume`. `down` first pauses running workflows at their next tag boundary;
`--force` terminates without waiting. A timeout leaves shutdown pending — it
never silently escalates to force; use `info`/`logs`, or ask for `--force`
explicitly.

Run checkpoints are separate from session state: `omar run --checkpoint-period
off|tag|30m|logical:30m` sets the cadence (default hourly physical time), and
`omar checkpoint ...` manages them. A session whose startup failed keeps its
directory either way, so its log explains why. A stale/unreachable record is
never permission to signal a PID: control verifies the session's incarnation
before executing operations.

## Migrating from 0.4.2 or earlier

Before sessions, all state lived directly under `~/.omar/`. Current OMAR reads
only `~/.omar/sessions/<name>/`, so that state needs a one-time move. The old
layout is byte-identical to a session directory, so the migration is: let OMAR
build a proper session shell, stop it, move the old files in, and boot it.

Stop the old OMAR and its agent tmux panes first. Run this one-time recipe
with Bash and Python 3 (Linux or macOS). It respects `OMAR_HOME`; the default
is `~/.omar`. Set `OMAR_MIGRATION_NAME` to choose another destination name.
An existing session without this recipe's marker is refused. Newly generated
placeholders are parked internally, and interrupted moves can be retried by
running the same recipe. A conflict stops without overwriting either copy.

```bash
#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

home = Path(os.environ.get("OMAR_HOME", str(Path.home() / ".omar"))).resolve()
name = os.environ.get("OMAR_MIGRATION_NAME", "migrated")
if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", name):
    sys.exit("Choose a session name using 1–64 letters, digits, '-' or '_'.")
dest = home / "sessions" / name
marker = dest / ".legacy-import.json"
park = dest / ".legacy-import-placeholders"
items = "eas.json eas.lock ea_next_id active_ea scheduled_events.json ea prompts logs metrics workspaces workspace-history".split()
items += sorted(p.name for p in home.glob("manager_notes_ea*.md") if p.is_file() or p.is_symlink())

def exists(path):
    return os.path.lexists(path)

def fingerprint(path):
    if path.is_symlink():
        return ["link", os.readlink(path)]
    if path.is_dir():
        return ["dir", {p.name: fingerprint(p) for p in sorted(path.iterdir())}]
    return ["file", hashlib.sha256(path.read_bytes()).hexdigest()]

def cli(*args):
    return subprocess.check_output(["omar", *args], text=True)

def state():
    return json.loads(cli("--json", "info", "-s", name))["session"]["state"]

def save():
    tmp = marker.with_suffix(".tmp")
    with tmp.open("w") as stream:
        json.dump(record, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(tmp, marker)

def migrate():
    global record
    if exists(dest) and not marker.is_file():
        sys.exit(f"Refusing existing session {dest}; choose another OMAR_MIGRATION_NAME.")
    if not exists(dest):
        if not any(exists(home / item) for item in items):
            print("No legacy state to migrate.")
            return
        cli("up", "--name", name, "--no-ea")
        # If interrupted before this marker, refuse the ambiguous shell on retry.
        record = {"version": 1, "home": str(home), "name": name, "phase": "preparing"}
        save()
    record = json.loads(marker.read_text())
    if (record.get("version"), record.get("home"), record.get("name")) != (1, str(home), name):
        sys.exit(f"Migration marker does not match {dest}")
    if record["phase"] == "complete":
        if state() == "stopped":
            cli("up", "--name", name)
        print(f"Migration already complete: {dest}")
        return
    if record["phase"] == "preparing":
        cli("down", "-s", name)
        if state() != "stopped":
            sys.exit("Session must be stopped before importing.")
        record.update(phase="moving", items=items,
                      placeholders={item: fingerprint(dest / item) for item in items + ["config.toml"] if exists(dest / item)},
                      config=fingerprint(home / "config.toml") if exists(home / "config.toml") else None)
        save()
    if state() != "stopped":
        sys.exit(f"Stop session {name} before resuming this migration.")
    park.mkdir(exist_ok=True)
    for item in record["items"]:
        src, dst, parked = home / item, dest / item, park / item
        if not exists(src):
            continue
        if exists(dst):
            if exists(parked) or fingerprint(dst) != record["placeholders"].get(item):
                sys.exit(f"Conflict: {src} and {dst}; not overwriting.")
            os.rename(dst, parked)
        # Explicit destination prevents directory nesting. Same-filesystem rename
        # leaves either the source or the destination intact after interruption.
        os.rename(src, dst)
    if record["config"] is not None:
        src, dst, parked = home / "config.toml", dest / "config.toml", park / "config.toml"
        if fingerprint(src) != record["config"]:
            sys.exit("Legacy config changed during migration; inspect before continuing.")
        if exists(dst) and fingerprint(dst) != record["config"]:
            if exists(parked) or fingerprint(dst) != record["placeholders"].get("config.toml"):
                sys.exit(f"Conflict: {dst}; not overwriting.")
            os.rename(dst, parked)
        if not exists(dst):
            tmp = dest / ".legacy-config.tmp"
            with tmp.open("wb") as stream:
                stream.write(src.read_bytes())
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(tmp, dst)
    history = dest / "workspace-history"
    if history.is_dir():
        for item in sorted(history.iterdir()):
            repository = item / "repository.git"
            if not repository.is_dir():
                continue
            worktree = dest / "workspaces" / item.name / "worktree"
            if not worktree.is_dir():
                sys.exit(f"Missing workspace worktree: {worktree}")
            subprocess.run(["git", "--git-dir", str(repository), "worktree", "repair", str(worktree)], check=True)
    record["phase"] = "complete"
    save()
    cli("up", "--name", name)
    print(f"Migrated legacy state to {dest}")

home.mkdir(parents=True, exist_ok=True)
# Advisory lock is automatically released after a crash (Linux and macOS).
with (home / ".legacy-import.lock").open("a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    migrate()
PY
```

The same recipe is checked in as `scripts/migrate_legacy_session.sh`; a binary
installation can use the complete command above. Keep the internal marker and
`.legacy-import-placeholders` for retry/recovery; no separate backup step is
required. A shell created just before interruption but without a marker is
ambiguous: choose a new migration name; the legacy source remains intact.

EA registry, chats, scheduled events, manager notes, and workspace snapshots
come along. Old live runs cannot resume: their tmux server belonged to the old
runtime. Obsolete top-level `mcp` and `dashboard_handoff.json` are left alone;
the session runtime ignores them. After success, rerunning leaves imported data
alone. `omar rm -s migrated` deletes that session and its imported state.

## Storage and build ownership

Discovery: `$OMAR_HOME/registry`, default `~/.omar/registry`.
Private state: `$OMAR_HOME/sessions/<name>/` (owner-only directory).
Each session has its own config, EA registry, chats, event store, workspace
metadata, credentials, logs, HTTP port, control socket, and dedicated tmux
server. The private local control protocol verifies protocol version, session
name, and incarnation before executing operations.

The runtime and available `omarc` compiler are copied into the session's `bin/`
at launch. Helpers use that pinned runtime, so rebuilding/replacing an installed
binary does not change existing sessions. `ls`/`info` expose the source path and
SHA-256 build identity. Do not delete a live session directory.

## Layout

The session layout is the only layout: every runtime lives under
`$OMAR_HOME/sessions/<name>/` and nothing targets a shared `~/.omar` state
directory any more. `$OMAR_HOME/config.toml` is only the template a new session
copies. `omar run` (alias `start`) submits a topology to a runtime; the
foreground runner is gone.

`attach --tui` runs the terminal dashboard as a client: it reads the session's
state directory and drives the session's tmux server, and it runs inside an
`omar-dashboard` tmux session on that server so popups and agent attachment work
unchanged. The daemon keeps the scheduler, assistant launches (the dashboard
asks it through the control protocol), and every workload. `z` detaches and
leaves everything running; reattaching joins the dashboard that is still
running there, on the EA it was showing unless `--ea` names one. A fresh
dashboard opens EA 0 unless `--ea` names one; no persisted selection. Inside the
dashboard `[`/`]` cycle the session's EAs. From an agent pane on the session's
tmux server, `attach` switches that client to the dashboard session instead of
running a second dashboard in the pane. `Q` asks for confirmation, then stops
the session's runtime like `omar down`. Slack and computer bridges belong to
the daemon.

## Sandbox boundary

Same-host nested sessions share discovery metadata but not mutable runtime
state. Inside a Docker Sandbox, keep `OMAR_HOME` and the registry inside that
sandbox; never mount the supervising runtime's private directory or tmux/control
sockets. A host needs an explicitly forwarded nested-runtime HTTP endpoint to
open its dashboard, and an authenticated sandbox exec channel to run its
CLI/TUI. For an SSH-capable environment, this is a loopback SSH port forward
plus `ssh -t <environment> omar attach -s <session> --tui`; it is not a shared
state mount.
