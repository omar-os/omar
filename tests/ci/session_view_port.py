#!/usr/bin/env python3
"""`omar up --view-address` serves a read-only Mission Control beside the
session's own URL.

Against a real session and a live run: the view port lists the run, relays
its loopback diagram, upgrades an agent terminal for any origin, and refuses
every write while the full port keeps working. A restart keeps the view port;
a session without the flag serves none.
"""
import base64
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()

# A timer an hour out keeps the run, and its stub agent, up for the test.
PROGRAM = dict(version=1, team="Idle", instructions=[
    {"op": "begin_plan", "team": "Idle"},
    {"op": "declare_instance", "name": "w", "team": "Waiter", "parent": ""},
    {"op": "define_port", "instance": "w", "kind": "input", "name": "w.go", "type": "string"},
    {"op": "define_port", "instance": "w", "kind": "output", "name": "w.out", "type": "string"},
    {"op": "declare_timer", "instance": "w", "name": "w.later", "offset": 3_600_000_000_000, "period": 0},
    {"op": "spawn_agent", "instance": "w", "name": "w.agent", "backend": "stub"},
    {"op": "install_reaction", "instance": "w", "id": "w.answer", "agent": "w.agent",
     "triggers": ["w.go"], "effects": ["w.out"], "contract": "w.out", "prompt": "Answer."},
    {"op": "commit_plan"},
])


def http(method, url, body=None, timeout=10):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method, headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def terminal_upgrade(base, agent, origin):
    """Status code of a WebSocket handshake for an agent's terminal."""
    host = base.removeprefix("http://")
    address, port = host.rsplit(":", 1)
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection((address, int(port)), timeout=10) as conn:
        conn.sendall((f"GET /v1/agents/{agent}/terminal HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
                      f"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n"
                      f"Origin: {origin}\r\n\r\n").encode())
        return int(conn.recv(4096).decode(errors="replace").split()[1])


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-view-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source, shims = root / "home", root / "source", root / "bin"
        for path in (home, source, shims):
            path.mkdir()
        env = dict(os.environ, HOME=str(home), PATH=str(shims) + os.pathsep + os.environ["PATH"])
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        bytecode = root / "idle.json"
        bytecode.write_text(json.dumps(PROGRAM))
        (source / "idle.omar").write_text("// fixture\n")
        compiler = shims / "omarc"
        # Invoked as `omarc [flags] <source> <output>`.
        compiler.write_text(f"#!/bin/sh\ncp {bytecode} \"$(eval echo \\${{$#}})\"\n")
        compiler.chmod(0o700)
        env["OMARC_BIN"] = str(compiler)

        def omar(*args, check=True):
            result = subprocess.run([str(BIN), *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=120)
            if check:
                assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result

        try:
            session = json.loads(omar("up", "--no-ea", "--name", "shown", "--json",
                                      "--view-address", "127.0.0.1:0").stdout)
            full, view = session["url"], session["view_url"]
            assert full != view, session
            assert http("GET", f"{full}/health")[1]["access"] == "full"
            assert http("GET", f"{view}/health")[1]["access"] == "view"

            run = json.loads(omar("-s", "shown", "run", str(source / "idle.omar"), "--input", "w.go=\"hi\"").stdout)
            run_id = run["run_id"]
            end = time.monotonic() + 60
            while time.monotonic() < end:
                runs = http("GET", f"{view}/v1/runs")[1]["runs"]
                live = [r for r in runs if r["run_id"] == run_id and r.get("diagram_address")]
                if live and live[0]["status"] == "running":
                    break
                time.sleep(0.3)
            else:
                raise AssertionError(f"the view port never showed the live run: {runs}")

            status, diagram = http("GET", f"{view}/v1/runs/{run_id}/diagram/v1/diagram")
            assert status == 200 and diagram.get("team") == "Idle", (status, diagram)
            # Mission Control asks through its conversation-scoped URL.
            chat = http("GET", f"{view}/v1/chats")[1]["active_id"]
            status, scoped = http("GET", f"{view}/chats/{chat}/v1/runs/{run_id}/diagram/v1/diagram")
            assert status == 200 and scoped.get("team") == "Idle", (status, scoped)

            assert terminal_upgrade(view, "w.agent", "https://display.example") == 101
            # The full port still refuses a page from anywhere else.
            assert terminal_upgrade(full, "w.agent", "https://display.example") == 403

            for method, path in (("POST", f"/v1/runs/{run_id}/stop"), ("POST", "/v1/chats"),
                                 ("POST", "/v1/chat"), ("GET", "/v1/workspaces")):
                status, body = http(method, f"{view}{path}", {} if method == "POST" else None)
                assert status == 403 and body == {"error": "read-only view"}, (method, path, status, body)
            assert http("GET", f"{full}/v1/runs/{run_id}")[1]["status"] == "running"
            stopped = http("POST", f"{full}/v1/runs/{run_id}/stop", {})
            assert stopped[0] in (200, 202), stopped

            omar("down", "-s", "shown")
            restarted = json.loads(omar("up", "--name", "shown", "--json").stdout)
            assert restarted["url"] == full and restarted["view_url"] == view, restarted
            assert http("GET", f"{view}/health")[1]["access"] == "view"

            plain = json.loads(omar("up", "--no-ea", "--name", "plain", "--json").stdout)
            assert "view_url" not in plain, plain
            print("PASS: the view port shows a live run, its diagram and terminals, refuses writes, "
                  "and survives a restart; a session without it serves none")
        finally:
            for name in ("shown", "plain"):
                omar("rm", "-s", name, check=False)


if __name__ == "__main__":
    main()
