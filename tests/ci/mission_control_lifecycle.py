#!/usr/bin/env python3
"""Real sockets + isolated tmux: windows come and go, explicit shutdown, relaunch.

Each foreground `serve` is its own session; resuming a chat across runtimes
belongs to a later milestone. No model calls. Run with OMAR_BIN pointing at a
freshly built runtime.
"""
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time

BINARY = str(Path(os.environ.get("OMAR_BIN", "target/debug/omar")).resolve())


def eventually(check, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, ValueError, http.client.HTTPException):
            pass
        time.sleep(0.05)
    raise AssertionError("condition did not become true")


with tempfile.TemporaryDirectory(prefix="omar-window-") as temporary:
    root = Path(temporary)
    env = {**os.environ, "HOME": temporary}
    for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
        env.pop(key, None)
    log = root / "launches.jsonl"
    fake = root / "claude"
    fake.write_text(f'''#!/usr/bin/env python3
import json, os, signal, subprocess, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
signal.signal(signal.SIGHUP, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); signal.signal(signal.SIGHUP, signal.SIG_IGN); time.sleep(300)"])
with open({str(log)!r}, "a") as output:
    output.write(json.dumps({{"pid": os.getpid(), "child": child.pid, "args": sys.argv[1:]}}) + "\\n")
print("Claude Code ready", flush=True)
while True: time.sleep(1)
''')
    fake.chmod(0o700)
    config = root / "config.toml"
    config.write_text('[agent]\ndefault_command = ' + json.dumps(str(fake)) + '\n')
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    address = f"127.0.0.1:{port}"
    daemons = []
    sessions = []
    streams = []

    def api(method, path, body=None):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
        try:
            connection.request(method, path, body=json.dumps(body) if body else None)
            response = connection.getresponse()
            assert response.status < 300, response.read()
            return json.loads(response.read())
        finally:
            connection.close()

    def window():
        stream = socket.create_connection(("127.0.0.1", port), timeout=2)
        stream.sendall(b"GET /v1/chat/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        assert b"200 OK" in stream.recv(4096)
        streams.append(stream)
        return stream

    def close(stream):
        stream.shutdown(socket.SHUT_RDWR)
        stream.close()
        streams.remove(stream)

    def launches():
        return [json.loads(line) for line in log.read_text().splitlines()]

    def launch():
        output = open(root / f"daemon-{len(daemons)}.log", "w")
        name = f"mc{len(daemons)}"
        daemon = subprocess.Popen([BINARY, "--config", str(config), "serve", "--name", name, "--address", address], env=env, stdout=output, stderr=output)
        output.close()
        daemons.append(daemon)
        eventually(lambda: api("GET", "/health"))
        def listed():
            return json.loads(subprocess.run([BINARY, "ls", "--json"], env=env, capture_output=True, text=True, check=True).stdout)
        # Health answers while the assistant is still starting; the runtime is
        # addressable once the registry says ready.
        sessions.append(eventually(lambda: next((s for s in listed() if s["name"] == name and s["state"] == "ready"), None), seconds=60))
        try:
            eventually(lambda: len(launches()) == len(daemons))
        except AssertionError:
            print((root / f"daemon-{len(daemons)-1}.log").read_text())
            raise
        return daemon

    def dead(pid):
        result = subprocess.run(["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True)
        return result.returncode != 0 or result.stdout.strip().startswith("Z")

    try:
        first = launch()
        token = json.loads((Path(sessions[0]["directory"]) / "mcp/ea-0/context.json").read_text())["serve"]["token"]
        api("POST", "/v1/agent/reply", {"token": token, "text": "Saved before closing"})
        before = api("GET", "/v1/chat")
        one, two = window(), window()
        close(one)
        time.sleep(1)
        assert first.poll() is None, "closing one of two windows stopped the daemon"
        close(two)
        # A runtime outlives its browser windows: no window for longer than the
        # old grace period leaves it running, and a new window finds the same chat.
        time.sleep(12)
        assert first.poll() is None, "closing the last window stopped the runtime"
        reloaded = window()
        assert api("GET", "/v1/chat")["id"] == before["id"]
        close(reloaded)
        # Shutdown is explicit, and takes the EA and its child with it.
        down = subprocess.run([BINARY, "down", "-s", sessions[0]["id"], "--timeout", "30"], env=env, capture_output=True, text=True)
        assert down.returncode == 0, down.stderr
        assert first.wait(timeout=15) == 0
        original = launches()[0]
        eventually(lambda: dead(original["pid"]) and dead(original["child"]))
        assert subprocess.run(["tmux", "-L", sessions[0]["tmux_server"], "has-session", "-t", "=omar-agent-ea-0"], capture_output=True).returncode != 0
        second = launch()
        reopened = window()
        after = api("GET", "/v1/chat")
        assert after["id"] != before["id"], "a new runtime is a new session with its own chat"
        time.sleep(1)
        assert len(launches()) == 2, "opening the chat launched a second EA"
        close(reopened)
        down = subprocess.run([BINARY, "down", "-s", sessions[1]["id"], "--timeout", "30"], env=env, capture_output=True, text=True)
        assert down.returncode == 0, down.stderr
        assert second.wait(timeout=15) == 0
        eventually(lambda: all(dead(item["pid"]) and dead(item["child"]) for item in launches()))
        print("PASS: multiple windows, runtime outlives its windows, explicit shutdown cleans EA + child, relaunch")
    finally:
        for stream in streams:
            stream.close()
        for daemon in daemons:
            if daemon.poll() is None:
                daemon.terminate()
                daemon.wait(timeout=5)
        if log.exists():
            for item in launches():
                for key in ("pid", "child"):
                    if not dead(item[key]):
                        os.kill(item[key], signal.SIGKILL)
        for session in sessions:
            subprocess.run(["tmux", "-L", session["tmux_server"], "kill-server"], capture_output=True)
