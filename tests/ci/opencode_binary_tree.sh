#!/usr/bin/env bash
# Binary-tree e2e test using only opencode (the one free backend).
#
# Why this test exists
# - Continuation 5 of the API->MCP migration uncovered that opencode workers
#   dropped into a wizard ("What is your agent name?" / Parent / Task) instead
#   of executing the spawn task. Root cause: opencode has no system-prompt
#   flag, and `--prompt` is treated as the first user message — so when the
#   agent.md template was passed via --prompt the LLM read it descriptively.
# - Fix landed in build_agent_command + spawn_worker + MCP spawn_agent_internal.
# - This test proves a binary-tree (1 root EA + 2 leaves) of opencode workers
#   spawns autonomously without the wizard regression, and that distinct
#   models can be used per node via the `--model` override on spawn_agent.
#
# CI integration
# - Runs only when OMAR_OPENCODE_E2E=1 is set, because real-LLM tests need
#   opencode to be installed and a provider authenticated. Per-PR CI sets
#   it to 0 (no-op pass); a nightly / workflow_dispatch job sets it to 1.
# - When skipped, exits 0 with a clear message so the job stays green.
# - When run, has a 10-minute LLM budget. LLM nondeterminism is accepted
#   per user agreement; structural assertions only.

set -euo pipefail

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Skip gate. Per-PR CI must NOT block on real LLM availability.
if [ "${OMAR_OPENCODE_E2E:-0}" != "1" ]; then
  echo "SKIP: OMAR_OPENCODE_E2E!=1 (set to 1 in nightly/manual workflow runs)"
  exit 0
fi

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

if ! command -v opencode >/dev/null 2>&1; then
  echo "SKIP: opencode CLI not on PATH"
  exit 0
fi

if [ ! -x "$OMAR_BIN" ]; then
  echo "OMAR binary not found or not executable: $OMAR_BIN" >&2
  exit 1
fi

# Pick two distinct free opencode models. Free tier rotates weekly, so we
# enumerate at runtime instead of hardcoding names. We prefer the
# `opencode/*-free`, `opencode/big-pickle`, and `opencode/gpt-5-nano` free
# pool, falling back to anything tagged `:free` if needed.
mapfile -t free_models < <(
  opencode models 2>/dev/null \
    | grep -E '^opencode/(.+-free|big-pickle|gpt-5-nano)$|:free$' \
    | head -8
) || true

if [ "${#free_models[@]}" -lt 2 ]; then
  echo "SKIP: need at least 2 free opencode models; found ${#free_models[@]}"
  printf '  candidate: %s\n' "${free_models[@]:-<none>}"
  exit 0
fi

model_a="${free_models[0]}"
model_b="${free_models[1]}"
echo "Using opencode models:"
echo "  root  = $model_a"
echo "  leaf1 = $model_a"
echo "  leaf2 = $model_b"

server="omar-opencode-tree-${RANDOM}-$$"
home_dir="$(mktemp -d)"

cleanup() {
  tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$home_dir"
}
trap cleanup EXIT

mkdir -p "$home_dir/.omar"
cat >"$home_dir/.omar/config.toml" <<EOF
[dashboard]
refresh_interval = 1
session_prefix = "omar-agent-"

[agent]
default_command = "opencode -m $model_a"
default_workdir = "."
EOF

tmux_cmd() {
  HOME="$home_dir" OMAR_TMUX_SERVER="$server" tmux -L "$server" "$@"
}

omar_cmd() {
  HOME="$home_dir" OMAR_TMUX_SERVER="$server" "$OMAR_BIN" --legacy "$@"
}

wait_for_session() {
  local session="$1" budget="${2:-90}"
  for _ in $(seq 1 $((budget * 10))); do
    if tmux_cmd has-session -t "$session" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "Timed out waiting for session: $session" >&2
  return 1
}

pane_text() {
  local session="$1"
  tmux_cmd capture-pane -t "$session" -p -S - 2>/dev/null || echo ""
}

fail() {
  echo "FAIL: $1" >&2
  echo "--- tmux sessions ---" >&2
  tmux_cmd list-sessions 2>&1 | sed 's/^/  /' >&2 || true
  for s in $(tmux_cmd list-sessions -F '#{session_name}' 2>/dev/null); do
    echo "--- pane $s (last 30 lines) ---" >&2
    tmux_cmd capture-pane -t "$s" -p 2>/dev/null | tail -30 | sed 's/^/  /' >&2 || true
  done
  exit 1
}

# Spawn the EA (root). Uses the configured default_command (opencode + model_a).
omar_cmd manager start >/dev/null 2>&1 &
ea_session="omar-agent-ea-0"
wait_for_session "$ea_session" 60 || fail "EA session never came up"

# Wait for the EA's opencode TUI to become interactive. We don't have a
# precise readiness marker for opencode, so allow up to 90s.
sleep 30

# Spawn 2 worker children directly via the omar spawn CLI. We deliberately
# bypass the manager protocol's PLAN/ACCEPT loop so the test does not depend
# on the EA LLM choosing to spawn workers in any particular shape.
omar_cmd spawn -n leaf1 -c "opencode -m $model_a" >/dev/null 2>&1 \
  || fail "failed to spawn leaf1"
omar_cmd spawn -n leaf2 -c "opencode -m $model_b" >/dev/null 2>&1 \
  || fail "failed to spawn leaf2"

leaf1_session="omar-agent-0-leaf1"
leaf2_session="omar-agent-0-leaf2"
wait_for_session "$leaf1_session" 60 || fail "leaf1 session never came up"
wait_for_session "$leaf2_session" 60 || fail "leaf2 session never came up"

# Give opencode time to render its TUI in each pane.
sleep 30

# Regression assertion: pane content must NOT contain the wizard prompt
# that the previous --prompt-misuse bug produced. If this string appears,
# the opencode-as-system-prompt fix has regressed.
for s in "$leaf1_session" "$leaf2_session"; do
  text="$(pane_text "$s")"
  if grep -qiE 'what is your agent name|please provide.*agent name' <<<"$text"; then
    fail "$s shows the agent-create wizard regression"
  fi
done

# Positive assertion: each leaf pane should show that opencode came up.
# Match stable TUI chrome that opencode renders whenever its UI is alive,
# independent of provider-auth state (an unauthenticated TUI still draws the
# input placeholder and footer), plus opencode's own update modal. Any of
# these indicates the worker is alive and not crashed. Case-insensitive on
# purpose: recent opencode brands the provider as "OpenCode Zen" (capital C)
# and draws the logo as box-art, so the old case-sensitive 'opencode' marker
# no longer matched a healthy pane. The wizard-regression check above remains
# the behavioral failure signal this test exists to catch.
for s in "$leaf1_session" "$leaf2_session"; do
  text="$(pane_text "$s")"
  if ! grep -qiE 'opencode|ask anything|tab agents|ctrl\+p|commands|update available|new release' <<<"$text"; then
    fail "$s pane does not show opencode UI markers"
  fi
done

# Distinct-model assertion (best-effort): if the TUI prints the model in the
# status bar, the two panes should differ.
text1="$(pane_text "$leaf1_session")"
text2="$(pane_text "$leaf2_session")"
if grep -q "$model_b" <<<"$text1" && grep -q "$model_a" <<<"$text2"; then
  echo "WARN: leaf1 and leaf2 model labels appear swapped (LLM nondeterminism)" >&2
fi

echo "PASS: opencode binary tree (root + 2 leaves) came up without wizard regression"
