#!/usr/bin/env bash
set -uo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)
source_dir="$script_dir/src"
results_dir=${OMAR_TEST_RESULTS_DIR:-"$repo_root/target/topology-tests"}
# A backstop: each program's Smoke team ends it at its own deadline.
case_timeout=${OMAR_TEST_CASE_TIMEOUT_SECONDS:-3000}
invocation_timeout=${OMAR_TEST_INVOCATION_TIMEOUT_SECONDS:-300}
case_filter=${OMAR_TEST_CASE:-}
# Run the core cases with every agent on this backend instead, e.g. `cursor`.
backend=${OMAR_TEST_BACKEND:-}
core_cases=" AllTypes EffectContracts FanOutFanIn OrTriggers SameAgentSerial Timer "
fixture="$script_dir/README.md"
omar="$repo_root/target/debug/omar"
summary="$results_dir/results.tsv"

passed_cases=0
failed_cases=0
passed_checks=0
failed_checks=0
skipped_checks=0
started_at=$SECONDS

mkdir -p "$results_dir"
printf 'status\tcase\tseconds\tpassed\tfailed\tskipped\treason\tlog\n' >"$summary"

verdict() {
  local status=$1
  local elapsed=$((SECONDS - started_at))

  printf '\nTopology integration verdict: %s\n' "$status"
  printf 'Cases: %d passed, %d failed, %d total\n' \
    "$passed_cases" "$failed_cases" "$((passed_cases + failed_cases))"
  printf 'Checks: %d passed, %d failed, %d skipped\n' \
    "$passed_checks" "$failed_checks" "$skipped_checks"
  printf 'Elapsed: %ds\n' "$elapsed"
  printf 'Metrics: %s\n' "$summary"
}

build_prerequisites() {
  printf 'Building OMAR compiler and runtime...\n'
  if ! (cd "$repo_root/lang" && lake build omarc); then
    printf 'FAIL prerequisite: Lean compiler build\n' >&2
    failed_cases=$((failed_cases + 1))
    failed_checks=$((failed_checks + 1))
    printf 'FAIL\tprerequisites\t%d\t0\t1\t0\tLean compiler build\t-\n' \
      "$SECONDS" >>"$summary"
    verdict FAIL
    exit 1
  fi
  if ! (cd "$repo_root" && cargo build --quiet --bin omar); then
    printf 'FAIL prerequisite: Rust runtime build\n' >&2
    failed_cases=$((failed_cases + 1))
    failed_checks=$((failed_checks + 1))
    printf 'FAIL\tprerequisites\t%d\t0\t1\t0\tRust runtime build\t-\n' \
      "$SECONDS" >>"$summary"
    verdict FAIL
    exit 1
  fi
}

run_with_timeout() {
  local timeout=$1
  local log=$2
  local case_name=$3
  shift 3

  python3 - "$timeout" "$log" "$case_name" "$@" <<'PY'
import subprocess
import json
import os
import re
import sys
import time

timeout = int(sys.argv[1])
log_path = sys.argv[2]
case_name = sys.argv[3]
command = sys.argv[4:]
peek_agents = os.environ.get("OMAR_TEST_PEEK_AGENTS", "").lower() in {
    "1", "true", "yes"
}
agent_names = set()
if peek_agents:
    try:
        program_path = command[command.index("run") + 1]
        if program_path.endswith(".omar"):
            with open(program_path, encoding="utf-8") as source_file:
                source = source_file.read()
            agent_names = {
                match.group(1)
                for match in re.finditer(
                    r"\b([A-Za-z_][A-Za-z0-9_]*)\s*:\s*"
                    r"(?:Codex|Claude|ClaudeCode|OpenCode|Cursor|Agy|Pi|Jev)\b",
                    source,
                    re.IGNORECASE,
                )
            }
        else:
            with open(program_path, encoding="utf-8") as bytecode_file:
                bytecode = json.load(bytecode_file)
            agent_names = {
                instruction["name"]
                for instruction in bytecode["instructions"]
                if instruction["op"] == "spawn_agent"
            }
    except (OSError, ValueError, KeyError, json.JSONDecodeError):
        pass


def capture_agent_panes(elapsed):
    # OMAR runs one named tmux server per session, beside the default one.
    socket_dir = os.path.join(os.environ.get("TMUX_TMPDIR", "/tmp"), f"tmux-{os.getuid()}")
    try:
        sockets = sorted(os.listdir(socket_dir))
    except OSError:
        sockets = []
    servers = [[]] + [["-S", os.path.join(socket_dir, name)] for name in sockets]

    # Instance-qualified agents reach tmux with '.' flattened to '_', so
    # `n1.agent` is stored as `…-n1_agent`.
    names = []
    for server in servers:
        sessions = subprocess.run(
            ["tmux", *server, "list-sessions", "-F", "#{session_name}"],
            capture_output=True,
            text=True,
        )
        names += [
            (server, name) for name in sessions.stdout.splitlines()
            if name.startswith("omar-")
            and any(
                name.endswith(f"-{agent}") or name.endswith(f"_{agent}")
                for agent in agent_names
            )
        ]
    if not names:
        print(f"[{case_name}] no tmux agent sessions found", flush=True)
        return
    group = os.environ.get("GITHUB_ACTIONS") == "true"
    if group:
        print(f"::group::{case_name} agent panes at {elapsed}s", flush=True)
    else:
        print(f"[{case_name}] agent panes at {elapsed}s", flush=True)

    ansi = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
    for server, name in names:
        pane = subprocess.run(
            ["tmux", *server, "capture-pane", "-p", "-t", name, "-S", "-20"],
            capture_output=True,
            text=True,
        )
        lines = [
            ansi.sub("", line).rstrip()
            for line in pane.stdout.splitlines()
            if line.strip()
        ][-20:]
        print(f"--- {name} ---", flush=True)
        print("\n".join(lines) if lines else "(empty pane)", flush=True)

    if group:
        print("::endgroup::", flush=True)

with open(log_path, "ab") as log:
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
    started = time.monotonic()
    next_heartbeat = 30
    next_snapshot = 60
    while process.poll() is None:
        elapsed = int(time.monotonic() - started)
        if elapsed >= timeout:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            log.write(f"\nCASE TIMEOUT after {timeout}s\n".encode())
            raise SystemExit(124)
        if elapsed >= next_heartbeat:
            print(f"[{case_name}] still running ({elapsed}s elapsed)", flush=True)
            next_heartbeat += 30
        if peek_agents and elapsed >= next_snapshot:
            capture_agent_panes(elapsed)
            next_snapshot += 60
        time.sleep(1)
status = process.returncode
if status != 0 and peek_agents:
    elapsed = int(time.monotonic() - started)
    capture_agent_panes(elapsed)
raise SystemExit(status)
PY
}

# Each program checks itself: its Smoke team stops the run once the program
# has produced what it owes, and asserts at shutdown. A failed assert fails the
# run, so the checks here are the runtime's exit and the completion marker.
run_case() {
  local name=$1
  local team=$2
  shift 2

  if [[ -n $case_filter && $name != "$case_filter" ]]; then
    return
  fi
  if [[ -n $backend && $core_cases != *" $name "* ]]; then
    return
  fi

  local source="$source_dir/$name.omar"
  if [[ -n $backend ]]; then
    # A copy beside the schemas it imports, keeping the program's file name.
    local dir="$results_dir/src"
    mkdir -p "$dir"
    ln -sfn "$source_dir/schemas" "$dir/schemas"
    ln -sfn "$source_dir/scripts" "$dir/scripts"
    sed -E "s/:[[:space:]]*(Codex|ClaudeCode|Claude)\b/: $backend/g" \
      "$source" >"$dir/$name.omar"
    source="$dir/$name.omar"
  fi
  local log="$results_dir/$name.log"
  local case_started=$SECONDS
  local case_passed=0
  local case_failed=0
  local case_skipped=0
  local total_checks=2
  local status=PASS
  local reason=-

  : >"$log"

  printf '\n[%s] running with %s agents\n' "$name" "${backend:-local}"
  if run_with_timeout "$case_timeout" "$log" "$name" \
    "$omar" run "$source" --fg --replace \
    --timeout-seconds "$invocation_timeout" "$@"; then
    case_passed=$((case_passed + 1))
  else
    local runtime_status=$?
    status=FAIL
    if ((runtime_status == 124)); then
      reason="case timed out"
    elif grep -Fq 'panicked at' "$log"; then
      reason="smoke check failed"
    else
      reason="runtime exited $runtime_status"
    fi
    case_failed=$((case_failed + 1))
  fi

  # Teardown persists pane transcripts; keep them with the case results. The
  # run created its own session, named on stderr, and shut it down afterwards.
  local session_id
  session_id=$(grep -o 'Started session [^ ;]*' "$log" | awk '{print $3}' | tail -1)
  [[ -n $session_id ]] && cp -R "${OMAR_HOME:-$HOME/.omar}/sessions/$session_id/ea/0/topologies/$team/logs" "$results_dir/$name-agents" 2>/dev/null || true

  if grep -Fq "Topology '$team' completed" "$log"; then
    case_passed=$((case_passed + 1))
  else
    status=FAIL
    [[ $reason == - ]] && reason="missing completion marker"
    case_failed=$((case_failed + 1))
  fi

  local elapsed=$((SECONDS - case_started))
  passed_checks=$((passed_checks + case_passed))
  failed_checks=$((failed_checks + case_failed))
  skipped_checks=$((skipped_checks + case_skipped))

  if [[ $status == PASS ]]; then
    passed_cases=$((passed_cases + 1))
    printf 'PASS %-18s %2ds  checks %d/%d\n' \
      "$name" "$elapsed" "$case_passed" "$total_checks"
  else
    failed_cases=$((failed_cases + 1))
    printf 'FAIL %-18s %2ds  passed=%d failed=%d skipped=%d  reason=%s  log=%s\n' \
      "$name" "$elapsed" "$case_passed" "$case_failed" "$case_skipped" \
      "$reason" "$log"
  fi

  printf '%s\t%s\t%d\t%d\t%d\t%d\t%s\t%s\n' \
    "$status" "$name" "$elapsed" "$case_passed" "$case_failed" \
    "$case_skipped" "$reason" "$log" >>"$summary"
}

build_prerequisites

# Every program instantiates its teams in a main block, so ports are named
# `instance.port` and the program takes its source file's name.
run_case AllTypes AllTypes \
  --input types.bool_in=true \
  --input types.int_in=7 \
  --input types.float_in=2.5 \
  --input types.string_in=hello \
  --input "types.path_in=$fixture" \
  --input types.bytes_in=SGVsbG8= \
  --input 'types.list_in=[1,2,3]' \
  --input 'types.option_in="present"' \
  --input 'types.nested_in=[1,null,3]'

run_case BusinessLoop BusinessLoop \
  --input biz.brief='Launch a reliable self-service analytics product for small retailers'
run_case EffectContracts EffectContracts \
  --input contracts.candidate='a careful systems engineer'
run_case FanOutFanIn FanOutFanIn \
  --input fan.request='design a resilient task queue'
run_case HR HR --input "hr.resume=$fixture"
run_case HRCodex HRCodex --input "hr.resume=$fixture"
run_case OrTriggers OrTriggers \
  --input triggers.request='compare the fast and slow paths'
run_case OrderedWrites OrderedWrites \
  --input writes.request='declaration order'
# A watcher one hop and three hops from the same source. It should be invoked
# once, holding both; its Smoke team counts the firings.
run_case Depths Depths --input src.go=1
# Two enum types imported from schema files, written by a stub and read by a
# code body. The stub picks the first value each type admits.
run_case SchemaEnum SchemaEnum \
  --input review.request='route this'
# The same types inside list and option, crossing connections between two
# prompt teams.
run_case SchemaNested SchemaNested \
  --input triage.request='triage this'
# An enum input from the command line. CI also runs this with a value outside
# the enum and checks the run is refused; here only the admitted value runs.
run_case SchemaInput SchemaInput --input gate.decision=stop
run_case Recurrence Recurrence --input rec.start=0
# Three instances wired into a ring. Only the seed is supplied; the other two
# inputs come off the ring itself.
run_case Ring Ring --input n1.token=0
# A team instantiating teams: two nested Stage instances inside one Pipeline.
run_case Nested Nested --input run.brief='hierarchy'
# No --input: a timer is the only thing that starts this one.
run_case Timer Timer
# Four one-shot timers opening three rounds and a deadline, on Claude Code.
run_case Cadence Cadence
# Two deadlines at one tag on Claude Code: one generous and met, one of 1ms
# that cannot be.
# The 1ms contract is optional, so its expiry completes the tag rather than
# failing the run, and `report` shows <absent> where that write would have been.
# `main` names the program, so the completion marker is WithinTest.
run_case DeadlineProbe WithinTest
run_case SameAgentSerial SameAgentSerial \
  --input serial.request='serialize these reactions'
run_case SuperdenseTime SuperdenseTime \
  --input time.start=7
# Periodic timers never finish on their own; their Smoke teams stop them.
run_case Heartbeat Heartbeat
# Fetches from the internet; two ticks of two minutes.
run_case WorldNews WorldNewsEveryTwoMinutes
# 32 agents across eight sequential stages, on Claude Code. The slowest case.
run_case Bureau Bureau \
  --input bureau.topic='how cities can cut heat-wave deaths'
# A Jev review, its uncertain verdicts resolved by a Codex agent. Needs
# TYPESAFE_API_KEY.
run_case JevReview JevReview \
  --input checks.request='Mira Chen, founder of Northstar, welcomed guests to the Spring Summit.'
# `PortManager` is deliberately absent: its panel is answered by a web client,
# and nothing here is one. Compiled and backend-checked like every other source.

if ((passed_cases + failed_cases == 0)); then
  printf 'No topology case named %s\n' "$case_filter" >&2
  failed_cases=1
  failed_checks=1
  printf 'FAIL\t%s\t0\t0\t1\t0\tunknown case\t-\n' "$case_filter" >>"$summary"
fi

if ((failed_cases > 0)); then
  verdict FAIL
  exit 1
fi

verdict PASS
