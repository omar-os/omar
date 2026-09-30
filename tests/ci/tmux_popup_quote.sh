#!/usr/bin/env bash
set -euo pipefail

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

if ! command -v zsh >/dev/null 2>&1; then
  echo "zsh is required" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

if [ ! -x "$OMAR_BIN" ]; then
  echo "OMAR binary not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

server="omar-popup-quote-${RANDOM}-$$"
home_dir="$(mktemp -d)"

cleanup() {
  tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$home_dir"
}
trap cleanup EXIT

mkdir -p "$home_dir/.omar"
cat >"$home_dir/.omar/config.toml" <<'EOF'
[dashboard]
refresh_interval = 1
session_prefix = "omar-agent-"

[agent]
default_command = "bash"
default_workdir = "."
EOF

tmux_cmd() {
  HOME="$home_dir" OMAR_TMUX_SERVER="$server" tmux -L "$server" "$@"
}

wait_for_session() {
  local session="$1"
  for _ in $(seq 1 50); do
    if tmux_cmd has-session -t "$session" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "Timed out waiting for tmux session: $session" >&2
  tmux_cmd list-sessions -F '#{session_name}|#{session_attached}|#{pane_current_command}' >&2 || true
  return 1
}

tmux -L "$server" new-session -d -s omar-dashboard \
  "cd '$REPO_ROOT' && HOME='$home_dir' OMAR_TMUX_SERVER='$server' '$OMAR_BIN' --legacy"
tmux_cmd set-option -g default-shell "$(command -v zsh)"

wait_for_session omar-dashboard
wait_for_session omar-agent-ea-0

python3 - "$server" <<'PY'
import os
import pty
import signal
import subprocess
import sys
import time

server = sys.argv[1]

def tmux(*args):
    return subprocess.run(
        ["tmux", "-L", server, *args],
        text=True,
        capture_output=True,
    )

def attached(session):
    output = tmux("list-sessions", "-F", "#{session_name}|#{session_attached}")
    if output.returncode != 0:
        return False
    for line in output.stdout.splitlines():
        name, _, state = line.partition("|")
        if name == session:
            return state == "1"
    return False

def wait_for_attached(session, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if attached(session):
            return True
        time.sleep(0.1)
    return False

def fail(message):
    print(message, file=sys.stderr)
    print("Dashboard pane:", file=sys.stderr)
    pane = tmux("capture-pane", "-pt", "omar-dashboard:0.0", "-S", "-80")
    print(pane.stdout or pane.stderr, file=sys.stderr)
    print("Sessions:", file=sys.stderr)
    sessions = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|attached=#{session_attached}|cmd=#{pane_current_command}",
    )
    print(sessions.stdout or sessions.stderr, file=sys.stderr)
    sys.exit(1)

pid, fd = pty.fork()
if pid == 0:
    os.environ.setdefault("TERM", "xterm-256color")
    os.execvp("tmux", ["tmux", "-L", server, "attach-session", "-t", "omar-dashboard"])

try:
    if not wait_for_attached("omar-dashboard"):
        fail("Dashboard did not attach through pty harness")

    os.write(fd, b"\r")

    if not wait_for_attached("omar-agent-ea-0"):
        fail(
            "EA popup did not stay attached. "
            "This usually means the popup shell misparsed the exact tmux target "
            "(=omar-agent-ea-0)."
        )

    print("PASS: Enter opens the EA popup under zsh with exact tmux target quoting")
finally:
    try:
        os.kill(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    try:
        os.close(fd)
    except OSError:
        pass
PY
