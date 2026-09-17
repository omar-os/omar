#!/usr/bin/env bash
# Behavioral Pi integration test: a real Pi root must use OMAR's extension to
# spawn two tracked Pi leaves that load OMAR tools and accept their tasks.
# By default a local provider drives real Pi deterministically without secrets.
# Only the opt-in live root allows an authentication-related skip.

set -euo pipefail

OMAR_BIN="${OMAR_BIN:-target/debug/omar}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ORIGINAL_HOME="${HOME}"
PI_VERSION="0.85.1"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi
if ! command -v npx >/dev/null 2>&1; then
  echo "npx is required" >&2
  exit 1
fi
if [ ! -x "$OMAR_BIN" ]; then
  echo "OMAR binary not found or not executable: $OMAR_BIN" >&2
  exit 1
fi
OMAR_BIN="$(cd "$(dirname "$OMAR_BIN")" && pwd)/$(basename "$OMAR_BIN")"

auth_source="$ORIGINAL_HOME/.pi/agent/auth.json"
if [ "${PI_E2E_LIVE:-0}" = "1" ] && [ ! -s "$auth_source" ] && [ -z "${OPENAI_API_KEY:-}" ] && [ -z "${ANTHROPIC_API_KEY:-}" ]; then
  echo "SKIP: Pi credentials unavailable"
  exit 0
fi

test_root="$(mktemp -d)"
server="omar-pi-tree-${RANDOM}-$$"
export HOME="$test_root/home"
export OMAR_DIR="$HOME/.omar"
export OMAR_EA_ID=0
export OMAR_TMUX_SERVER="$server"
export OMAR_BINARY="$OMAR_BIN"
unset OMAR_MCP_CONTEXT_FILE
export PI_CODING_AGENT_DIR="$HOME/.pi/agent"
export PI_CODING_AGENT_SESSION_DIR="$HOME/.pi/agent/sessions"
export PATH="$HOME/bin:$PATH"

cleanup() {
  tmux -L "$server" kill-server >/dev/null 2>&1 || true
  rm -rf "$test_root"
}
trap cleanup EXIT

mkdir -p "$HOME/bin" "$PI_CODING_AGENT_DIR" "$PI_CODING_AGENT_SESSION_DIR" "$OMAR_DIR"
if [ "${PI_E2E_LIVE:-0}" = "1" ] && [ -s "$auth_source" ]; then
  cp "$auth_source" "$PI_CODING_AGENT_DIR/auth.json"
fi
# Do not inherit user packages, models, trust decisions, or credentials in CI.
# The local provider is available to leaves as well as the RPC root; OMAR must
# still supply its own extension through the normal backend launch command.
python3 - "$PI_CODING_AGENT_DIR/settings.json" "$REPO_ROOT" <<'PYSETTINGS'
import json, sys
from pathlib import Path
settings = {
    "defaultProvider": "omar-pi-e2e",
    "defaultModel": "tree",
    "extensions": [str(Path(sys.argv[2]) / "bridges/pi/test/fixtures/deterministic-provider.mjs")],
}
Path(sys.argv[1]).write_text(json.dumps(settings), encoding="utf-8")
PYSETTINGS

# OMAR resolves the leaf backend from PATH. Pin it to the same Pi version as
# the root so the test also exercises OMAR's normal Pi launch path.
printf '%s\n' '#!/usr/bin/env bash' \
  "exec npx --yes --package=@earendil-works/pi-coding-agent@$PI_VERSION -- pi \"\$@\"" \
  >"$HOME/bin/pi"
chmod +x "$HOME/bin/pi"

# Initialize EA-scoped state, then use the same MCP protocol the extension uses.
"$OMAR_BIN" list >/dev/null 2>&1 || true

mcp_call() {
  local tool="$1" args="$2"
  python3 - "$OMAR_BIN" "$tool" "$args" <<'PY'
import json, subprocess, sys

binary, tool, arguments = sys.argv[1:]
requests = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "pi-tree-e2e", "version": "1"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": json.loads(arguments)}},
]
payload = "".join(json.dumps(item) + "\n" for item in requests)
proc = subprocess.run([binary, "mcp-server"], input=payload, text=True, capture_output=True, timeout=30)
if proc.returncode:
    raise SystemExit(proc.stderr or f"mcp-server exited {proc.returncode}")
for line in proc.stdout.splitlines():
    response = json.loads(line)
    if response.get("id") == 2:
        if "error" in response:
            raise SystemExit(response["error"])
        print(json.dumps(response["result"]))
        break
else:
    raise SystemExit(f"No tools/call response: {proc.stdout}")
PY
}

project_response="$(mcp_call add_project '{"name":"Pi binary tree E2E"}')"
project_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["structuredContent"]["project_id"])' <<<"$project_response")"
root_log="$test_root/pi-root.jsonl"
prompt="You are the root of a binary-tree integration test. You MUST call the tool omar_spawn_agent exactly twice and do no other work. First call it with name leaf1, project_id $project_id, parent ea, backend pi, and task 'Pi binary tree leaf one'. Second call it with name leaf2, project_id $project_id, parent ea, backend pi, and task 'Pi binary tree leaf two'. Do not use bash or merely describe the calls. After both tool calls succeed, reply with TREE_CREATED."

# Drive Pi through its documented JSONL RPC protocol and retain every event so
# the assertions prove real Pi invoked the dynamically registered tools.
set +e
PI_PROMPT="$prompt" PI_PROJECT_ID="$project_id" PI_ROOT_LOG="$root_log" PI_EXTENSION="$REPO_ROOT/bridges/pi/index.js" python3 - <<'PY'
import json, os, queue, re, subprocess, sys, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

child_env = os.environ.copy()
# The local provider stays alive until both interactive leaves finish.
project_id = int(os.environ["PI_PROJECT_ID"])
calls = [
    ("call_leaf1", "fc_leaf1", {"name": "leaf1", "project_id": project_id, "parent": "ea", "backend": "pi", "task": "Pi binary tree leaf one"}),
    ("call_leaf2", "fc_leaf2", {"name": "leaf2", "project_id": project_id, "parent": "ea", "backend": "pi", "task": "Pi binary tree leaf two"}),
]

class DeterministicResponses(BaseHTTPRequestHandler):
    def log_message(self, _format, *_args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        request = json.loads(self.rfile.read(length) or b"{}")
        has_results = any(item.get("type") == "function_call_output" for item in request.get("input", []) if isinstance(item, dict))
        # Leaf identity is supplied by OMAR's delivered task, not the root's
        # prompt (which also mentions both leaf names).
        inputs = json.dumps(request.get("input", []))
        leaf = next((name for name in ("leaf1", "leaf2") if f"YOUR NAME: {name}" in inputs), None)
        tool_calls = calls
        done_text = "TREE_CREATED"
        if leaf:
            tool_calls = [(f"call_{leaf}_ready", f"fc_{leaf}_ready", {"name": leaf, "status": "PI_LEAF_READY"})]
            done_text = "PI_LEAF_READY"
        tool_name = "omar_update_agent_status" if leaf else "omar_spawn_agent"
        available = {tool.get("name") for tool in request.get("tools", [])}
        if tool_name not in available:
            self.send_error(400, f"Pi did not load {tool_name}")
            return
        response_id = "resp_done" if has_results else "resp_tools"
        output = []
        if has_results:
            output.append({"id": "msg_done", "type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": done_text, "annotations": []}]})
        else:
            for call_id, item_id, arguments in tool_calls:
                output.append({"id": item_id, "type": "function_call", "status": "completed", "call_id": call_id, "name": tool_name, "arguments": json.dumps(arguments)})
        response = {"id": response_id, "object": "response", "created_at": int(time.time()), "status": "completed", "model": "gpt-5-mini", "output": output, "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2, "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}}}
        events = [{"type": "response.created", "response": {**response, "status": "in_progress", "output": []}}]
        for index, item in enumerate(output):
            events.append({"type": "response.output_item.added", "output_index": index, "item": {**item, "status": "in_progress"}})
            if item["type"] == "function_call":
                events.append({"type": "response.function_call_arguments.done", "output_index": index, "item_id": item["id"], "arguments": item["arguments"]})
            elif item["type"] == "message":
                events.append({"type": "response.output_text.done", "output_index": index, "item_id": item["id"], "content_index": 0, "text": done_text})
            events.append({"type": "response.output_item.done", "output_index": index, "item": item})
        events.append({"type": "response.completed", "response": response})
        body = "".join(f"data: {json.dumps(event)}\n\n" for event in events) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body.encode())))
        self.end_headers()
        self.wfile.write(body.encode())

server = ThreadingHTTPServer(("127.0.0.1", 0), DeterministicResponses)
threading.Thread(target=server.serve_forever, daemon=True).start()
child_env["PI_E2E_BASE_URL"] = f"http://127.0.0.1:{server.server_port}/v1"
provider = os.environ.get("PI_E2E_PROVIDER", "openai") if os.environ.get("PI_E2E_LIVE") == "1" else "omar-pi-e2e"
model = os.environ.get("PI_E2E_MODEL", "gpt-5-mini") if os.environ.get("PI_E2E_LIVE") == "1" else "tree"
cmd = ["pi", "--mode", "rpc", "--no-session", "--approve", "--provider", provider, "--model", model, "-e", os.environ["PI_EXTENSION"]]
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, env=child_env)
assert proc.stdin and proc.stdout and proc.stderr
proc.stdin.write(json.dumps({"id": "tree", "type": "prompt", "message": os.environ["PI_PROMPT"]}) + "\n")
proc.stdin.flush()
# A queue drains complete lines, including lines already buffered by Python's
# text reader. select()+readline() can strand the final agent_settled event.
output_lines = queue.Queue()
def read_stdout():
    for line in proc.stdout:
        output_lines.put(line)
    output_lines.put(None)
threading.Thread(target=read_stdout, daemon=True).start()
deadline = time.monotonic() + 300
events = []
settled = False
leaf_error = None

def check_leaves():
    """A session alone can be a dead pane, a shell, or a blocked trust dialog."""
    tmux = ["tmux", "-L", os.environ["OMAR_TMUX_SERVER"]]
    pending = {"leaf1", "leaf2"}
    deadline = time.monotonic() + 90
    snapshots = {}
    while pending and time.monotonic() < deadline:
        for leaf in sorted(pending):
            session = f"omar-agent-0-{leaf}"
            pane = subprocess.check_output(tmux + ["display-message", "-p", "-t", session, "#{pane_dead}|#{pane_pid}|#{pane_current_command}"], text=True).strip()
            text = subprocess.check_output(tmux + ["capture-pane", "-p", "-S", "-", "-t", session], text=True)
            snapshots[leaf] = f"{pane}\n{text}"
            dead, pid, command = pane.split("|", 2)
            if dead != "0":
                raise AssertionError(f"{leaf}: dead pane\n{text}")
            os.kill(int(pid), 0)
            # Status is written only by a real leaf calling its OMAR tool.
            # Thus a banner, echoed task, or shell left after Pi exits cannot pass.
            status = Path(os.environ["OMAR_DIR"]) / "ea/0/status" / f"{session}.md"
            ready = status.exists() and status.read_text().strip() == "PI_LEAF_READY"
            loaded = re.search(r"OMAR: loaded [1-9][0-9]* MCP tools", text)
            if ready and loaded and "pi v" in text:
                screen = subprocess.check_output(tmux + ["capture-pane", "-p", "-t", session], text=True)
                assert not re.search(r"trust this|trust project folder|trust.*directory|project trust|project is not trusted|OMAR MCP unavailable", screen, re.I), screen
                assert command not in ("bash", "zsh", "sh", "fish"), pane
                print(f"PASS: {leaf}: pane alive ({command}), Pi ready, OMAR tools loaded, leaf called update_agent_status", flush=True)
                pending.remove(leaf)
        if pending:
            time.sleep(0.1)
    if pending:
        raise AssertionError("Leaves did not pass trust/readiness/tool execution: " + ", ".join(sorted(pending)) + "\n" + "\n".join(snapshots.values()))

try:
    while time.monotonic() < deadline:
        try:
            line = output_lines.get(timeout=1)
        except queue.Empty:
            if proc.poll() is not None:
                break
            continue
        if line is None:
            break
        events.append(line)
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "agent_settled":
            settled = True
            break
    if settled:
        try:
            check_leaves()
        except (AssertionError, OSError, subprocess.SubprocessError) as error:
            leaf_error = str(error)
finally:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    server.shutdown()
stderr = proc.stderr.read()
with open(os.environ["PI_ROOT_LOG"], "w", encoding="utf-8") as handle:
    handle.writelines(events)
    if stderr:
        handle.write(json.dumps({"type": "stderr", "text": stderr}) + "\n")
combined = "".join(events) + stderr
auth_markers = ("authentication", "not logged in", "api key", "unauthorized", "login required", "credit balance", "billing")
if os.environ.get("PI_E2E_LIVE") == "1" and any(marker in combined.lower() for marker in auth_markers):
    print("SKIP: Pi credentials are present but not usable", file=sys.stderr)
    raise SystemExit(77)
if leaf_error:
    raise SystemExit(leaf_error)
if not settled:
    print(combined[-8000:], file=sys.stderr)
    raise SystemExit("Pi RPC did not settle within 300 seconds")
PY
rpc_status=$?
set -e
if [ "$rpc_status" -eq 77 ]; then
  exit 0
fi
if [ "$rpc_status" -ne 0 ]; then
  exit "$rpc_status"
fi

python3 - "$root_log" <<'PY'
import json, sys

calls = []
completed = []
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("type") == "tool_execution_start" and event.get("toolName") == "omar_spawn_agent":
        calls.append(event.get("args", {}))
    if event.get("type") == "tool_execution_end" and event.get("toolName") == "omar_spawn_agent":
        completed.append(event)
if len(completed) != 2 or any(event.get("isError") for event in completed):
    raise SystemExit(f"Expected two successful Pi spawn tool completions; got {completed}")
if [call.get("name") for call in calls] != ["leaf1", "leaf2"]:
    tail = open(sys.argv[1], encoding="utf-8").read()[-12000:]
    raise SystemExit(f"Expected Pi to call omar_spawn_agent for leaf1 and leaf2; got {calls}\nPi event tail:\n{tail}")
PY

state_dir="$OMAR_DIR/ea/0"
python3 - "$state_dir" "$project_id" <<'PY'
import json, os, sys

state, project_id = sys.argv[1], int(sys.argv[2])
def load(name):
    with open(os.path.join(state, name), encoding="utf-8") as handle:
        return json.load(handle)

tasks = load("worker_tasks.json")
parents = load("agent_parents.json")
projects = load("agent_projects.json")
expected_tasks = {"omar-agent-0-leaf1": "Pi binary tree leaf one", "omar-agent-0-leaf2": "Pi binary tree leaf two"}
for session, task in expected_tasks.items():
    assert tasks.get(session) == task, (session, tasks)
    assert parents.get(session) == "omar-agent-ea-0", (session, parents)
    assert projects.get(session) == project_id, (session, projects)
PY

sessions="$(tmux -L "$server" list-sessions -F '#{session_name}')"
grep -qx 'omar-agent-0-leaf1' <<<"$sessions"
grep -qx 'omar-agent-0-leaf2' <<<"$sessions"

echo "PASS: real Pi root called OMAR tools; both tracked Pi leaves are alive, ready, and executed OMAR tools"
