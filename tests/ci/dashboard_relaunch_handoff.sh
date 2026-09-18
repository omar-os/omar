#!/usr/bin/env bash
set -euo pipefail

# End-to-end check for the dashboard relaunch handoff wired up in
# `relaunch_in_tmux` (src/omar.rs). The unit tests cover the serde
# round-trip and the App-side apply; this test runs the real `omar` binary
# against a fake "already running" dashboard tmux session and asserts the
# handoff JSON written to ~/.omar/dashboard_handoff.json carries the cwd,
# backend, active EA, and restart_manager flag taken from the second
# invocation's arguments.
#
# Why this works without a TTY: relaunch_in_tmux writes the handoff
# *before* it calls `tmux attach-session`. The attach fails in a
# non-interactive subshell, after which omar tries to spawn a fresh
# dashboard which also fails — both side effects we don't care about. The
# handoff file is what we inspect.

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
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

# Absolutize OMAR_BIN so the subshells that `cd` into $work_dir can still find it.
OMAR_BIN="$(cd "$(dirname "$OMAR_BIN")" && pwd)/$(basename "$OMAR_BIN")"

server="omar-handoff-${RANDOM}-$$"
home_dir="$(mktemp -d)"
work_dir="$(mktemp -d)"

# Canonicalize $work_dir because std::env::current_dir() returns the
# resolved path (e.g. /var/folders/... -> /private/var/folders/... on macOS),
# and we compare it verbatim against the handoff's default_workdir.
work_dir="$(cd "$work_dir" && pwd -P)"

cleanup() {
  tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$home_dir" "$work_dir"
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

# Pre-register a single EA so resolve_cli_ea has an unambiguous answer
# without falling back to interactive bootstrap.
cat >"$home_dir/.omar/eas.json" <<'EOF'
[
  { "id": 0, "name": "Default", "description": null, "created_at": 1700000000 }
]
EOF
printf '0' >"$home_dir/.omar/active_ea"

tmux_cmd() {
  HOME="$home_dir" OMAR_TMUX_SERVER="$server" tmux -L "$server" "$@"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

handoff_file="$home_dir/.omar/dashboard_handoff.json"

start_fake_dashboard() {
  tmux_cmd kill-session -t "omar-dashboard" 2>/dev/null || true
  tmux_cmd new-session -d -s "omar-dashboard" "sleep 9999"
  for _ in $(seq 1 50); do
    if tmux_cmd has-session -t "omar-dashboard" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  fail "fake dashboard session did not come up"
}

tmux_cmd new-session -d -s omar-agent-ea-0 "sleep 9999"
original_pane="$(tmux_cmd display-message -p -t omar-agent-ea-0 '#{pane_id}')"

# A long-lived fake claude keeps each -a manager pane alive so consecutive
# launches are a successful startup plus a fresh handoff, not a swallowed
# command-not-found. relaunch_in_tmux still writes before attach; attach is
# expected to fail in this non-interactive subshell.
mkdir -p "$work_dir/bin"
cat >"$work_dir/bin/claude" <<'EOF'
#!/bin/sh
exec sleep 9999
EOF
chmod +x "$work_dir/bin/claude"
PATH="$work_dir/bin:$PATH"
export PATH

run_omar() {
  (
    cd "$work_dir"
    HOME="$home_dir" OMAR_TMUX_SERVER="$server" PATH="$PATH" "$OMAR_BIN" "$@"
  )
}

# Case 1: `omar -a claude` from $work_dir should write a handoff with the
# new cwd, the claude backend command, and restart_manager=false (the new EA is already running).
start_fake_dashboard
rm -f "$handoff_file"

run_omar -a claude >/dev/null 2>&1 || true

if [ ! -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was not written by omar -a claude"
fi

python3 - "$handoff_file" "$work_dir" <<'PY'
import json, sys
path, work_dir = sys.argv[1], sys.argv[2]
with open(path) as fh:
    h = json.load(fh)
errs = []
if h.get("active_ea") != 1:
    errs.append(f"active_ea: expected 1, got {h.get('active_ea')!r}")
if "claude" not in str(h.get("default_command", "")):
    errs.append(f"default_command: expected to mention 'claude', got {h.get('default_command')!r}")
if h.get("default_workdir") != work_dir:
    errs.append(f"default_workdir: expected {work_dir!r}, got {h.get('default_workdir')!r}")
if h.get("restart_manager") is not False:
    errs.append(f"restart_manager: expected False (new EA must survive handoff), got {h.get('restart_manager')!r}")
if errs:
    raise SystemExit("handoff field mismatch (case: -a claude from new cwd):\n  " + "\n  ".join(errs))
PY

# A second backend launch allocates another EA and preserves the first tree.
start_fake_dashboard
rm -f "$handoff_file"
run_omar -a claude >/dev/null 2>&1 || true
if [ ! -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was not rewritten by the second omar -a claude"
fi
python3 - "$home_dir/.omar/eas.json" "$handoff_file" <<'PYTEST'
import json, sys
registry = json.load(open(sys.argv[1]))
handoff = json.load(open(sys.argv[2]))
assert [(e["id"], e["name"]) for e in registry] == [(0, "Default"), (1, "1"), (2, "2")], registry
assert handoff["active_ea"] == 2 and not handoff["restart_manager"], handoff
PYTEST
[ "$(tmux_cmd display-message -p -t omar-agent-ea-0 '#{pane_id}')" = "$original_pane" ] || fail "existing EA was replaced"

# Case 2: bare `omar` (no -a) should still hand off cwd, but
# restart_manager must be false so the live manager isn't kicked.
start_fake_dashboard
rm -f "$handoff_file"

run_omar >/dev/null 2>&1 || true

if [ ! -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was not written by bare omar relaunch"
fi

python3 - "$handoff_file" "$work_dir" <<'PY'
import json, sys
path, work_dir = sys.argv[1], sys.argv[2]
with open(path) as fh:
    h = json.load(fh)
errs = []
if h.get("default_workdir") != work_dir:
    errs.append(f"default_workdir: expected {work_dir!r}, got {h.get('default_workdir')!r}")
if h.get("restart_manager") is not False:
    errs.append(f"restart_manager: expected False (no -a), got {h.get('restart_manager')!r}")
if errs:
    raise SystemExit("handoff field mismatch (case: bare omar from new cwd):\n  " + "\n  ".join(errs))
PY

# Case 3: no existing dashboard => no handoff file should be written.
# (relaunch_in_tmux only saves the handoff inside the has_session branch.)
tmux_cmd kill-session -t "omar-dashboard" 2>/dev/null || true
rm -f "$handoff_file"

run_omar -a claude >/dev/null 2>&1 || true

if [ -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was written on cold start (no existing dashboard)"
fi

echo "PASS: dashboard relaunch writes handoff with correct fields (and skips it on cold start)"
