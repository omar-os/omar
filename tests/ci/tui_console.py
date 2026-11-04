#!/usr/bin/env python3
"""The terminal console follows a run in logical time and stops it through serve.

A real `omar serve` (no assistant) runs a periodic program: a timer wakes a
Stub agent, whose action wakes a second one a microstep later. `omar attach
--tui` runs in a private tmux pane. Its Runs view places the run on the
program's timeline, `(0, 0)` then `(0, 1)`, and once the run is past them
they read `done`. `x` then `y` stops the run, which serve then reports as
stopped. Everything the console shows comes from serve's HTTP API.

The real compiler and runtime, the Stub backend; no model.
"""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
OMARC = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()

PROGRAM = '''
team Relay[first : Stub, second : Stub] {
    timer beat(0, 2s)
    action handoff : string
    output done : string
    task first(beat) -> handoff "Start."
    task second(handoff) -> done "Finish $(handoff)."
}
main Ticks {
    r = Relay()
}
'''


def main():
    assert shutil.which("tmux"), "tmux is required"
    assert OMARC.exists(), f"{OMARC}: build the compiler with `cd lang && lake build`"
    with tempfile.TemporaryDirectory(prefix="omar-console-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        home = Path(directory)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        env = dict(os.environ, HOME=str(home), OMARC_BIN=str(OMARC), TERM="xterm-256color")
        for key in ("TMUX", "TMUX_PANE", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        base = f"http://127.0.0.1:{port}"
        tmux = ["tmux", "-L", f"omar-console-test-{port}"]
        log = open(home / "serve.log", "w")
        daemon = subprocess.Popen([str(BIN), "serve", "--no-ea", "-s", "console", "--port", str(port)],
                                  env=env, stdout=log, stderr=log)

        def call(method, path, body=None):
            request = urllib.request.Request(base + path, method=method,
                                             data=json.dumps(body).encode() if body is not None else None)
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.loads(response.read() or b"null")

        def screen():
            return subprocess.run(tmux + ["capture-pane", "-p", "-t", "console"],
                                  capture_output=True, text=True, check=True).stdout

        def wait_for(check, what, seconds=60):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                text = screen()
                if check(text):
                    return text
                time.sleep(0.3)
            raise AssertionError(f"{what}; the console shows:\n{screen()}\nserve log:\n{(home / 'serve.log').read_text()[-3000:]}")

        try:
            deadline = time.monotonic() + 60
            while True:
                try:
                    call("GET", "/health")
                    break
                except OSError:
                    assert time.monotonic() < deadline, (home / "serve.log").read_text()
                    time.sleep(0.2)

            subprocess.run(tmux + ["new-session", "-d", "-s", "console", "-x", "120", "-y", "34",
                                   f"{BIN} attach -s console --tui; sleep 600"], env=env, check=True)
            wait_for(lambda text: "1 Chat" in text and "Conversation with the assistant" in text,
                     "the console opened on the chat")

            run = call("POST", "/v1/runs", {"program": PROGRAM})
            # In the chat every key is text; Tab moves on: Design, then Runs.
            subprocess.run(tmux + ["send-keys", "-t", "console", "Tab", "Tab"], check=True)
            # The second beat is two seconds in, so the first tag's handoff
            # lands at (0, 1) and both are done before (2s, 0) begins.
            text = wait_for(lambda text: all(
                any(tag in line and "done" in line for line in text.splitlines())
                for tag in ("(0, 0)", "(0, 1)")), "the first two tags read done")
            assert "Ticks" in text and "(2s, 0)" in text, text
            assert "r.reaction.0" in text and "r.reaction.1" in text, text

            subprocess.run(tmux + ["send-keys", "-t", "console", "x"], check=True)
            wait_for(lambda text: "y stops it" in text, "the stop asks first")
            subprocess.run(tmux + ["send-keys", "-t", "console", "y"], check=True)
            wait_for(lambda text: "stopped" in text, "the console shows the run stopped")
            status = next(r["status"] for r in call("GET", "/v1/runs")["runs"] if r["run_id"] == run["run_id"])
            assert status == "stopped", status
            print("the console followed the run in logical time and stopped it through serve")
        finally:
            subprocess.run(tmux + ["kill-server"], capture_output=True)
            daemon.terminate()
            daemon.wait(timeout=30)
            log.close()
            subprocess.run([str(BIN), "rm", "-s", "console"], env=env, capture_output=True)


if __name__ == "__main__":
    main()
