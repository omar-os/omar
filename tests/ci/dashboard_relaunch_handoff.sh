#!/usr/bin/env bash
set -euo pipefail

# End-to-end check for the dashboard launch handoff (src/sessions.rs). The
# unit tests cover the serde round-trip and the App-side apply; this test runs
# the real `omar` binary against a session whose dashboard tmux session is a
# fake "already running" one and asserts the handoff JSON written to the
# session's dashboard_handoff.json carries the named EA, the session's backend
# and workdir, and restart_manager=false; and that no handoff is written when
# no EA is named or when no dashboard is running.
#
# Why this works without a TTY: the handoff is written *before* the attach.
# The attach fails in a non-interactive subshell, after which omar tries to
# start a fresh dashboard, which also fails — side effects we don't care about.
# The handoff file is what we inspect.

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

server=""
session_id=""
home_dir="$(mktemp -d)"
work_dir="$(mktemp -d)"

# Canonicalize $work_dir because std::env::current_dir() returns the
# resolved path (e.g. /var/folders/... -> /private/var/folders/... on macOS),
# and we compare it verbatim against the handoff's default_workdir.
work_dir="$(cd "$work_dir" && pwd -P)"

cleanup() {
  [ -n "$session_id" ] && HOME="$home_dir" "$OMAR_BIN" down -s "$session_id" --force --timeout 5 >/dev/null 2>&1 || true
  [ -n "$server" ] && tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$home_dir" "$work_dir"
}
trap cleanup EXIT

mkdir -p "$home_dir/.omar" "$home_dir/bin"
# Keep the real backend and user shell profiles out of this test. Omar starts
# panes with `sh -lc`; the fixture shell skips login profiles so they cannot
# reset PATH and select an installed Claude instead of our long-lived fake.
cat >"$home_dir/bin/sh" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "-lc" ]; then
  shift
  exec /bin/sh -c "$@"
fi
exec /bin/sh "$@"
EOF
cat >"$home_dir/bin/claude" <<'EOF'
#!/bin/sh
printf 'handoff backend fixture ready\n'
exec sleep 9999
EOF
chmod +x "$home_dir/bin/sh" "$home_dir/bin/claude"
export PATH="$home_dir/bin:$PATH"
# Exercise the outside-tmux path even when the test itself runs in a pane.
unset TMUX TMUX_PANE

cat >"$home_dir/.omar/config.toml" <<'EOF'
[dashboard]
refresh_interval = 1
session_prefix = "omar-agent-"

[agent]
default_command = "bash"
default_workdir = "."
EOF

session_json="$(cd "$work_dir" && HOME="$home_dir" "$OMAR_BIN" -a claude up --name handoff --json)" \
  || { echo "FAIL: omar up did not start" >&2; exit 1; }
session_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$session_json")"
server="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["tmux_server"])' <<<"$session_json")"
state_dir="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["directory"])' <<<"$session_json")"

tmux_cmd() {
  HOME="$home_dir" tmux -L "$server" "$@"
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  if [ -f "$home_dir/launch.log" ]; then
    cat "$home_dir/launch.log" >&2
  fi
  tmux_cmd list-panes -a -F '#{session_name}: dead=#{pane_dead} command=#{pane_current_command}' >&2 || true
  exit 1
}

handoff_file="$state_dir/dashboard_handoff.json"

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

assert_backend_alive() {
  local session="omar-agent-ea-$1"
  for _ in $(seq 1 50); do
    if tmux_cmd capture-pane -p -t "$session" 2>/dev/null | grep -q 'handoff backend fixture ready'; then
      [ "$(tmux_cmd display-message -p -t "$session" '#{pane_dead}')" = 0 ] || fail "$session backend exited"
      return
    fi
    sleep 0.1
  done
  fail "$session did not start the backend fixture"
}

# A non-TTY attach fails after the handoff is written. Require a freshly
# written file instead of accepting the process status or an old one.
launch_handoff() {
  rm -f "$handoff_file"
  (
    cd "$work_dir"
    HOME="$home_dir" "$OMAR_BIN" attach -s "$session_id" --tui "$@" </dev/null >"$home_dir/launch.log" 2>&1
  ) || true
}

# The runtime launched EA 0 with the fixture backend; a second EA is the handoff target.
assert_backend_alive 0
original_pane="$(tmux_cmd display-message -p -t omar-agent-ea-0 '#{pane_id}')"
HOME="$home_dir" "$OMAR_BIN" -s "$session_id" ea create --name second >/dev/null || fail "ea create failed"

# Case 1: naming an EA for a dashboard that is already running hands it off.
start_fake_dashboard
launch_handoff --ea second
[ -f "$handoff_file" ] || fail "fresh dashboard_handoff.json was not written for --ea second"
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
    errs.append(f"restart_manager: expected False (a handoff never kicks the live EA), got {h.get('restart_manager')!r}")
if errs:
    raise SystemExit("handoff field mismatch (case: --ea second with a running dashboard):\n  " + "\n  ".join(errs))
PY
[ "$(tmux_cmd display-message -p -t omar-agent-ea-0 '#{pane_id}')" = "$original_pane" ] || fail "existing EA was replaced by a handoff"

# Case 2: attaching without naming an EA keeps the running dashboard's EA: no handoff.
start_fake_dashboard
launch_handoff
if [ -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was written although no EA was named"
fi

# Case 3: no running dashboard => no handoff, even with --ea (a fresh dashboard starts on it).
tmux_cmd kill-session -t "omar-dashboard" 2>/dev/null || true
launch_handoff --ea second
if [ -f "$handoff_file" ]; then
  fail "dashboard_handoff.json was written on cold start (no existing dashboard)"
fi
assert_backend_alive 0

echo "PASS: dashboard attach writes a handoff only for a named EA on a running dashboard"
