#!/usr/bin/env python3
"""`omar backends` reads each CLI's own sign-in status, with stand-in CLIs.

Never runs a real backend or touches a credential: PATH holds only the
stand-ins, and HOME is disposable. Run after cargo build.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
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

    def test_json_reports_each_backend_as_its_cli_does(self):
        status = {row["name"]: row for row in json.loads(self.omar("--json"))}
        self.assertEqual(list(status), ["claude", "codex", "cursor", "opencode", "pi", "agy"])
        self.assertEqual(status["claude"]["status"], "connected")
        self.assertEqual(status["claude"]["via"], "Claude Max")
        self.assertEqual(status["codex"]["status"], "signed_out")
        self.assertEqual(status["codex"]["login"], "codex login")
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
            "via": "CURSOR_API_KEY", "login": "cursor agent login"})

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
        self.assertIn("codex login", rows["codex"])
        self.assertIn("not installed", rows["cursor"])


if __name__ == "__main__":
    unittest.main()
