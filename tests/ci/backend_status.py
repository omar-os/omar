#!/usr/bin/env python3
"""`omar backends` reads each CLI's own sign-in status, with stand-in CLIs.

Never runs a real backend or touches a credential: PATH holds only the
stand-ins, and HOME is disposable. Run after cargo build.
"""
import json
import os
import pty
import select
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
OMAR = Path(os.environ.get("OMAR_BIN", ROOT / "target/debug/omar")).resolve()

# Each answers `--version`, then its status command the way the real CLI does.
STAND_INS = {
    "claude": """
[ "$1 $2" = "auth status" ] && echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}'
""",
    "codex": """
[ "$1 $2" = "login status" ] && { echo 'Not logged in' >&2; exit 1; }
""",
    "pi": "",
    "agy": """
[ "$1" = "models" ] && { echo 'Error: Please sign in to view available models.' >&2; exit 1; }
""",
}


class BackendStatus(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="omar-backend-status-")
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        for name, body in STAND_INS.items():
            path = bin_dir / name
            path.write_text(f'#!/bin/sh\n[ "$1" = --version ] && exit 0\n{body}exit 0\n')
            path.chmod(0o755)
        (root / "home").mkdir()
        self.env = {"HOME": str(root / "home"), "PATH": str(bin_dir)}

    def omar(self, *args):
        result = subprocess.run([str(OMAR), "backends", *args], env=self.env,
            capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def test_help_leads_with_backends_and_their_status(self):
        result = subprocess.run([str(OMAR), "--help"], env=self.env,
            capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        sections = result.stdout.split("Usage:")[1].split("\n\n")
        backends = sections[1]
        self.assertTrue(backends.startswith("Backends:\n  connect"), backends)
        self.assertIn("cursor    not installed", backends)
        self.assertIn("omar connect cursor", backends)

    def test_json_reports_each_backend_as_its_cli_does(self):
        status = {row["name"]: row for row in json.loads(self.omar("--json"))}
        self.assertEqual(list(status), ["claude", "codex", "cursor", "opencode", "pi", "agy"])
        self.assertEqual(status["claude"]["status"], "connected")
        self.assertEqual(status["claude"]["via"], "Claude Max")
        self.assertEqual(status["codex"]["status"], "signed_out")
        self.assertEqual(status["codex"]["login"], "omar connect codex")
        self.assertEqual(status["agy"]["status"], "signed_out")
        self.assertEqual(status["cursor"]["status"], "not_installed")
        # Pi has no status command; its own store and the environment are empty.
        self.assertEqual(status["pi"]["status"], "signed_out")

    def test_an_api_key_in_the_environment_counts_as_signed_in(self):
        self.env["CURSOR_API_KEY"] = "test"
        cursor = self.env["PATH"] + "/cursor"
        Path(cursor).write_text("#!/bin/sh\nexit 0\n")
        Path(cursor).chmod(0o755)
        status = {row["name"]: row for row in json.loads(self.omar("--json"))}
        self.assertEqual(status["cursor"], {"name": "cursor", "status": "connected",
            "via": "CURSOR_API_KEY", "login": "omar connect cursor"})

    def test_pi_is_signed_in_to_what_its_store_holds(self):
        store = Path(self.env["HOME"]) / ".pi/agent"
        store.mkdir(parents=True)
        (store / "auth.json").write_text('{"anthropic": {"type": "oauth"}}')
        status = {row["name"]: row for row in json.loads(self.omar("--json"))}
        self.assertEqual(status["pi"]["status"], "connected")
        self.assertEqual(status["pi"]["via"], "anthropic")

    def test_the_table_says_how_to_sign_in_what_is_signed_out(self):
        rows = {line.split()[0]: line for line in self.omar().splitlines()[1:]}
        self.assertIn("✓ Claude Max", rows["claude"])
        self.assertIn("not signed in", rows["codex"])
        self.assertIn("omar connect codex", rows["codex"])
        self.assertIn("not installed", rows["cursor"])


class Connect(unittest.TestCase):
    """`omar connect` installs and signs in, offline."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="omar-connect-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()
        self.env = {"HOME": str(self.home), "PATH": f"{self.bin}:/usr/bin:/bin",
            "XDG_CONFIG_HOME": str(self.root / "config")}

    def stand_in(self, name, body):
        path = self.bin / name
        path.write_text(f'#!/bin/sh\n[ "$1" = --version ] && exit 0\n{body}\nexit 0\n')
        path.chmod(0o755)

    def omar(self, *args, ok=True):
        result = subprocess.run([str(OMAR), *args], env=self.env, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=60)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_a_missing_backend_is_installed_then_signed_in(self):
        # Pi installs with npm; this npm drops a pi off PATH, as installers do.
        pi = self.home / ".local/bin/pi"
        package = self.root / "pi"
        package.write_text("""#!/bin/sh
[ "$1" = --version ] && exit 0
mkdir -p "$HOME/.pi/agent" && echo '{"openai": {}}' > "$HOME/.pi/agent/auth.json"
""")
        package.chmod(0o755)
        self.stand_in("npm", f"""
[ "$*" = "install -g @earendil-works/pi-coding-agent" ] || exit 1
mkdir -p {pi.parent} && cp {package} {pi}""")
        self.env["SHELL"] = "/bin/bash"
        out = self.omar("connect", "pi").stdout
        self.assertIn("Installing pi: npm install -g @earendil-works/pi-coding-agent", out)
        self.assertIn("/login", out)
        self.assertIn("✓ pi connected (openai)", out)
        # The shell finds pi from its next start; OMAR finds it already.
        bashrc = self.home / ".bashrc"
        self.assertIn('export PATH="$PATH:$HOME/.local/bin"', bashrc.read_text())
        self.assertIn(f"Added {pi.parent} to PATH in {bashrc}", out)
        rows = {line.split()[0]: line for line in self.omar("backends").stdout.splitlines()}
        self.assertIn("✓ openai", rows["pi"])
        self.omar("connect", "pi")
        self.assertEqual(bashrc.read_text().count("$HOME/.local/bin"), 1)

    def test_a_signed_out_cli_runs_its_own_sign_in(self):
        signed_in = self.root / "codex-signed-in"
        self.stand_in("codex", f"""
[ "$1 $2" = "login status" ] && {{ [ -f {signed_in} ] && echo 'Logged in using ChatGPT' >&2 && exit 0; echo 'Not logged in' >&2; exit 1; }}
[ "$*" = "login --device-auth" ] && touch {signed_in}""")
        out = self.omar("connect", "codex").stdout
        self.assertIn("Device code sign-in must be allowed", out)
        self.assertIn("✓ codex connected (ChatGPT)", out)
        # Connected already: nothing is run again.
        signed_in.unlink()
        signed_in.write_text("")
        self.assertEqual(self.omar("connect", "codex").stdout, "✓ codex connected (ChatGPT)\n")

    def test_a_sign_in_that_does_not_take_fails(self):
        self.stand_in("codex", """[ "$1 $2" = "login status" ] && { echo 'Not logged in' >&2; exit 1; }""")
        result = self.omar("connect", "codex", ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("still not signed in", result.stderr)

    def picker(self):
        """`omar connect` in a pty: a function that waits for text, and the fd."""
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(str(OMAR), [str(OMAR), "connect"], {**self.env, "TERM": "xterm"})
        self.addCleanup(lambda: os.close(fd))
        screen = bytearray()
        def until(text, count=1):
            deadline = time.monotonic() + 30
            while screen.count(text.encode()) < count:
                self.assertLess(time.monotonic(), deadline, screen.decode(errors="replace"))
                if select.select([fd], [], [], 1)[0]:
                    try:
                        screen.extend(os.read(fd, 4096))
                    except OSError:
                        break
            return screen.decode(errors="replace")
        def exit_code():
            return os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1])
        return until, fd, exit_code

    def codex(self, signed_in):
        self.stand_in("codex", f"""
[ "$1 $2" = "login status" ] && {{ [ -f {signed_in} ] && echo 'Logged in using ChatGPT' >&2 && exit 0; echo 'Not logged in' >&2; exit 1; }}
[ "$*" = "login --device-auth" ] && touch {signed_in}
[ "$*" = "logout" ] && rm {signed_in}""")

    def test_the_picker_connects_with_arrow_keys(self):
        self.codex(self.root / "codex-signed-in")
        until, fd, exit_code = self.picker()
        # claude is first; one step down is codex.
        until("enter connect")
        os.write(fd, b"\x1b[B\r")
        until("✓ codex connected (ChatGPT)")
        # Back at the list for the next one; q leaves.
        until("enter connect", 2)
        os.write(fd, b"q")
        screen = until("Done")
        self.assertEqual(exit_code(), 0)
        self.assertIn("Connecting codex", screen)

    def test_the_picker_disconnects_after_asking(self):
        signed_in = self.root / "codex-signed-in"
        signed_in.touch()
        self.codex(signed_in)
        until, fd, exit_code = self.picker()
        until("enter connect")
        # d asks first; anything but y leaves codex signed in.
        os.write(fd, b"\x1b[Bd")
        until("Sign codex out?")
        os.write(fd, b"n")
        os.write(fd, b"d")
        until("Sign codex out?", 2)
        self.assertTrue(signed_in.exists())
        os.write(fd, b"y")
        until("✓ codex disconnected")
        self.assertFalse(signed_in.exists())
        until("enter connect", 2)
        os.write(fd, b"q")
        until("Done")
        self.assertEqual(exit_code(), 0)

    def test_only_agent_backends_connect(self):
        result = self.omar("connect", "jev", ok=False)
        self.assertIn("topology-only", result.stderr)

if __name__ == "__main__":
    unittest.main()
