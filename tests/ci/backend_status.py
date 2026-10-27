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
        self.assertNotIn("SANDBOX", self.omar().splitlines()[0])


class Connect(unittest.TestCase):
    """`omar connect` installs, signs in and gives sandboxes a key, offline."""

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
        out = self.omar("connect", "pi").stdout
        self.assertIn("Installing pi: npm install -g @earendil-works/pi-coding-agent", out)
        self.assertIn("/login", out)
        self.assertIn("✓ pi connected (openai)", out)
        self.assertIn(f"Add {pi.parent} to PATH", out)

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

    def test_sandboxes_get_a_key_and_its_approval(self):
        stored = self.root / "sbx-secrets"
        self.stand_in("sbx", f"""
if [ "$*" = "secret ls --global --json" ]; then
  printf '{{"secrets":['; [ -f {stored} ] && printf '{{"name":"%s"}}' "$(cat {stored})"; printf ']}}'
  exit 0
fi
[ "$*" = "secret set anthropic" ] && printf anthropic > {stored}""")
        self.stand_in("claude", """[ "$1 $2" = "auth status" ] && echo '{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}'""")
        claude = lambda: next(r for r in json.loads(self.omar("backends", "--json").stdout)
            if r["name"] == "claude")
        self.assertEqual(claude()["sandbox"], {"status": "signed_out",
            "login": "omar connect claude --sandbox"})
        out = self.omar("connect", "claude", "--sandbox").stdout
        self.assertIn("✓ claude can run in sandboxes (anthropic)", out)
        approvals = (self.root / "config/sbx/credentials.yaml").read_text()
        self.assertIn("  anthropic:\n    apiKey:\n      domains: [api.anthropic.com]", approvals)
        self.assertEqual(claude()["sandbox"]["status"], "connected")
        self.assertEqual(claude()["via"], "Claude Max")
        # The table grows a sandbox column once sbx is there.
        self.assertIn("SANDBOX", self.omar("backends").stdout.splitlines()[0])
        result = self.omar("connect", "jev", "--sandbox", ok=False)
        self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
