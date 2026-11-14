#!/usr/bin/env python3
"""No-model demonstration of the production OMAR MCP scheduler and HTTP delivery.

Usage: python3 demo.py --mode completion|idle --binary /path/to/target/debug/omar
All state/processes use a disposable directory and a dedicated tmux -L socket.
The local HTTP sink accepts the OpenCode prompt_async contract; no inference runs.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import selectors
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

BASE = Path(__file__).resolve().parent


class MCP:
    def __init__(self, binary, context, env, log):
        self.proc = subprocess.Popen([str(binary), "mcp-server", "--context-file", str(context)],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=log, text=True, env=env)
        self.seq = 0

    def call(self, tool_name, **arguments):
        self.seq += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.seq,
            "method": "tools/call", "params": {"name": tool_name, "arguments": arguments}}) + "\n")
        self.proc.stdin.flush()
        with selectors.DefaultSelector() as selector:
            selector.register(self.proc.stdout, selectors.EVENT_READ)
            assert selector.select(20), f"MCP timeout: {tool_name}"
        response = json.loads(self.proc.stdout.readline())["result"]
        assert not response.get("isError"), response
        return response["structuredContent"]

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=5)
        self.proc.stdin.close()
        self.proc.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["completion", "idle"], required=True)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    commit = subprocess.check_output(["git", "-C", str(binary.parent.parent.parent),
                                      "rev-parse", "HEAD"], text=True).strip()
    socket = "omar-demo-" + uuid.uuid4().hex[:12]
    root = Path(tempfile.mkdtemp(prefix=f"{args.mode}-state-", dir=BASE))
    receipts = queue.Queue()
    received = []
    started = time.monotonic()
    processes = []
    log = (BASE / f"{args.mode}-runtime.log").open("w")
    evidence = {"mode": args.mode, "commit": commit, "binary": str(binary),
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "tmux_socket": socket, "receipts": received}

    def say(message):
        print(f"[{time.monotonic() - started:6.1f}s] {message}", flush=True)

    class Sink(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            item = {"seconds": round(time.monotonic() - started, 3),
                    "path": self.path, "body": body}
            received.append(item)
            self.send_response(204)
            self.end_headers()
            receipts.put(item)

    backend = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Sink)
    threading.Thread(target=backend.serve_forever, daemon=True).start()
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("OMAR_") and key not in ("TMUX", "TMUX_PANE")}
    env.update(OMAR_HOME=str(root / "home"), OMAR_TMUX_SERVER=socket,
               TERM="xterm-256color")

    def tmux(*arguments, check=True):
        return subprocess.run(["tmux", "-L", socket, *arguments], env=env,
                              capture_output=True, text=True, check=check)

    def transport(session, receiver):
        tmux("set-environment", "-t", session, "OMAR_BACKEND", "opencode")
        tmux("set-environment", "-t", session, "OMAR_DELIVERY",
             f"opencode:{backend.server_port}:ses_{receiver}")

    def receive(receiver, contains):
        item = receipts.get(timeout=20)
        assert item["path"] == f"/session/ses_{receiver}/prompt_async", item
        text = item["body"]["parts"][0]["text"]
        assert contains in text, text
        assert "noReply" not in item["body"], item
        return item

    def quiet(seconds):
        try:
            unexpected = receipts.get(timeout=seconds)
            raise AssertionError(f"unexpected notification: {unexpected}")
        except queue.Empty:
            pass

    try:
        print("\033[2J\033[H", end="")
        print("OMAR | " + ("Event-driven parent notices" if args.mode == "completion"
                            else "One-shot idle escalation"), flush=True)
        print(f"Commit {commit[:7]} | production target/debug/omar", flush=True)
        print("Dedicated tmux socket + disposable OMAR home; no paid model calls.", flush=True)
        print("Shell child; local HTTP receipt sink replaces inference only.", flush=True)
        print("All waits use real wall-clock time; playback waits are sped up.\n", flush=True)
        (root / "config.toml").write_text("[health]\nidle_critical = 2\n")
        context = {"omar_dir": str(root), "ea_id": 0, "session_prefix": "demo-",
                   "default_command": "cat", "default_workdir": str(root),
                   "health_idle_warning": 15, "tmux_server": socket,
                   "topology": None, "serve": None}
        parent_file = root / "parent.json"
        parent_file.write_text(json.dumps(context))
        tmux("new-session", "-d", "-s", "demo-ea-0", "cat")
        transport("demo-ea-0", "parent")
        parent = MCP(binary, parent_file, env, log)
        processes.append(parent)
        project = parent.call("add_project", name="isolated notification demo")["project_id"]
        parent.call("log_justification", agent_name="ea", action="spawn_agent",
                    justification="Demonstrate event-driven supervision with a no-model shell child")
        if args.mode == "completion":
            shell = root / "working.sh"
            shell.write_text("#!/bin/sh\nwhile :; do echo working; sleep 10; done\n")
            command = f"/bin/sh {shell}"
        else:
            command = "cat"
        task = parent.call("spawn_agent", name="child", project_id=project,
                           parent="ea", task="Return a durable result", command=command,
                           supervise=True)
        task_id = task["task_id"]
        say("spawn_agent: tracked shell child started; parent = ea")
        if args.mode == "completion":
            say("Watching 130 s of child work (> two former 60 s check-ins).")
            quiet(130)
            evidence["working_observation_seconds"] = 130
            say("PASS: child still running; parent HTTP notices = 0")
            assert parent.call("get_task", task_id=task_id)["status"] == "running"
            child_file = root / "child.json"
            child_file.write_text(json.dumps(dict(context, agent_name="child")))
            child_env = dict(env, OMAR_EA_ID="0", OMAR_AGENT_NAME="child",
                             OMAR_MCP_CONTEXT_FILE=str(child_file))
            child = MCP(binary, child_file, child_env, log)
            processes.append(child)
            child.call("finish_task", task_id=task_id, status="completed",
                       result={"validation": "shell work complete"})
            say("finish_task(completed): durable result written")
            receive("parent", task_id)
            say("PASS: parent received exactly 1 completion notice")
            say("Result stays unacknowledged; watching another 65 s (sped up).")
            quiet(65)
            evidence["duplicate_observation_seconds"] = 65
            say("PASS: no repeat; total parent notices = 1")
            result = parent.call("get_task", task_id=task_id)
            state = parent.call("coordination_state")["tasks"][0]
            assert result["status"] == "completed" and not state["acknowledged"], state
            say("PASS: get_task still exposes the durable, unconsumed result")
            parent.call("acknowledge_task", task_id=task_id,
                        result_revision=result["result_revision"])
        else:
            transport("demo-0-child", "child")
            say("No result; child screen goes idle. idle_critical = 2 s.")
            receive("child", "Idle without a result: child")
            say("PASS: child received 1 progress reminder")
            receive("parent", "Stalled child: child")
            say("PASS: parent received exactly 1 idle escalation")
            say("Watching 12 s more (6 idle thresholds; sped up).")
            quiet(12)
            evidence["duplicate_observation_seconds"] = 12
            say("PASS: no repeat; worker reminders = 1, parent escalations = 1")
            state = parent.call("coordination_state")["tasks"][0]
            assert state["idle_escalated"] and state["status"] == "running", state
            say("PASS: task remains running; idleness does not mark it failed")
        evidence["observed_receipts_before_cleanup"] = len(received)
        parent.call("log_justification", agent_name="ea", action="kill_agent",
                    justification="Retire only this isolated demonstration's shell child")
        parent.call("kill_agent", name="child")
        # Cancellation may emit a terminal-state notice; it is outside the idle observation.
        evidence["status"] = "passed"
    except BaseException as error:
        evidence.update(status="failed", error=repr(error))
        raise
    finally:
        for process in processes:
            process.stop()
        tmux("kill-server", check=False)
        backend.shutdown()
        backend.server_close()
        log.close()
        shutil.rmtree(root)
        evidence["cleanup"] = {"processes_stopped": True, "state_removed": not root.exists(),
                               "tmux_stopped": tmux("list-sessions", check=False).returncode != 0}
        (BASE / f"{args.mode}-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
        say("Cleanup: own MCP processes, tmux server and temporary state removed.")
        time.sleep(3)


if __name__ == "__main__":
    main()
