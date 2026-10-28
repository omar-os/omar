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
