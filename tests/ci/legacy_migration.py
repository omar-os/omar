#!/usr/bin/env python3
"""Execute the documented one-time recipe against crash/retry fixtures."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
SCRIPT = (REPO / "scripts/migrate_legacy_session.sh").read_text()
BODY = SCRIPT.split("python3 - <<'PY'\n", 1)[1].rsplit("\nPY\n", 1)[0]


class Migration(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="omar migration ")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name) / "legacy home"
        self.home.mkdir()
        self.dest = self.home / "sessions/migrated"
        self.state = "stopped"
        self.files = {
            "eas.json": b'[{"id":0},{"id":2}]', "ea/0/chats.json": b"chat zero",
            "ea/2/state": b"other EA", "logs/log": b"user log", "prompts/p": b"prompt",
            "metrics/m": b"metric", "workspaces/key/worktree/data": b"working data",
            "workspace-history/key/repository.git/HEAD": b"history",
            "manager_notes_ea2.md": b"notes", "scheduled_events.json": b"events",
            "config.toml": b"legacy config", "mcp/keep": b"obsolete scratch",
            "dashboard_handoff.json": b"obsolete handoff",
        }
        for name, data in self.files.items():
            path = self.home / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (self.home / "active_ea").symlink_to("missing-link-target")

    def cli(self, args, **kwargs):
        self.assertEqual(args[0], "omar")
        if args[1] == "up":
            if not self.dest.exists():
                self.assertIn("--no-ea", args)
                self.dest.mkdir(parents=True)
                for name, data in {"eas.json": b"placeholder", "logs/fresh": b"new log", "ea/0/state": b"new state", "config.toml": b"new config"}.items():
                    path = self.dest / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            self.state = "ready"
            return "{}"
        if args[1] == "down":
            self.state = "stopped"
            return "{}"
        self.assertEqual(args[1:3], ["--json", "info"])
        return '{"session":{"state":"' + self.state + '"}}'

    def run_recipe(self, fail_after=None, repair_failure=False, permission_failure=False):
        rename = os.rename
        counter = 0

        def fault(src, dst):
            nonlocal counter
            if permission_failure:
                raise PermissionError("injected permission error")
            rename(src, dst)
            counter += 1
            if counter == fail_after:
                raise OSError("injected interruption after rename")

        with patch.dict(os.environ, OMAR_HOME=str(self.home)), \
             patch("subprocess.check_output", side_effect=self.cli), \
             patch("subprocess.run", side_effect=subprocess.CalledProcessError(1, "git") if repair_failure else None), \
             patch("os.rename", side_effect=fault):
            exec(compile(BODY, str(REPO / "scripts/migrate_legacy_session.sh"), "exec"), {})

    def verify(self):
        for name, data in self.files.items():
            if name.startswith("mcp/") or name == "dashboard_handoff.json":
                self.assertEqual((self.home / name).read_bytes(), data)
                continue
            self.assertEqual((self.dest / name).read_bytes(), data, name)
            if name != "config.toml":
                self.assertFalse((self.home / name).exists(), name)
        self.assertEqual((self.home / "config.toml").read_bytes(), b"legacy config")
        self.assertTrue((self.dest / "active_ea").is_symlink())
        self.assertFalse((self.dest / "ea/ea").exists())
        self.assertFalse((self.dest / "logs/logs").exists())

    def test_documentation_is_the_tested_script(self):
        self.assertIn("```bash\n" + SCRIPT + "```", (REPO / "docs/sessions.md").read_text())
        self.assertNotIn("rm -rf", SCRIPT)

    def test_success_and_retry(self):
        self.run_recipe()
        self.verify()
        before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in self.dest.rglob("*") if p.is_file()}
        self.run_recipe()
        self.verify()
        after = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in self.dest.rglob("*") if p.is_file()}
        self.assertEqual(before, after)

    def test_crash_after_each_rename(self):
        # Includes parking, the gap before install, and half-completed imports.
        for count in range(1, 16):
            with self.subTest(rename=count):
                fixture = Migration()
                fixture.setUp()
                try:
                    try:
                        fixture.run_recipe(fail_after=count)
                    except OSError:
                        pass
                    fixture.run_recipe()
                    fixture.verify()
                finally:
                    fixture.doCleanups()

    def test_repair_failure_retries_without_loss(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_recipe(repair_failure=True)
        self.run_recipe()
        self.verify()

    def test_existing_real_session_refused(self):
        self.dest.mkdir(parents=True)
        (self.dest / "eas.json").write_bytes(b"real session")
        with self.assertRaises(SystemExit):
            self.run_recipe()
        self.assertEqual((self.dest / "eas.json").read_bytes(), b"real session")
        self.assertEqual((self.home / "eas.json").read_bytes(), self.files["eas.json"])

    def test_ambiguous_collision_refused(self):
        with self.assertRaises(OSError):
            self.run_recipe(fail_after=1)
        (self.dest / "eas.json").write_bytes(b"foreign data")
        with self.assertRaises(SystemExit):
            self.run_recipe()
        self.assertEqual((self.dest / "eas.json").read_bytes(), b"foreign data")
        self.assertEqual((self.home / "eas.json").read_bytes(), self.files["eas.json"])

    def test_permission_failure_leaves_both_copies_intact(self):
        with self.assertRaises(PermissionError):
            self.run_recipe(permission_failure=True)
        self.assertEqual((self.home / "eas.json").read_bytes(), self.files["eas.json"])
        self.run_recipe()
        self.verify()

    def test_default_home_and_no_notes_or_workspaces(self):
        with tempfile.TemporaryDirectory() as folder:
            self.home = Path(folder) / ".omar"
            self.dest = self.home / "sessions/migrated"
            (self.home / "ea/0").mkdir(parents=True)
            (self.home / "ea/0/sentinel").write_bytes(b"minimal legacy")
            with patch.dict(os.environ, {"HOME": folder}, clear=True), \
                 patch("subprocess.check_output", side_effect=self.cli):
                exec(compile(BODY, "migration", "exec"), {})
            self.assertEqual((self.dest / "ea/0/sentinel").read_bytes(), b"minimal legacy")

    def test_empty_home(self):
        with tempfile.TemporaryDirectory() as home, patch.dict(os.environ, OMAR_HOME=home):
            exec(compile(BODY, "migration", "exec"), {})
            self.assertFalse((Path(home) / "sessions").exists())


class RealMigration(unittest.TestCase):
    def test_migrated_session_boots_with_registry_notes_and_events(self):
        import shutil
        binary = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
        compiler = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()
        if not binary.is_file() or not compiler.is_file():
            self.skipTest("build omar and omarc for the real migration check")
        with tempfile.TemporaryDirectory(prefix="omar real migration ") as folder:
            root = Path(folder)
            home = root / "legacy home"
            bindir = root / "bin"
            bindir.mkdir()
            (bindir / "omar").symlink_to(binary)
            fake = bindir / "claude"
            fake.write_text("#!/usr/bin/env python3\nimport time\nprint('Claude Code ready', flush=True)\nwhile True: time.sleep(1)\n")
            fake.chmod(0o700)
            env = {k: v for k, v in os.environ.items() if not k.startswith("OMAR_") and k not in ("TMUX", "TMUX_PANE")}
            env.update(OMAR_HOME=str(home), OMARC_BIN=str(compiler), PATH=str(bindir) + os.pathsep + env["PATH"])

            def cli(*args):
                return subprocess.check_output([str(binary), *args], env=env, cwd=root, text=True, stderr=subprocess.PIPE, timeout=120)

            sessions = []
            try:
                import json
                seed = json.loads(cli("up", "--name", "seed", "--no-ea", "--json"))
                sessions.append(seed)
                for name in ("one", "two"):
                    cli("-s", "seed", "ea", "create", "--name", name)
                cli("-s", "seed", "--ea", "two", "ea", "event", "schedule", "--receiver", "ea", "--payload", "retained", "--in-seconds", "3600")
                cli("down", "-s", "seed")
                source = Path(seed["directory"])
                for item in "eas.json eas.lock ea_next_id scheduled_events.json ea".split():
                    (source / item).rename(home / item)
                shutil.copyfile(source / "config.toml", home / "config.toml")
                (home / "manager_notes_ea2.md").write_bytes(b"real migration notes")
                result = subprocess.run(["bash", str(REPO / "scripts/migrate_legacy_session.sh")], env=env, cwd=root, capture_output=True, text=True, timeout=150)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                migrated = json.loads(cli("info", "-s", "migrated"))["session"]
                sessions.append(migrated)
                self.assertEqual([ea["id"] for ea in json.loads(cli("-s", "migrated", "ea", "list"))], [0, 1, 2])
                dest = Path(migrated["directory"])
                self.assertEqual((dest / "manager_notes_ea2.md").read_bytes(), b"real migration notes")
                self.assertTrue(any(event["ea_id"] == 2 for event in json.loads((dest / "scheduled_events.json").read_text())))
                self.assertEqual(migrated["state"], "ready")
            finally:
                for session in sessions:
                    subprocess.run([str(binary), "down", "-s", session["name"], "--force", "--timeout", "5"], env=env, capture_output=True, timeout=20)
                    subprocess.run(["tmux", "-L", session["tmux_server"], "kill-server"], capture_output=True)


if __name__ == "__main__":
    unittest.main()
