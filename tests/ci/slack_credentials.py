#!/usr/bin/env python3
"""Exercise Rust login's hidden prompts and logout in a private pseudo-terminal."""
import errno
import json
import os
from pathlib import Path
import pty
import select
import signal
import stat
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()


def terminal_login(env, cancel=False):
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(REPO)
        os.execve(str(BIN), [str(BIN), "messaging", "slack", "login"], env)
    def wait_hidden():
        import termios
        while termios.tcgetattr(fd)[3] & termios.ECHO:
            assert time.monotonic() < deadline, "Terminal echo was not disabled"
            time.sleep(0.01)

    output = b""
    sent_bot = sent_app = False
    deadline = time.monotonic() + 20
    finished = False
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                try:
                    chunk = os.read(fd, 4096)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    break
                if not chunk:
                    break
                output += chunk
                assert len(output) < 16384
                if b"Slack bot token (xoxb-): " in output and not sent_bot:
                    # Check echo is already disabled before sending any value.
                    wait_hidden()
                    os.write(fd, b"xoxb-terminal-fixture\n")
                    sent_bot = True
                if b"Slack app token (xapp-): " in output and not sent_app:
                    wait_hidden()
                    os.write(fd, b"\x03" if cancel else b"xapp-terminal-fixture\n")
                    sent_app = True
        else:
            raise AssertionError("Credential prompt timed out")
        _, status = os.waitpid(pid, 0)
        finished = True
        assert sent_bot and sent_app
        assert b"xoxb-terminal-fixture" not in output and b"xapp-terminal-fixture" not in output
        assert (os.waitstatus_to_exitcode(status) != 0) if cancel else (os.waitstatus_to_exitcode(status) == 0), output.decode()
        return output
    finally:
        os.close(fd)
        if not finished:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)


with tempfile.TemporaryDirectory(prefix="omar-slack-login-") as folder:
    root = Path(folder)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("OMAR_", "SLACK_")) and key not in ("TMUX", "TMUX_PANE")}
    env.update(XDG_CONFIG_HOME=str(root / "config"), OMAR_HOME=str(root / "state"))
    credentials = root / "config/omar/credentials/slack.json"
    terminal_login(env)
    assert credentials.is_file()
    assert stat.S_IMODE(credentials.stat().st_mode) == 0o600
    assert stat.S_IMODE(credentials.parent.stat().st_mode) == 0o700
    saved = json.loads(credentials.read_text())
    assert saved == {"bot_token": "xoxb-terminal-fixture", "app_token": "xapp-terminal-fixture"}
    print("PASS: Rust login hides both tokens and saves private credentials outside the repository")
    original = credentials.read_bytes()
    terminal_login(env, cancel=True)
    assert credentials.read_bytes() == original
    print("PASS: cancelled login leaves existing credentials unchanged")
    for _ in range(2):
        result = subprocess.run([str(BIN), "messaging", "slack", "logout"], cwd=REPO,
                                env=env, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, result.stderr
        assert not credentials.exists()
    print("PASS: Rust logout removes the saved copy and is safe to repeat")
