#!/usr/bin/env python3
"""Real sockets + isolated tmux: last-window shutdown, child cleanup, native resume.

No model calls. Run with OMAR_BIN pointing at a freshly built runtime.
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
    server = root.name
    env = {**os.environ, "HOME": temporary, "OMAR_TMUX_SERVER": server}
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
    subprocess.run(["tmux", "-L", server, "-f", "/dev/null", "new-session", "-d", "-s", "harness"], check=True)
    subprocess.run(["tmux", "-L", server, "set-option", "-g", "default-shell", "/bin/sh"], check=True)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    address = f"127.0.0.1:{port}"
    daemons = []
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
        daemon = subprocess.Popen([BINARY, "--config", str(config), "serve", "--address", address], env=env, stdout=output, stderr=output)
        output.close()
        daemons.append(daemon)
        eventually(lambda: api("GET", "/health"))
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
        token = json.loads((root / ".omar/mcp/ea-0/context.json").read_text())["serve"]["token"]
        api("POST", "/v1/agent/reply", {"token": token, "text": "Saved before closing"})
        before = api("GET", "/v1/chat")
        one, two = window(), window()
        close(one)
        time.sleep(1)
        assert first.poll() is None, "closing one of two windows stopped the daemon"
        close(two)
        time.sleep(4)
        reloaded = window()
        time.sleep(7)
        assert first.poll() is None, "reconnect did not cancel the original grace period"
        closed_at = time.monotonic()
        close(reloaded)
        assert first.wait(timeout=15) == 0
        assert time.monotonic() - closed_at >= 9.5, "shutdown skipped the grace period"
        original = launches()[0]
        eventually(lambda: dead(original["pid"]) and dead(original["child"]))
        assert subprocess.run(["tmux", "-L", server, "has-session", "-t", "=omar-agent-ea-0"], capture_output=True).returncode != 0
        second = launch()
        reopened = window()
        after = api("GET", "/v1/chat")
        assert before["id"] == after["id"]
        assert after["messages"] == before["messages"]
        resumed = launches()[1]["args"]
        native_id = original["args"][original["args"].index("--session-id") + 1]
        assert resumed[resumed.index("--resume") + 1] == native_id
        time.sleep(1)
        assert len(launches()) == 2, "opening the chat launched a second EA"
        close(reopened)
        assert second.wait(timeout=15) == 0
        eventually(lambda: all(dead(item["pid"]) and dead(item["child"]) for item in launches()))
        print("PASS: multiple windows, refresh grace, EA + child + runtime cleanup, saved chat + native resume")
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
        subprocess.run(["tmux", "-L", server, "kill-server"], capture_output=True)
