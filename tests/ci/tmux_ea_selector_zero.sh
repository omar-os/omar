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
  echo "OMAR binary not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

server="omar-ea-selector-${RANDOM}-$$"
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

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

run_case_single_ea_no_zero() {
  cat >"$home_dir/.omar/eas.json" <<'EOF'
[
  {
    "id": 3,
    "name": "cap-x",
    "description": null,
    "created_at": 1234567890
  }
]
EOF

  tmux_cmd kill-session -t "omar-agent-3-capx-services" 2>/dev/null || true
  tmux_cmd new-session -d -s "omar-agent-3-capx-services" "sleep 9999"
  wait_for_session "omar-agent-3-capx-services"

  output="$(HOME="$home_dir" OMAR_TMUX_SERVER="$server" "$OMAR_BIN" --legacy --ea 0 list 2>&1)"
  if ! grep -q "EA 3: cap-x" <<<"$output"; then
    echo "$output" >&2
    fail "Expected --ea 0 to resolve to existing EA name cap-x"
  fi

  if ! grep -q "capx-services" <<<"$output"; then
    echo "$output" >&2
    fail "Expected output to include cap-x worker session for EA 3"
  fi

  python3 - "$home_dir/.omar/eas.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path) as handle:
    eas = json.load(handle)

if len(eas) != 1 or eas[0]["id"] != 3 or eas[0]["name"] != "cap-x":
    raise SystemExit(
        "Registry should remain a single EA with id 3/name cap-x; did --ea 0 insert default EA 0?"
    )
PY

  tmux_cmd kill-session -t "omar-agent-3-capx-services" 2>/dev/null || true
}

run_case_multi_ea_still_fails() {
  cat >"$home_dir/.omar/eas.json" <<'EOF'
[
  {
    "id": 2,
    "name": "alpha",
    "description": null,
    "created_at": 1234567890
  },
  {
    "id": 3,
    "name": "cap-x",
    "description": null,
    "created_at": 1234567890
  }
]
EOF

  tmux_cmd kill-session -t "omar-agent-3-capx-services" 2>/dev/null || true
  tmux_cmd new-session -d -s "omar-agent-3-capx-services" "sleep 9999"
  wait_for_session "omar-agent-3-capx-services"

  set +e
  output="$(HOME="$home_dir" OMAR_TMUX_SERVER="$server" "$OMAR_BIN" --legacy --ea 0 list 2>&1)"
  status=$?
  set -e

  if [ "$status" -eq 0 ]; then
    echo "$output" >&2
    fail "Expected --ea 0 to fail when multiple non-zero EAs are present"
  fi

  if ! grep -q "EA '0' not found" <<<"$output"; then
    echo "$output" >&2
    fail "Expected resolver error for numeric zero with >1 non-zero EAs"
  fi

  tmux_cmd kill-session -t "omar-agent-3-capx-services" 2>/dev/null || true
}

run_case_single_ea_no_zero
run_case_multi_ea_still_fails

echo "PASS: numeric zero EA selector does not create Default and correctly errors with ambiguity"
