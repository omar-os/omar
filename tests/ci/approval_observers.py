#!/usr/bin/env python3
"""Drive captured permission screens through real tmux, daemon and HTTP observers."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
from urllib.request import urlopen

BIN = Path(os.environ.get("OMAR_BIN", Path(__file__).resolve().parents[2] / "target/debug/omar")).resolve()
SCREENS = {
    "claude": "Bash command\n\n  git status\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No",
    "cursor": "Run this command?\n  → Run (once) (y)\n    Skip (esc or n)",
    "agy": "Requesting permission for: git status\nDo you want to proceed?\n> 1. Yes\n  2. No",
}

def until(read, matches):
    limit = time.monotonic() + 15
    while time.monotonic() < limit:
        result = read()
        if matches(result):
            return result
        time.sleep(.1)
    raise AssertionError(f"approval transition timed out: {result}")

def verify(backend, screen):
    with tempfile.TemporaryDirectory(prefix="omar-approval-") as folder:
        root = Path(folder)
        env = {k:v for k,v in os.environ.items() if not k.startswith("OMAR_") and k not in ("TMUX","TMUX_PANE")}
        env.update(OMAR_HOME=str(root/"state"), TERM="xterm-256color")
        def cli(*args):
            return subprocess.check_output([str(BIN), *args], env=env, cwd=root, text=True, timeout=90)
        runtime = "observe-"+backend
        session = json.loads(cli("-a", backend, "up", "-s", runtime, "--no-ea", "--json"))
        try:
            def tmux(*args):
                return subprocess.check_output(["tmux","-L",session["tmux_server"],*args], env=env, cwd=root, text=True, timeout=10)
            def snapshot():
                with urlopen(session["url"]+"/v1/approvals", timeout=3) as response:
                    return json.load(response)
            # Use a private pane on this runtime's own tmux server. Only the
            # terminal contents are fixtures; capture, tracking and HTTP are real.
            source = root/"screen"
            source.write_text(screen+"\n")
            pane = "omar-agent-ea-0"
            command = "cat "+shlex.quote(str(source))+"; read -r answer; clear; echo Ready; sleep 30"
            tmux("new-session","-d","-s",pane,"-x","100","-y","25",command)
            pending = until(snapshot,lambda s:len(s["requests"])==1)["requests"][0]
            assert pending["agent_id"]=="assistant" and pending["tool_name"]=="Command execution", pending
            time.sleep(1.7)
            assert [r["request_id"] for r in snapshot()["requests"]]==[pending["request_id"]]
            tmux("send-keys","-t",pane,"Enter")
            done = until(snapshot,lambda s:not s["requests"] and s["recent"])
            assert done["recent"][-1]["outcome"]=="resolved"
            print(f"PASS: {backend} command request detected once, retained and cleared")
        finally:
            cli("rm","-s",runtime)

if __name__ == "__main__":
    for name, screen in SCREENS.items():
        verify(name,screen)
