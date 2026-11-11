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
        self.assertIn("  disconnect", backends)
        # Only what is connected; `omar connect` lists the rest.
        self.assertIn("claude    ✓ Claude Max", backends)
        self.assertNotIn("cursor", backends)

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

    def test_check_has_each_signed_in_backend_answer_a_test_prompt(self):
        # pi is signed in to a key its vendor rejects; only a prompt finds out.
        store = Path(self.env["HOME"]) / ".pi/agent"
        store.mkdir(parents=True)
        (store / "auth.json").write_text('{"groq": {"type": "api_key"}}')
        pi = Path(self.env["PATH"]) / "pi"
        pi.write_text('#!/bin/sh\n[ "$1" = --version ] && exit 0\n'
            '[ "$1" = -p ] && { echo \'401: {"message":"Invalid API Key"}\' >&2; exit 1; }\nexit 0\n')
        pi.chmod(0o755)
        check = lambda *args: subprocess.run([str(OMAR), "backends", "--check", *args],
            env=self.env, capture_output=True, text=True, timeout=60)

        result = check()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("✓ claude answered its test prompt", result.stdout)
        self.assertIn('✗ pi is signed in, but its test prompt failed: 401: {"message":"Invalid API Key"}',
            result.stdout)
        # Signed out and not named: nothing to check.
        self.assertNotIn("codex", result.stdout.split("\n\n")[1])

        result = check("--json", "claude")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)[0]["check"], {"ok": True})

        # Named, so it must work.
        result = check("codex")
        self.assertEqual(result.returncode, 1)
        self.assertIn("✗ codex is not signed in; run `omar connect codex`", result.stdout)

    def test_the_table_lists_what_is_connected_and_where_to_add_more(self):
        self.assertEqual(self.omar(), "BACKEND   STATUS\nclaude    ✓ Claude Max\n\n"
            "Use `omar connect` to add more backends.\n")
        # Named backends are listed whatever their status, with how to sign in.
        rows = {line.split()[0]: line for line in self.omar("codex", "cursor").splitlines()[1:]}
        self.assertIn("not signed in", rows["codex"])
        self.assertIn("omar connect codex", rows["codex"])
        self.assertIn("not installed", rows["cursor"])

    def test_a_long_status_keeps_its_column(self):
        codex = Path(self.env["PATH"]) / "codex"
        codex.write_text('#!/bin/sh\n[ "$1" = --version ] && exit 0\n'
            '[ "$1 $2" = "login status" ] && { echo \'something new\' >&2; exit 0; }\nexit 0\n')
        header, row = self.omar("codex").splitlines()
        self.assertEqual(row.index("omar connect codex"), header.index("NEXT"))
        self.assertIn("knows  omar connect codex", row)

    def test_with_nothing_connected_the_table_says_how_to_start(self):
        (Path(self.env["PATH"]) / "claude").unlink()
        self.assertEqual(self.omar(), "No backend is connected. Use `omar connect` to add one.\n")


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
        rows = {line.split()[0]: line for line in self.omar("backends", "pi").stdout.splitlines()}
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
        # Connected already: only the test prompt runs, and it says how to
        # switch.
        self.assertEqual(self.omar("connect", "codex").stdout,
            "Checking codex with a test prompt…\n✓ codex connected (ChatGPT)\n"
            "To sign in another way: omar connect codex --replace\n")

    def test_a_sign_in_whose_test_prompt_fails_is_not_called_connected(self):
        # Its status reads the stored sign-in; its vendor rejects the key.
        signed_in = self.root / "codex-signed-in"
        self.stand_in("codex", f"""
[ "$1 $2" = "login status" ] && {{ [ -f {signed_in} ] && echo 'Logged in using an API key - sk-***' >&2 && exit 0; echo 'Not logged in' >&2; exit 1; }}
[ "$*" = "login --device-auth" ] && touch {signed_in}
[ "$1" = exec ] && {{ echo 'ERROR: 401 Unauthorized: Incorrect API key provided' >&2; exit 1; }}""")
        result = self.omar("connect", "codex", ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("codex is signed in, but its test prompt failed: "
            "ERROR: 401 Unauthorized: Incorrect API key provided; "
            "run `omar connect codex` to sign in again", result.stderr)
        self.assertNotIn("connected", result.stdout)

    def test_a_stored_sign_in_that_fails_its_test_prompt_is_signed_in_again(self):
        signed_in, fixed, ran = (self.root / name for name in ("codex-signed-in", "fixed", "ran"))
        signed_in.touch()
        self.stand_in("codex", f"""
[ "$1 $2" = "login status" ] && {{ echo 'Logged in using ChatGPT' >&2; exit 0; }}
[ "$*" = "login --device-auth" ] && touch {fixed}
[ "$1" = exec ] && {{ echo "$PWD $*" >> {ran}; [ -f {fixed} ] || {{ echo 'model_not_found' >&2; exit 1; }}; }}""")
        out = self.omar("connect", "codex").stdout
        self.assertIn("codex is signed in, but its test prompt failed: model_not_found\n"
            "Signing codex in again.", out)
        self.assertTrue(out.endswith("✓ codex connected (ChatGPT)\n"), out)
        # Twice, before and after the sign-in, each in an empty directory
        # that is gone afterwards.
        runs = ran.read_text().splitlines()
        self.assertEqual(len(runs), 2, runs)
        for run in runs:
            directory, args = run.split(" ", 1)
            self.assertEqual(args, "exec --skip-git-repo-check Reply with OK")
            self.assertIn("omar-check-codex-", directory)
            self.assertFalse(Path(directory).exists())

    def test_a_sign_in_that_does_not_take_fails(self):
        self.stand_in("codex", """[ "$1 $2" = "login status" ] && { echo 'Not logged in' >&2; exit 1; }""")
        result = self.omar("connect", "codex", ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("still not signed in", result.stderr)

    def test_a_sign_in_that_cannot_be_checked_fails(self):
        # Its status answers in a way no one can read, before and after.
        self.stand_in("codex", """[ "$1 $2" = "login status" ] && { echo 'huh' >&2; exit 1; }
[ "$1" = login ] && exit 3""")
        result = self.omar("connect", "codex", ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not confirm codex is signed in (its sign-in exit status: 3)", result.stderr)

    def sign_codex_out(self, env=None):
        """Sign codex out from the picker; what it printed."""
        if env:
            self.env = {**self.env, **env}
        until, fd, exit_code = self.picker()
        until("enter connect")
        os.write(fd, b"\x1b[Bdy")
        until("enter connect", 2)
        os.write(fd, b"q")
        screen = until("Done")
        self.assertEqual(exit_code(), 0)
        return screen

    def test_a_key_in_the_environment_is_named_when_sign_out_cannot_clear_it(self):
        self.stand_in("codex", """[ "$1 $2" = "login status" ] && { echo 'Logged in using an API key - sk-***' >&2; exit 0; }""")
        screen = self.sign_codex_out({"OPENAI_API_KEY": "sk-test"})
        self.assertIn("codex still connects with $OPENAI_API_KEY; unset it to disconnect", screen)

    def test_a_sign_out_that_cannot_be_checked_is_not_called_done(self):
        signed_in = self.root / "codex-signed-in"
        signed_in.touch()
        self.stand_in("codex", f"""[ "$1 $2" = "login status" ] && {{ [ -f {signed_in} ] && echo 'Logged in using ChatGPT' >&2 && exit 0; echo 'huh' >&2; exit 1; }}
[ "$*" = "logout" ] && rm {signed_in}""")
        screen = self.sign_codex_out()
        self.assertIn("could not confirm codex is signed out", screen)
        self.assertNotIn("codex disconnected", screen)

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

    def codex(self, signed_in_as=None):
        """A codex that keeps its sign-in where codex does, and, as codex
        does, deletes it as its login starts. The login stops there when
        `abandon` exists, else signs in as `next` (default "new"); a sign-in
        as "broken" fails the test prompt. Returns the operator's sign-in."""
        self.stand_in("codex", f"""
auth="${{CODEX_HOME:-$HOME/.codex}}/auth.json"
[ "$1 $2" = "login status" ] && {{ [ -f "$auth" ] && echo 'Logged in using ChatGPT' >&2 && exit 0; echo 'Not logged in' >&2; exit 1; }}
[ "$*" = "login --device-auth" ] && {{ rm -f "$auth"; [ -f {self.root}/abandon ] && exit 1; mkdir -p "$(dirname "$auth")"; cp {self.root}/next "$auth" 2>/dev/null || echo new > "$auth"; }}
[ "$*" = "logout" ] && rm "$auth"
[ "$1" = exec ] && [ "$(cat "$auth")" = broken ] && {{ echo '401 Unauthorized' >&2; exit 1; }}""")
        auth = self.home / ".codex/auth.json"
        if signed_in_as:
            auth.parent.mkdir(parents=True, exist_ok=True)
            auth.write_text(signed_in_as + "\n")
        return auth

    def test_the_picker_connects_with_arrow_keys(self):
        self.codex()
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

    def test_a_working_sign_in_is_replaced_or_disconnected_on_request(self):
        auth = self.codex(signed_in_as="old")
        staging = self.home / ".omar/sign-in/codex"
        out = self.omar("connect", "codex", "--replace").stdout
        self.assertIn("Your current codex sign-in stays until the new one works.", out)
        self.assertIn("✓ codex connected (ChatGPT)", out)
        self.assertEqual(auth.read_text(), "new\n")
        self.assertFalse(staging.exists())
        self.assertIn("✓ codex disconnected", self.omar("disconnect", "codex").stdout)
        self.assertFalse(auth.exists())
        result = self.omar("disconnect", "jev", ok=False)
        self.assertIn("topology-only", result.stderr)

    def test_a_replace_that_does_not_finish_keeps_the_old_sign_in(self):
        # codex deletes its sign-in as its login starts; the replace signs in
        # aside, so neither an abandoned login nor a new sign-in that fails
        # its test prompt touches the old one.
        auth = self.codex(signed_in_as="old")
        (self.root / "abandon").touch()
        result = self.omar("connect", "codex", "--replace", ok=False)
        self.assertIn("codex is still not signed in", result.stderr)
        self.assertEqual(auth.read_text(), "old\n")
        (self.root / "abandon").unlink()
        (self.root / "next").write_text("broken\n")
        result = self.omar("connect", "codex", "--replace", ok=False)
        self.assertIn("its test prompt failed: 401 Unauthorized", result.stderr)
        self.assertEqual(auth.read_text(), "old\n")
        self.assertFalse((self.home / ".omar/sign-in/codex").exists())
        # A codex home set by the operator is the one replaced.
        (self.root / "next").unlink()
        custom = self.root / "custom-codex"
        self.env["CODEX_HOME"] = str(custom)
        self.omar("connect", "codex", "--replace")
        self.assertEqual((custom / "auth.json").read_text(), "new\n")
        self.assertEqual(auth.read_text(), "old\n")
        result = self.omar("disconnect", "jev", ok=False)
        self.assertIn("topology-only", result.stderr)

    def test_the_picker_replaces_a_working_sign_in_after_asking(self):
        auth = self.codex(signed_in_as="old")
        until, fd, exit_code = self.picker()
        until("enter connect")
        # enter on a connected backend asks first; anything but y keeps it.
        os.write(fd, b"\x1b[B\r")
        until("Replace codex's sign-in?")
        os.write(fd, b"n\r")
        until("Replace codex's sign-in?", 2)
        os.write(fd, b"y")
        until("Replacing codex's sign-in")
        until("✓ codex connected (ChatGPT)")
        self.assertEqual(auth.read_text(), "new\n")
        until("enter connect", 2)
        os.write(fd, b"q")
        until("Done")
        self.assertEqual(exit_code(), 0)

    def test_the_picker_disconnects_after_asking(self):
        signed_in = self.codex(signed_in_as="old")
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
