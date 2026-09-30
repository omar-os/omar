#!/usr/bin/env bash
set -euo pipefail

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

if [ ! -x "$OMAR_BIN" ]; then
  echo "OMAR BIN not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

server="omar-unresolved-session-${RANDOM}-$$"
home_dir="$(mktemp -d)"
unresolved_session="omar-agent-rest-api"
legacy_session="omar-agent-ea"

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
  for _ in $(seq 1 80); do
    if tmux_cmd has-session -t "$session" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "Timed out waiting for tmux session: $session" >&2
  tmux_cmd list-sessions -F '#{session_name}|#{session_attached}|#{pane_current_command}' >&2 || true
  return 1
}

tmux_cmd new-session -d -s omar-dashboard \
  "cd '$REPO_ROOT' && HOME='$home_dir' OMAR_TMUX_SERVER='$server' '$OMAR_BIN' --legacy"

wait_for_session omar-dashboard
wait_for_session omar-agent-ea-0

tmux_cmd kill-session -t "$unresolved_session" 2>/dev/null || true
tmux_cmd new-session -d -s "$unresolved_session" "sleep 9999"
wait_for_session "$unresolved_session"

tmux_cmd kill-session -t "$legacy_session" 2>/dev/null || true
tmux_cmd new-session -d -s "$legacy_session" "sleep 9999"
wait_for_session "$legacy_session"

python3 - "$server" "$unresolved_session" "$legacy_session" <<'PY'
import re
import subprocess
import sys
import time

server = sys.argv[1]
unresolved_session = sys.argv[2]
legacy_session = sys.argv[3]


def tmux(*args):
    return subprocess.run(
        ["tmux", "-L", server, *args],
        text=True,
        capture_output=True,
    )


def strip_ansi(s):
    ansi = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")
    return ansi.sub("", s)


def capture_dashboard():
    output = tmux(
        "capture-pane",
        "-pt",
        "omar-dashboard:0.0",
        "-S",
        "-120",
    )
    if output.returncode != 0:
        return ""
    return strip_ansi(output.stdout)


def wait_for(predicate, timeout=6.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(0.1)
    return False


def fail(message):
    print(message, file=sys.stderr)
    print("Dashboard pane:", file=sys.stderr)
    print(capture_dashboard(), file=sys.stderr)
    print("Sessions:", file=sys.stderr)
    sessions = tmux(
        "list-sessions",
        "-F",
        "#{session_name}|attached=#{session_attached}|cmd=#{pane_current_command}",
    )
    print(sessions.stdout or sessions.stderr, file=sys.stderr)
    sys.exit(1)


def unresolved_visible():
    targets = [unresolved_session, legacy_session]
    view = capture_dashboard()
    return all(
        "[unresolved]" in view and session in view
        for session in targets
    )


if not wait_for(unresolved_visible, timeout=12.0):
    fail("Did not observe all unresolved sessions in dashboard")

print(
    "PASS: unresolved session '{}' and legacy '{}' are shown with [unresolved] markers".format(
        unresolved_session,
        legacy_session,
    )
)
PY
