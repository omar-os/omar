#!/usr/bin/env python3
"""Upgrade a session's runtime under live workloads.

A second build of omar (the same binary with bytes appended, so its build id
differs) takes over a session: every active run pauses at a tag boundary, the
old runtime hands its state over, the new one starts at the same URL and
resumes exactly the runs the upgrade paused. A run the operator had paused
stays paused; another session is untouched. A build that fails the check
changes nothing; one that does not start is rolled back. A stopped session
that kept its state restarts in place, and a runtime that died mid-run comes
back paused at its last checkpoint.
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
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\n"
                            f"table = {bytecodes!r}\n"
                            "team = open(sys.argv[1]).read().split('team:', 1)[1].split()[0]\n"
                            "shutil.copyfile(table[team], sys.argv[2])\n")
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
        # A build that answers nothing useful, and one that checks out but
        # cannot start as a runtime.
        broken = root / "broken-omar"
        broken.write_text("#!/bin/sh\necho 'not an omar build' >&2\nexit 1\n")
        broken.chmod(0o755)
        crashy = root / "crashy-omar"
        crashy.write_text(f'#!/bin/sh\ncase "$1" in session-daemon) echo "boom" >&2; exit 1;; *) exec {v2_omar} "$@";; esac\n')
        crashy.chmod(0o755)

        sessions = {}

        def up(name, *extra):
            record = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", name, "--json", *extra], cwd=source,
                                               env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
            sessions[name] = record
            return record

        def omar(name, *args, timeout=240):
            result = subprocess.run([str(BIN), "-s", name, *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            assert result.returncode == 0, f"{name} {args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        def refused(name, *args, timeout=240):
            result = subprocess.run([str(BIN), "-s", name, *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            assert result.returncode != 0, f"{name} {args} succeeded: {result.stdout}"
            return result.stdout + result.stderr

        def ls():
            return {s["name"]: s for s in json.loads(subprocess.run([str(BIN), "ls", "--json"], env=env, text=True,
                                                                     capture_output=True, timeout=60, check=True).stdout)}

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
            # The operator pauses one run themselves; the upgrade leaves it be.
            paused_sleeper = json.loads(omar("alpha", "pause", "Sleeper", "--wait"))
            assert paused_sleeper["status"] == "paused"
            before_upgrade = log_lines(worktree("alpha", "Ticker", "w"))
            paused_tree = worktree("alpha", "Ticker", "w")

            # 1. The upgrade itself.
            result = json.loads(omar("alpha", "upgrade", "--executable", str(v2_omar), "--json"))
            upgrade, session = result["upgrade"], result["session"]
            assert upgrade["state"] == "completed", upgrade
            assert [p["run_id"] for p in upgrade["paused"]] == [ticker["run_id"]], upgrade
            assert upgrade["from"]["build_id"] == alpha["build_id"] != upgrade["to"]["build_id"] == session["build_id"], upgrade
            assert session["url"] == alpha["url"] and session["pid"] != alpha["pid"] and session["state"] == "ready", session
            assert session["source_executable"] == str(v2_omar.resolve()), session
            listed = ls()
            assert listed["alpha"]["build_id"] == session["build_id"] and listed["alpha"]["state"] == "ready", listed["alpha"]
            assert listed["beta"]["pid"] == beta["pid"] and listed["beta"]["build_id"] == beta["build_id"], listed["beta"]
            assert (Path(alpha["directory"]) / "bin" / "omar.prev").exists(), "previous build not kept"
            # The new runtime pinned the sibling compiler of the new build.
            assert (Path(alpha["directory"]) / "bin" / "omarc").read_bytes() == (v2 / "omarc").read_bytes()
            info = json.loads(omar("alpha", "info"))
            assert info["upgrade"]["state"] == "completed", info["upgrade"]

            # The ticker resumed under its run id from the pause checkpoint,
            # in a new workspace, without repeating or skipping a tag.
            resumed = status("alpha", "Ticker")
            assert resumed["run_id"] == ticker["run_id"] and resumed["status"] == "running", resumed
            rec = record("alpha", "Ticker")
            assert rec["state"] == "RUNNING" and rec["resumed_from"], rec
            pause_manifest = json.loads(next((deployment("alpha", "Ticker") / "checkpoints").glob(f"*-{rec['resumed_from']}")).joinpath("manifest.json").read_text())
            assert pause_manifest["trigger"] == "pause", pause_manifest
            resumed_tree = worktree("alpha", "Ticker", "w")
            assert resumed_tree != paused_tree
            after = assert_continuous(lambda: resumed_tree, len(before_upgrade) + 3, "the ticker after the upgrade")
            assert after[: len(before_upgrade)] == before_upgrade
            # The operator's paused run is still paused; beta never noticed.
            assert status("alpha", "Sleeper")["status"] == "paused"
            assert not record("alpha", "Sleeper").get("resumed_from")
            assert status("beta", "Ticker")["status"] == "running"
            assert all(event["state"] != "PAUSING" for event in record("beta", "Ticker")["history"]), record("beta", "Ticker")
            # New work is admitted again on the new build.
            assert json.loads(omar("alpha", "resume", "Sleeper"))["status"] == "running"
            assert_continuous(lambda: worktree("alpha", "Sleeper", "s"), 2, "the sleeper after resume")

            # 2. A build that fails the check changes nothing.
            ticks_before = len(checkpoints("alpha", "Ticker"))
            text = refused("alpha", "upgrade", "--executable", str(broken))
            assert "did not answer the upgrade check" in text, text
            assert ls()["alpha"]["pid"] == session["pid"] and status("alpha", "Ticker")["status"] == "running"
            assert json.loads(omar("alpha", "info"))["upgrade"]["state"] == "failed"
            assert len([l for l in checkpoints("alpha", "Ticker") if "pause" in l]) == len([l for l in checkpoints("alpha", "Ticker")[:ticks_before] if "pause" in l]), "a failed check paused the run"

            # 3. A build that checks out but cannot start is rolled back: the
            #    session comes back on its previous build and resumes its runs.
            lines_before = log_lines(worktree("alpha", "Ticker", "w"))
            text = refused("alpha", "upgrade", "--executable", str(crashy))
            assert "rolled back" in text, text
            rolled = ls()["alpha"]
            assert rolled["state"] == "ready" and rolled["build_id"] == session["build_id"], rolled
            assert rolled["pid"] != session["pid"]
            info = json.loads(omar("alpha", "info"))
            assert info["upgrade"]["state"] == "rolled_back" and "boom" in info["upgrade"]["error"] or "startup failed" in info["upgrade"]["error"], info["upgrade"]
            assert status("alpha", "Ticker")["status"] == "running"
            rolled_lines = assert_continuous(lambda: worktree("alpha", "Ticker", "w"), len(lines_before) + 2, "the ticker after the rollback")
            assert rolled_lines[: len(lines_before)] == lines_before
            assert status("alpha", "Sleeper")["status"] == "running"
            assert status("beta", "Ticker")["status"] == "running"

            # 4. A stopped session that kept its state restarts in place and
            #    lists its runs again: a paused run resumes; a run the runtime
            #    died under comes back paused at its last checkpoint.
            gamma = up("gamma", "--checkpoint")
            gamma_ticker = start_ticker("gamma")
            gamma_sleeper = start_ticker("gamma", "sleeper", "s")
            wait_until(lambda: any("automatic" in l for l in checkpoints("gamma", "Ticker")), "gamma's automatic checkpoint", 30)
            omar("gamma", "pause", "Sleeper", "--wait")
            sleeper_lines = log_lines(worktree("gamma", "Sleeper", "s"))
            # A forced shutdown ends the ticker mid-run; its last checkpoint
            # is whatever had landed by then.
            omar("gamma", "down", "--force")
            assert ls()["gamma"]["state"] == "stopped"
            assert record("gamma", "Ticker")["state"] == "RUNNING", record("gamma", "Ticker")
            restarted = json.loads(subprocess.run([str(BIN), "up", "--name", "gamma", "--json"], cwd=source, env=env, text=True,
                                                  capture_output=True, timeout=90, check=True).stdout)
            assert restarted["url"] == gamma["url"] and restarted["pid"] != gamma["pid"], restarted
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
            # An upgrade of the restarted session works the same way.
            result = json.loads(omar("gamma", "upgrade", "--executable", str(v2_omar), "--json"))
            assert result["upgrade"]["state"] == "completed" and sorted(p["run_id"] for p in result["upgrade"]["paused"]) == sorted([gamma_ticker["run_id"], gamma_sleeper["run_id"]]), result["upgrade"]
            assert status("gamma", "Ticker")["status"] == "running" and status("gamma", "Sleeper")["status"] == "running"
            print("PASS: upgrade pauses, hands over, restarts at the same URL on the new build and resumes only what it paused; "
                  "other sessions untouched; failed check changes nothing; failed start rolls back; "
                  "stopped session restarts in place with interrupted runs paused at their last checkpoint")
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
