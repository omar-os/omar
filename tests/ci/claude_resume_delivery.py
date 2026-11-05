#!/usr/bin/env python3
"""Mission Control reaches the EA both on a fresh start and after a restart
resumes its Claude conversation.

A resumed Claude repaints its history and never the "Claude Code" banner,
so delivery must not wait for the banner. The fake `claude` below paints like
the real one and takes messages over a real peer socket. No model calls.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()

FAKE = r'''#!/usr/bin/env python3
import json, os, socket, sys, threading, time
from pathlib import Path
if "--version" in sys.argv:
    sys.exit(print("2.1.0 (Claude Code)"))
home, log = Path(os.environ["HOME"]), Path(os.environ["FAKE_CLAUDE_LOG"])
resumed = "--resume" in sys.argv
with log.open("a") as out:
    out.write(json.dumps({"launch": "resume" if resumed else "fresh"}) + "\n")
if resumed:
    history = "".join(f"  line {n} of an earlier answer\n" for n in range(1, 41))
    print("\x1b[2J\x1b[H" + history + "─" * 40 + "\n❯ \n" + "─" * 40, flush=True)
else:
    print("\x1b[2J\x1b[H Claude Code v0\n" + "─" * 40 + "\n❯ \n" + "─" * 40, flush=True)
sessions = home / ".claude" / "sessions"
sessions.mkdir(parents=True, exist_ok=True)
peer = socket.socket(socket.AF_UNIX)
peer.bind(str(home / f"peer-{os.getpid()}.sock"))
peer.listen()
(sessions / f"{os.getpid()}.json").write_text(json.dumps(
    {"sessionId": "fake", "messagingSocketPath": peer.getsockname()}))
(sessions / f"{os.getpid()}.0.key").write_text(json.dumps({"peerToken": "token"}))
def serve(conn):
    frames = conn.makefile().read().splitlines()
    for frame in map(json.loads, frames):
        if frame.get("type") == "user":
            with log.open("a") as out:
                out.write(json.dumps({"message": frame["message"]["content"]}) + "\n")
while True:
    threading.Thread(target=serve, args=(peer.accept()[0],), daemon=True).start()
'''


def post(url, body):
    request = urllib.request.Request(url, data=json.dumps(body).encode(), method="POST",
                                     headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status
    except urllib.error.HTTPError as error:
        raise AssertionError(f"{body['text']!r}: {error.code} {error.read().decode()}") from None


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-resume-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, shims, work = root / "home", root / "bin", root / "work"
        for path in (home, shims, work):
            path.mkdir()
        log = root / "claude.jsonl"
        (shims / "claude").write_text(FAKE)
        (shims / "claude").chmod(0o700)
        env = dict(os.environ, HOME=str(home), PATH=f"{shims}{os.pathsep}{os.environ['PATH']}",
                   FAKE_CLAUDE_LOG=str(log))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)

        def omar(*args):
            result = subprocess.run([str(BIN), *args], cwd=work, env=env, text=True,
                                    capture_output=True, timeout=120)
            assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        def entries():
            return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

        def delivered(url, text):
            assert post(f"{url}/v1/chat", {"text": text}) == 202
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if any(text in entry.get("message", "") for entry in entries()):
                    return
                time.sleep(0.2)
            raise AssertionError(f"{text!r} never reached the EA: {entries()}")

        try:
            session = json.loads(omar("-a", "claude", "up", "-s", "resumed", "--json"))
            delivered(session["url"], "before the restart")
            omar("down", "-s", "resumed")
            session = json.loads(omar("up", "-s", "resumed", "--json"))
            launches = [entry["launch"] for entry in entries() if "launch" in entry]
            assert launches[0] == "fresh" and launches[-1] == "resume", launches
            delivered(session["url"], "after the restart")
            print("PASS: Mission Control reaches a fresh EA and a resumed one without its banner")
        finally:
            subprocess.run([str(BIN), "rm", "-s", "resumed"], cwd=work, env=env, capture_output=True, timeout=120)


if __name__ == "__main__":
    main()
