#!/usr/bin/env python3
"""Production MCP/scheduler regression, real isolated tmux, mock inference transport.

No provider credentials or model calls. Parent uses the OpenCode HTTP contract;
children are terminal processes driven via the same MCP tools a model uses.
Exercises durable completion, complete process restart, redelivery without a
human prompt, consumption, retirement, and Claude post-compaction hooks.
"""
import http.server
import json
import os
from pathlib import Path
import queue
import selectors
import subprocess
import tempfile
import threading
import time
import uuid

BIN = Path(os.environ.get("OMAR_BIN", "target/debug/omar")).resolve()
receipts = queue.Queue()


class Backend(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        receipts.put((self.path, body))
        self.send_response(204)
        self.end_headers()


class MCP:
    def __init__(self, context, env):
        self.proc = subprocess.Popen([str(BIN), "mcp-server", "--context-file", str(context)],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, env=env)
        self.seq = 0

    def call(self, tool_name, **arguments):
        self.seq += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.seq,
                              "method": "tools/call", "params": {"name": tool_name, "arguments": arguments}}) + "\n")
        self.proc.stdin.flush()
        with selectors.DefaultSelector() as selector:
            selector.register(self.proc.stdout, selectors.EVENT_READ)
            assert selector.select(15), f"MCP timed out: {tool_name}"
        line = self.proc.stdout.readline()
        assert line, f"MCP exited: {tool_name}"
        result = json.loads(line)["result"]
        assert not result.get("isError"), result
        return result["structuredContent"]

    def stop(self):
        self.proc.kill()
        self.proc.wait(timeout=5)
        self.proc.stdin.close()
        self.proc.stdout.close()


def main():
    server_name = "omar-supervision-" + uuid.uuid4().hex[:12]
    backend = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    threading.Thread(target=backend.serve_forever, daemon=True).start()
    processes = []
    env = dict(os.environ, OMAR_TMUX_SERVER=server_name)
    # Do not inherit the invoking pane's identity into this isolated test.
    env.pop("OMAR_AGENT_NAME", None)
    env.pop("OMAR_EA_ID", None)
    env.pop("OMAR_MCP_CONTEXT_FILE", None)

    def tmux(*args):
        return subprocess.run(["tmux", "-L", server_name, *args], check=True, capture_output=True, text=True)

    try:
        with tempfile.TemporaryDirectory(prefix="omar-supervision-") as directory:
            root = Path(directory)
            context = {"omar_dir": str(root), "ea_id": 0, "session_prefix": "reg-",
                       "default_command": "cat", "default_workdir": str(root), "health_idle_warning": 15,
                       "tmux_server": server_name, "topology": None, "serve": None}
            parent_file = root / "parent.json"
            parent_file.write_text(json.dumps(context))
            tmux("new-session", "-d", "-s", "reg-ea-0", "cat")
            tmux("set-environment", "-t", "reg-ea-0", "OMAR_BACKEND", "opencode")
            tmux("set-environment", "-t", "reg-ea-0", "OMAR_DELIVERY",
                 f"opencode:{backend.server_port}:ses_parent")
            parent = MCP(parent_file, env)
            processes.append(parent)
            project = parent.call("add_project", name="supervision regression")["project_id"]
            child = parent.call("spawn_agent", name="child", project_id=project,
                                task="Produce durable result", command="cat", supervise=True)
            task_id = child["task_id"]
            worker_file = root / "child.json"
            worker_file.write_text(json.dumps(dict(context, agent_name="child")))
            shared_file = root / "shared-backend.json"
            shared_file.write_text(json.dumps(context))
            worker_env = dict(env, OMAR_EA_ID="0", OMAR_AGENT_NAME="child", OMAR_MCP_CONTEXT_FILE=str(worker_file))
            worker = MCP(shared_file, worker_env)
            processes.append(worker)
            state = worker.call("coordination_state")
            assert state["agent"] == "child", "shared backend config lost worker identity"
            assert state["tasks"][0]["task_id"] == task_id
            result = {"artifact": "proof.txt", "validation": "complete", "detail": "λ" * 20000}
            worker.call("finish_task", task_id=task_id, status="completed", result=result)
            endpoint, body = receipts.get(timeout=15)
            assert endpoint == "/session/ses_parent/prompt_async", endpoint
            assert "noReply" not in body
            assert task_id in body["parts"][0]["text"]
            assert len(body["parts"][0]["text"]) < 5000, "large result leaked into wake context"
            print("PASS: worker completion automatically starts parent inference; wake context bounded", flush=True)

            # Lose every model/MCP process after transport acceptance, before ack.
            for process in processes:
                process.stop()
            processes.clear()
            while not receipts.empty():
                receipts.get_nowait()
            parent = MCP(parent_file, env)
            processes.append(parent)
            # No MCP request or user prompt triggers this retry. It comes from
            # the persisted obligation and newly elected runtime alone.
            endpoint, body = receipts.get(timeout=75)
            assert task_id in body["parts"][0]["text"]
            print("PASS: full process restart redelivers an accepted but unconsumed result autonomously", flush=True)

            # Native compaction hook reads disk, not a transcript or model summary.
            hook = subprocess.run([str(BIN), "agent-hook", "--context-file", str(parent_file)],
                                  input=json.dumps({"hook_event_name": "SessionStart", "source": "compact"}),
                                  capture_output=True, text=True, env=env, check=True)
            assert task_id in json.loads(hook.stdout)["hookSpecificOutput"]["additionalContext"]
            stop = subprocess.run([str(BIN), "agent-hook", "--context-file", str(parent_file)],
                                  input=json.dumps({"hook_event_name": "Stop", "stop_hook_active": False}),
                                  capture_output=True, text=True, env=env, check=True)
            assert json.loads(stop.stdout)["decision"] == "block"
            text, offset = "", 0
            while True:
                page = parent.call("get_task", task_id=task_id, offset=offset)
                text += page["content"]
                if page["next_offset"] is None:
                    break
                offset = page["next_offset"]
            assert json.loads(text)["result"] == result
            parent.call("acknowledge_task", task_id=task_id, result_revision=page["result_revision"])
            parent.call("kill_agent", name="child")
            parent.call("complete_project", project_id=project)
            assert parent.call("coordination_state")["tasks"] == []
            print("PASS: compact/stop hooks restore obligations; full result pages survive; ack and retirement settle work", flush=True)
    finally:
        for process in processes:
            process.stop()
        backend.shutdown()
        backend.server_close()
        subprocess.run(["tmux", "-L", server_name, "kill-server"], capture_output=True)


if __name__ == "__main__":
    main()
