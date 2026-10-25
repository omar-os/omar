#!/usr/bin/env python3
"""A session survives an upgrade of the omar binary.

The package manager replaces the binary; omar itself has no upgrade command.
The operator pauses what they want kept (`omar down` pauses the rest), the
new build restarts the session by name at the same URL, and `omar resume`
continues each workflow from its checkpoint. A second build of omar (the
same binary with bytes appended, so its build id differs) stands in for the
upgrade. A stopped session stays listed until `omar rm`, like a container; one
whose runtime was killed mid-run comes back paused at its last checkpoint; a
build that cannot start leaves the session restartable on a build that can.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
TICK_NS = 400_000_000


def program_for(team, instance):
    body = '''
self.count += 1;
use std::io::Write;
let mut log = std::fs::OpenOptions::new().create(true).append(true).open("log.txt").unwrap();
writeln!(log, "{}", self.count).unwrap();
total = Some(self.count);
'''
    i = instance
    return dict(version=1, team=team, instructions=[
        {"op": "begin_plan", "team": team},
        {"op": "declare_instance", "name": i, "team": "Writer", "parent": ""},
        {"op": "define_port", "instance": i, "kind": "input", "name": f"{i}.go", "type": "int"},
        {"op": "define_port", "instance": i, "kind": "output", "name": f"{i}.total", "type": "int"},
        {"op": "define_port", "instance": i, "kind": "output", "name": f"{i}.greeting", "type": "string"},
        {"op": "declare_state", "instance": i, "name": f"{i}.count", "type": "int", "initial": 0},
        {"op": "declare_timer", "instance": i, "name": f"{i}.beat", "offset": 0, "period": TICK_NS},
        {"op": "spawn_agent", "instance": i, "name": f"{i}.agent", "backend": "stub"},
        {"op": "install_reaction", "instance": i, "id": f"{i}.tick", "agent": "",
         "triggers": [f"{i}.beat"], "effects": [f"{i}.total"], "contract": f"{i}.total", "prompt": "", "body": body},
        {"op": "install_reaction", "instance": i, "id": f"{i}.hello", "agent": f"{i}.agent",
         "triggers": [f"{i}.go"], "effects": [f"{i}.greeting"], "contract": f"{i}.greeting", "prompt": "Say hello."},
        {"op": "commit_plan"},
    ])


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-upgrade-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source, shims, v2 = root / "home", root / "source", root / "bin", root / "v2"
        for path in (home, source, shims, v2):
            path.mkdir()
        # Host execution under test; sandboxes have their own suite.
        (home / ".omar").mkdir()
        (home / ".omar" / "config.toml").write_text("[sandbox]\nenabled = false\n")
        env = dict(os.environ, HOME=str(home),
                   PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)

        # The compiler fixture picks the bytecode by a marker in the source,
        # since the daemon stages every program under the same file name.
        bytecodes = {}
        for team, instance in (("Ticker", "w"), ("Sleeper", "s")):
            path = root / f"{team}.json"
            path.write_text(json.dumps(program_for(team, instance)))
            (source / f"{team.lower()}.omar").write_text(f"// team: {team}\n")
            bytecodes[team] = str(path)
        compiler = shims / "omarc"
        # Invoked as `omarc [flags] <source> <output>`; only the last two matter.
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\n"
                            f"table = {bytecodes!r}\n"
                            "team = open(sys.argv[-2]).read().split('team:', 1)[1].split()[0]\n"
                            "shutil.copyfile(table[team], sys.argv[-1])\n")
        compiler.chmod(0o700)
        env["OMARC_BIN"] = str(compiler)

        # "Version 2": the same program with a different build id. The ELF
        # loader ignores trailing bytes; the pinned copy hashes differently.
        v2_omar = v2 / "omar"
        shutil.copyfile(BIN, v2_omar)
        with open(v2_omar, "ab") as handle:
            handle.write(b"\n# omar v2 for the upgrade test\n")
        v2_omar.chmod(0o755)
        shutil.copyfile(compiler, v2 / "omarc")
        (v2 / "omarc").chmod(0o700)

        sessions = {}

        def up(name, *extra, binary=BIN, check=True):
            result = subprocess.run([str(binary), "up", "--no-ea", "--name", name, "--json", *extra], cwd=source,
                                    env=env, text=True, capture_output=True, timeout=90)
            if not check:
                return result
            assert result.returncode == 0, f"up {name}: {result.stdout}\n{result.stderr}"
            record = json.loads(result.stdout)
            sessions[name] = record
            return record

        def omar(name, *args, timeout=240, binary=BIN):
            result = subprocess.run([str(binary), "-s", name, *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            assert result.returncode == 0, f"{name} {args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        def refused(name, *args, timeout=240):
            result = subprocess.run([str(BIN), "-s", name, *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            assert result.returncode != 0, f"{name} {args} succeeded: {result.stdout}"
            return result.stdout + result.stderr

        def ls(binary=BIN):
            return {s["name"]: s for s in json.loads(subprocess.run([str(binary), "ls", "--json"], env=env, text=True,
                                                                     capture_output=True, timeout=60, check=True).stdout)}

        def ls_text(binary=BIN):
            return subprocess.run([str(binary), "ls"], env=env, text=True, capture_output=True, timeout=60, check=True).stdout

        def listed_runs(name):
            return {r["team"]: r["status"] for r in ls()[name]["runs"]}

        def deployment(name, team):
            found = list(Path(sessions[name]["directory"]).glob(f"ea/*/topologies/{team}"))
            assert len(found) == 1, found
            return found[0]

        def record(name, team):
            return json.loads((deployment(name, team) / "deployment.json").read_text())

        def status(name, team):
            return json.loads(omar(name, "status", team))

        def worktree(name, team, instance):
            # The record exists a moment before its workspaces are assigned.
            workspace = record(name, team).get("workspaces", {}).get(instance, "unassigned")
            return Path(sessions[name]["directory"]) / "workspaces" / workspace / "worktree"

        def log_lines(tree):
            path = tree / "log.txt"
            return [int(l) for l in path.read_text().split()] if path.exists() else []

        def checkpoints(name, team):
            return [l for l in omar(name, "checkpoint", "list", team).splitlines() if l.startswith("#")]

        def checkpoints_offline(session, team):
            root = next(Path(session["directory"]).glob(f"ea/*/topologies/{team}/checkpoints"))
            return [json.loads((d / "manifest.json").read_text())["trigger"] for d in root.iterdir() if (d / "manifest.json").exists()]

        def wait_until(predicate, what, timeout=60):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if predicate():
                    return
                time.sleep(0.2)
            raise AssertionError(f"timed out waiting for {what}")

        def start_ticker(name, team="ticker", instance="w"):
            started = json.loads(omar(name, "run", str(source / f"{team}.omar"), "--input", f"{instance}.go=1",
                                      "--checkpoint-period", "2s"))
            assert started["status"] == "running", started
            return started

        def assert_continuous(tree, at_least, what):
            # `tree` is resolved on every poll: a run reports running before its
            # record names the workspaces it restored into.
            wait_until(lambda: len(log_lines(tree())) >= at_least, what)
            lines = log_lines(tree())
            assert lines == list(range(1, len(lines) + 1)), f"{what}: a tag ran twice or was skipped: {lines}"
            return lines

        try:
          try:
            alpha, beta = up("alpha"), up("beta")
            ticker = start_ticker("alpha")
            sleeper = start_ticker("alpha", "sleeper", "s")
            beta_ticker = start_ticker("beta")
            wait_until(lambda: len(log_lines(worktree("alpha", "Ticker", "w"))) >= 3, "alpha's ticker")
            wait_until(lambda: len(log_lines(worktree("beta", "Ticker", "w"))) >= 3, "beta's ticker")
            wait_until(lambda: any("automatic" in l for l in checkpoints("alpha", "Ticker")), "an automatic checkpoint", 30)
            # The operator pauses one workflow themselves and leaves the other
            # running; `ls` shows both, per session.
            paused_sleeper = json.loads(omar("alpha", "pause", "Sleeper", "--wait"))
            assert paused_sleeper["status"] == "paused"
            assert listed_runs("alpha") == {"Ticker": "running", "Sleeper": "paused"}, ls()["alpha"]
            text = ls_text()
            assert any(l.startswith("alpha") and "1 running, 1 paused" in l for l in text.splitlines()), text
            assert "WORKFLOWS" in text.splitlines()[0], text
            assert "upgrade" not in subprocess.run([str(BIN), "--help"], env=env, text=True, capture_output=True).stdout
            refused("alpha", "upgrade")

            # 1. `omar down` pauses the workflow still running at its next tag
            #    boundary; the stopped session keeps its state, like a container.
            lines_before = log_lines(worktree("alpha", "Ticker", "w"))
            paused_tree = worktree("alpha", "Ticker", "w")
            text = omar("alpha", "down")
            assert "2 paused workflow(s) kept" in text and "omar up --name alpha" in text and "omar rm -s alpha" in text, text
            stopped = ls()["alpha"]
            assert stopped["state"] == "stopped", stopped
            assert listed_runs("alpha") == {"Ticker": "paused", "Sleeper": "paused"}, stopped
            assert any(l.startswith("alpha") and "stopped" in l and "2 paused" in l for l in ls_text().splitlines())
            assert record("alpha", "Ticker")["state"] == "PAUSED", record("alpha", "Ticker")
            assert any(event["state"] == "PAUSING" for event in record("alpha", "Ticker")["history"]), "down did not pause the ticker"
            assert any("pause" in l for l in checkpoints_offline(alpha, "Ticker")), "no pause checkpoint"
            # Nothing is listening: its agents and tmux server are gone.
            assert not subprocess.run(["tmux", "-L", alpha["tmux_server"], "has-session"], capture_output=True).returncode == 0
            # beta, a different session, is still running.
            assert status("beta", "Ticker")["status"] == "running"

            # 2. The binary is replaced (brew, the install script), and the new
            #    build restarts the session by name: same URL, new build id,
            #    the paused workflows listed and waiting for `resume`.
            restarted = subprocess.run([str(v2_omar), "up", "--name", "alpha"], cwd=source, env=env, text=True,
                                       capture_output=True, timeout=90)
            assert restarted.returncode == 0, restarted.stderr
            banner = restarted.stdout
            assert banner.startswith("Restarted session alpha on build "), banner
            assert f"was {alpha['build_id'][:12]}" in banner, banner
            assert "Paused workflows:" in banner and "Ticker" in banner and "Sleeper" in banner, banner
            assert "at checkpoint #" in banner and "Resume: omar resume " in banner, banner
            session = ls()["alpha"]
            assert session["state"] == "ready" and session["url"] == alpha["url"], session
            assert session["pid"] != alpha["pid"] and session["build_id"] != alpha["build_id"], session
            assert session["source_executable"] == str(v2_omar.resolve()), session
            assert listed_runs("alpha") == {"Ticker": "paused", "Sleeper": "paused"}, session
            assert ls()["beta"]["pid"] == beta["pid"] and ls()["beta"]["build_id"] == beta["build_id"]
            # The new runtime is the one answering now, on the new build.
            assert json.loads(omar("alpha", "info"))["session"]["build_id"] == session["build_id"]
            runs = {r["run_id"]: r for r in json.loads(omar("alpha", "runs"))}
            assert runs[ticker["run_id"]]["status"] == "paused" and not runs[ticker["run_id"]]["error"], runs
            assert runs[sleeper["run_id"]]["status"] == "paused", runs

            # 3. Resume continues each workflow under its run id from the pause
            #    checkpoint, in a new workspace, without repeating or skipping a tag.
            resumed = json.loads(omar("alpha", "resume", "Ticker"))
            assert resumed["run_id"] == ticker["run_id"] and resumed["status"] == "running", resumed
            wait_until(lambda: record("alpha", "Ticker")["state"] == "RUNNING", "the ticker to be running again")
            rec = record("alpha", "Ticker")
            assert rec["resumed_from"], rec
            pause_manifest = json.loads(next((deployment("alpha", "Ticker") / "checkpoints").glob(f"*-{rec['resumed_from']}")).joinpath("manifest.json").read_text())
            assert pause_manifest["trigger"] == "pause", pause_manifest
            resumed_tree = worktree("alpha", "Ticker", "w")
            assert resumed_tree != paused_tree
            after = assert_continuous(lambda: resumed_tree, len(lines_before) + 3, "the ticker after the restart")
            assert after[: len(lines_before)] == lines_before
            assert json.loads(omar("alpha", "resume", "Sleeper"))["status"] == "running"
            assert_continuous(lambda: worktree("alpha", "Sleeper", "s"), 2, "the sleeper after resume")
            assert listed_runs("alpha") == {"Ticker": "running", "Sleeper": "running"}
            # beta never noticed.
            assert status("beta", "Ticker")["status"] == "running"
            assert all(event["state"] != "PAUSING" for event in record("beta", "Ticker")["history"]), record("beta", "Ticker")

            # 4. A stopped session stays listed even with no workflows left;
            #    only `rm` removes it, and `rm` kills a running one.
            omar("beta", "stop", "Ticker")
            wait_until(lambda: status("beta", "Ticker")["status"] == "stopped", "beta's ticker to stop")
            text = omar("beta", "down")
            assert "kept" not in text and "omar rm -s beta removes it" in text, text
            assert ls()["beta"]["state"] == "stopped" and Path(beta["directory"]).is_dir()
            assert any(l.startswith("beta") and "stopped" in l and " - " in l for l in ls_text().splitlines()), ls_text()
            omar("beta", "rm")
            assert "beta" not in ls(), ls()
            assert not Path(beta["directory"]).exists()
            del sessions["beta"]

            # 5. A forced shutdown cuts a workflow off mid-run. The state is kept
            #    anyway, `ls` says what was interrupted, and the restart pauses
            #    it at its last checkpoint.
            gamma = up("gamma")
            gamma_ticker = start_ticker("gamma")
            gamma_sleeper = start_ticker("gamma", "sleeper", "s")
            wait_until(lambda: any("automatic" in l for l in checkpoints("gamma", "Ticker")), "gamma's automatic checkpoint", 30)
            omar("gamma", "pause", "Sleeper", "--wait")
            sleeper_lines = log_lines(worktree("gamma", "Sleeper", "s"))
            omar("gamma", "down", "--force")
            assert ls()["gamma"]["state"] == "stopped"
            # The ticker's record is as it was admitted; only a pause or an end rewrites it.
            assert listed_runs("gamma")["Sleeper"] == "paused" and listed_runs("gamma")["Ticker"] in ("starting", "running"), ls()["gamma"]
            assert any(l.startswith("gamma") and "1 paused, 1 interrupted" in l for l in ls_text().splitlines()), ls_text()
            assert record("gamma", "Ticker")["state"] == "RUNNING", record("gamma", "Ticker")
            restarted = up("gamma", binary=v2_omar)
            assert restarted["url"] == gamma["url"] and restarted["pid"] != gamma["pid"], restarted
            assert restarted["previous_build"]["build_id"] == gamma["build_id"] != restarted["build_id"], restarted
            assert sorted(r["team"] for r in restarted["paused_runs"]) == ["Sleeper", "Ticker"], restarted
            runs = {r["run_id"]: r for r in json.loads(omar("gamma", "runs"))}
            assert runs[gamma_sleeper["run_id"]]["status"] == "paused", runs
            interrupted = runs[gamma_ticker["run_id"]]
            assert interrupted["status"] == "paused" and "exited mid-run" in interrupted["error"], interrupted
            assert record("gamma", "Ticker")["state"] == "PAUSED"
            # The resume point is the newest complete checkpoint, which may be
            # one the dying runtime published but never got to record.
            ticker_checkpoint = next(l for l in checkpoints("gamma", "Ticker") if l.endswith("<- resume point")).split()[1]
            checkpoint_count = json.loads(next((deployment("gamma", "Ticker") / "checkpoints").glob(f"*-{ticker_checkpoint}")).joinpath("state.json").read_text())["state_vars"]["w.count"]
            omar("gamma", "resume", "Sleeper")
            resumed_sleeper = assert_continuous(lambda: worktree("gamma", "Sleeper", "s"), len(sleeper_lines) + 2, "gamma's sleeper after restart")
            assert resumed_sleeper[: len(sleeper_lines)] == sleeper_lines
            omar("gamma", "resume", "Ticker")
            assert record("gamma", "Ticker")["resumed_from"] == ticker_checkpoint
            recovered = assert_continuous(lambda: worktree("gamma", "Ticker", "w"), checkpoint_count + 2, "gamma's ticker from its last checkpoint")
            assert recovered[checkpoint_count] == checkpoint_count + 1, recovered

            # 6. A runtime that fails to start (here: its address is taken)
            #    leaves the session stopped with its workflows; the next
            #    start that works brings them back.
            text = omar("gamma", "down")
            assert "2 paused workflow(s) kept" in text, text
            taken = ls()["alpha"]["url"].removeprefix("http://")
            failed = up("gamma", "--address", taken, check=False)
            assert failed.returncode != 0 and "startup failed" in failed.stderr, failed
            assert ls()["gamma"]["state"] == "failed" and listed_runs("gamma") == {"Ticker": "paused", "Sleeper": "paused"}
            again = up("gamma")
            assert again["url"] == gamma["url"] and again["build_id"] == gamma["build_id"], again
            assert sorted(r["team"] for r in again["paused_runs"]) == ["Sleeper", "Ticker"], again
            assert json.loads(omar("gamma", "resume", "Ticker"))["status"] == "running"
            assert json.loads(omar("gamma", "resume", "Sleeper"))["status"] == "running"
            print("PASS: down pauses running workflows and keeps the session; a new build restarts it by name at the same URL "
                  "listing the paused workflows; resume continues each from its checkpoint; other sessions untouched; "
                  "a stopped session stays until rm; a forced stop comes back paused at the last checkpoint; "
                  "a build that cannot start leaves the session restartable")
          except Exception:
            for name, record_ in sessions.items():
                log = Path(record_["directory"]) / "logs" / "runtime.log"
                if log.exists():
                    # Automatic captures land every two seconds here; they would
                    # push the lines that explain a failure out of view.
                    lines = [l for l in log.read_text().splitlines() if "published: automatic" not in l]
                    print(f"---- {name} runtime.log ----\n" + "\n".join(lines)[-8000:])
                for team in ("Ticker", "Sleeper"):
                    path = Path(record_["directory"]) / "ea" / "0" / "topologies" / team / "deployment.json"
                    if path.exists():
                        rec = json.loads(path.read_text())
                        print(f"---- {name} {team}: {rec['state']} error={rec.get('error')} history={[h.get('detail') for h in rec['history'][-3:]]}")
            raise
        finally:
            for name, record_ in sessions.items():
                subprocess.run([str(BIN), "down", "-s", name, "--force"], env=env, capture_output=True, timeout=60)
                subprocess.run(["tmux", "-L", record_["tmux_server"], "kill-server"], capture_output=True)


if __name__ == "__main__":
    main()
