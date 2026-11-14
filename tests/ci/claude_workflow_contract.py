#!/usr/bin/env python3
"""Verify installed Claude still supports what Omar-backed ultracode relies on.

Runs the real `claude -p` against a loopback mock Anthropic API (no credentials,
no model calls) with the same `--agents` omar stand-in, PreToolUse guard and
env Omar's EA launch uses. The mock plays the model: the main agent calls
Workflow, the stand-in calls spawn_agent on a stub MCP server and then
StructuredOutput. Each assertion names the Claude Code behavior that changed.
Needs `claude` on PATH and a built target/debug/omar (or OMAR_BIN).
Run with --stub-mcp <log> to act as the stub MCP server instead.
"""
import glob
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", ROOT / "target/debug/omar"))
SPAWN = "mcp__omar-hold__spawn_agent"
SCHEMA = '{type: "object", properties: {answer: {type: "integer"}}, required: ["answer"]}'
# Routes the agent through the omar stand-in the way the EA prompt teaches.
ROUTED = ("export const meta = {name: 'contract', description: 'omar routing contract'}\n"
          "const omar = (name, prompt, opts = {}) => agent(`omar: name=${name} backend= model=\\n` + prompt,\n"
          "  {...opts, agentType: 'omar', label: name})\n"
          f"return await omar('probe', 'answer 42', {{schema: {SCHEMA}}})\n")
NATIVE = ("export const meta = {name: 'contract', description: 'native agent contract'}\n"
          f"return await agent('answer 42', {{schema: {SCHEMA}}})\n")


def stub_mcp(log):
    """Minimal stdio MCP server: records tool calls, spawn_agent finishes at once."""
    tool = lambda n: {"name": n, "description": n, "inputSchema": {"type": "object", "properties": {}}}
    for line in sys.stdin:
        msg = json.loads(line)
        if "id" not in msg:
            continue
        method, result = msg["method"], {}
        if method == "initialize":
            result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                      "serverInfo": {"name": "stub", "version": "0"}}
        elif method == "tools/list":
            result = {"tools": [tool(n) for n in ("add_project", "spawn_agent", "get_task")]}
        elif method == "tools/call":
            with open(log, "a") as f:
                f.write(json.dumps(msg["params"]) + "\n")
            text = {"spawn_agent": {"status": "completed", "task_id": "t1", "result": {"answer": 42}},
                    "add_project": {"id": "p1"}}.get(msg["params"]["name"], {"status": "completed"})
            result = {"content": [{"type": "text", "text": json.dumps(text)}]}
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)


class Mock(http.server.BaseHTTPRequestHandler):
    script = ROUTED  # None: the main agent just answers
    seen = []  # every /v1/messages body, in arrival order

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if "messages" not in body:
            return self.reply({"type": "text", "text": "OK"}, body)
        Mock.seen.append(body)
        tools = {t["name"] for t in body.get("tools", [])}
        used = {b.get("name") for m in body["messages"] if m["role"] == "assistant" and isinstance(m["content"], list)
                for b in m["content"] if b.get("type") == "tool_use"}
        step = {"type": "text", "text": "OK"}
        if SPAWN in tools:  # the omar stand-in
            if SPAWN not in used:
                step = tool_use("t1", SPAWN, {"name": "probe", "project_id": "p1", "task": "answer 42", "wait_s": 5})
            elif "StructuredOutput" in tools and "StructuredOutput" not in used:
                step = tool_use("t2", "StructuredOutput", {"answer": 42})
        elif Mock.script and "Workflow" in tools and "Workflow" not in used:  # the main agent
            step = tool_use("t3", "Workflow", {"script": Mock.script})
        self.reply(step, body)

    def reply(self, block, body):
        tool_call = block["type"] == "tool_use"
        message = {"id": "msg_mock", "type": "message", "role": "assistant", "model": body.get("model", "mock"),
                   "content": [block], "stop_reason": "tool_use" if tool_call else "end_turn", "stop_sequence": None,
                   "usage": {"input_tokens": 1, "output_tokens": 1}}
        self.send_response(200)
        if not body.get("stream"):
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            return self.wfile.write(json.dumps(message).encode())
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        start = dict(message, content=[])
        delta = ({"type": "input_json_delta", "partial_json": json.dumps(block["input"])} if tool_call
                 else {"type": "text_delta", "text": block["text"]})
        empty = dict(block, input={}) if tool_call else dict(block, text="")
        events = [("message_start", {"type": "message_start", "message": start}),
                  ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": empty}),
                  ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta}),
                  ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                  ("message_delta", {"type": "message_delta", "delta": {"stop_reason": message["stop_reason"],
                                     "stop_sequence": None}, "usage": {"output_tokens": 1}}),
                  ("message_stop", {"type": "message_stop"})]
        for name, data in events:
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())


def check(condition, message):
    if not condition:
        raise SystemExit(f"FAIL: {message}")


def tool_use(id, name, input):
    return {"type": "tool_use", "id": id, "name": name, "input": input}


def main():
    cli = shutil.which("claude")
    if not cli:
        raise SystemExit("Claude CLI required for this contract test")
    if not BIN.exists():
        return print(f"SKIP: build omar first ({BIN} missing)")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="omar-workflow-contract-") as directory:
        root = Path(directory)
        stub_log = root / "stub.log"
        prompt = (ROOT / "prompts/claude-workflow-agent.md").read_text()
        agents = {"omar": {"description": "Runs one workflow agent as an Omar worker.", "prompt": prompt,
                           "model": "haiku", "tools": ["mcp__omar-hold__add_project", SPAWN,
                                                       "mcp__omar-hold__get_task"],
                           "mcpServers": [{"omar-hold": {"command": sys.executable,
                                                         "args": [str(Path(__file__).resolve()), "--stub-mcp", str(stub_log)]}}]}}
        settings = {"hooks": {"PreToolUse": [{"matcher": "Workflow", "hooks": [
            {"type": "command", "command": f"{BIN} claude-hook workflow-guard", "timeout": 5}]}]}}
        env = {"PATH": os.environ["PATH"], "HOME": str(root / "home"), "ANTHROPIC_API_KEY": "local-mock-only",
               "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
               "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_TELEMETRY": "1",
               "DISABLE_ERROR_REPORTING": "1", "API_TIMEOUT_MS": "20000"}

        def run(name, script, extra_env=None):
            """One `claude -p` turn. -p stays alive until the background workflow
            finishes (its completion notification starts a last model turn), so
            no stream-json session is needed. Returns (config dir, requests)."""
            config = root / name
            Mock.script, Mock.seen[:] = script, []
            cmd = [cli, "-p", "run the workflow", "--model", "claude-sonnet-4-6", "--agents", json.dumps(agents),
                   "--settings", json.dumps(settings), "--setting-sources", "", "--dangerously-skip-permissions"]
            process = subprocess.run(cmd, cwd=root, env=dict(env, CLAUDE_CONFIG_DIR=str(config), **(extra_env or {})),
                                     capture_output=True, text=True, timeout=120, stdin=subprocess.DEVNULL)
            check(process.returncode == 0, f"claude -p exited {process.returncode}: {process.stderr[-1500:]}")
            return config, list(Mock.seen)

        def tool_results(requests):
            return [b for r in requests for m in r["messages"] if isinstance(m["content"], list)
                    for b in m["content"] if b.get("type") == "tool_result"]

        # Run 1: a routed Workflow reaches the stub MCP server and returns typed data.
        config, seen = run("routed", ROUTED)
        workflow = next((t for r in seen for t in r.get("tools", []) if t["name"] == "Workflow"), None)
        check(workflow, "Claude Code no longer offers the Workflow tool to the model in -p mode")
        check("script" in workflow["input_schema"]["properties"],
              "Workflow's input schema lost its `script` property; Omar's guard and prompts pass scripts inline")
        standins = [r for r in seen if SPAWN in {t["name"] for t in r.get("tools", [])}]
        check(standins, f"--agents inline mcpServers no longer reach the subagent as {SPAWN} "
                        "(server not connected or trusted, or MCP tool-name format changed)")
        check(any("StructuredOutput" in {t["name"] for t in r["tools"]} for r in standins),
              "a Workflow agent() with a schema no longer gets a StructuredOutput tool on the omar subagent")
        calls = [json.loads(line) for line in stub_log.read_text().splitlines()] if stub_log.exists() else []
        check([c["name"] for c in calls] == ["spawn_agent"], f"stub MCP server saw {calls}, expected one spawn_agent")
        journals = glob.glob(f"{config}/projects/**/subagents/workflows/**/journal.jsonl", recursive=True)
        results = [json.loads(line).get("result") for j in journals for line in open(j) if '"result"' in line]
        check(results == [{"answer": 42}], f"the structured value did not reach the workflow journal: {results} "
              f"in {journals} (workflow journal layout or schema-mode result handling changed)")
        print("PASS: Workflow tool, inline agent MCP server, StructuredOutput and typed result", flush=True)

        # Run 2: an agent() that skips the omar stand-in is denied by the PreToolUse hook.
        stub_log.unlink()
        _, seen = run("native", NATIVE)
        denied = [r for r in tool_results(seen) if r["tool_use_id"] == "t3"]
        text = json.dumps(denied)
        check(denied and denied[0].get("is_error") and "agentType" in text,
              f"the workflow-guard PreToolUse deny reason no longer reaches the model: {text[:800]}")
        check("Workflow launched" not in text and not stub_log.exists(),
              "a denied Workflow call still launched (PreToolUse deny no longer blocks Workflow)")
        print("PASS: PreToolUse hook denies a Workflow agent() that bypasses omar", flush=True)

        # Run 3: CLAUDE_CODE_SKIP_PROMPT_HISTORY=1 keeps a worker out of the Claude app sidebar.
        config, _ = run("history", None, {"CLAUDE_CODE_SKIP_PROMPT_HISTORY": "1"})
        written = glob.glob(f"{config}/projects/**/*.jsonl", recursive=True)
        check(not written, f"CLAUDE_CODE_SKIP_PROMPT_HISTORY=1 no longer prevents transcripts: {written}")
        control, _ = run("history-control", None)  # proves the check above can see a transcript
        check(glob.glob(f"{control}/projects/**/*.jsonl", recursive=True),
              "without CLAUDE_CODE_SKIP_PROMPT_HISTORY Claude wrote no transcript, so the history check proves nothing")
        print("PASS: CLAUDE_CODE_SKIP_PROMPT_HISTORY=1 writes no transcript", flush=True)
    server.shutdown()


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--stub-mcp":
        stub_mcp(sys.argv[2])
    else:
        main()
