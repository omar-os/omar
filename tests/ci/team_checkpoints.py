#!/usr/bin/env python3
"""Durable checkpoints in a runtime session: automatic and manual capture,
pause, resume under the same run id, rollback, and the tags that run exactly
once.

No model or Lean compiler: a compiler fixture emits fixed bytecode, a Rust
body does the work, and the built-in stub backend answers the one agent
reaction. One session runs everything, on its own tmux server.
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
TICK_NS = 400_000_000  # 0.4s between timer firings, in logical nanoseconds


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-checkpoints-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source, shims = root / "home", root / "source", root / "bin"
        for path in (home, source, shims):
            path.mkdir()
        env = dict(os.environ, HOME=str(home),
                   PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)

        # Every firing appends its count to log.txt and raises the state
        # variable, so continuity across resumes is readable from both.
        body = '''
self.count += 1;
use std::io::Write;
let mut log = std::fs::OpenOptions::new().create(true).append(true).open("log.txt").unwrap();
writeln!(log, "{}", self.count).unwrap();
total = Some(self.count);
'''
        instructions = [
            {"op": "begin_plan", "team": "Ticker"},
            {"op": "declare_instance", "name": "w", "team": "Writer", "parent": ""},
            {"op": "define_port", "instance": "w", "kind": "input", "name": "w.go", "type": "int"},
            {"op": "define_port", "instance": "w", "kind": "output", "name": "w.total", "type": "int"},
            {"op": "define_port", "instance": "w", "kind": "output", "name": "w.greeting", "type": "string"},
            {"op": "declare_state", "instance": "w", "name": "w.count", "type": "int", "initial": 0},
            {"op": "declare_timer", "instance": "w", "name": "w.beat", "offset": 0, "period": TICK_NS},
            {"op": "spawn_agent", "instance": "w", "name": "w.agent", "backend": "stub"},
            {"op": "install_reaction", "instance": "w", "id": "w.tick", "agent": "",
             "triggers": ["w.beat"], "effects": ["w.total"], "contract": "w.total", "prompt": "", "body": body},
            {"op": "install_reaction", "instance": "w", "id": "w.hello", "agent": "w.agent",
             "triggers": ["w.go"], "effects": ["w.greeting"], "contract": "w.greeting", "prompt": "Say hello."},
            {"op": "commit_plan"},
        ]
        bytecode = root / "program.json"
        bytecode.write_text(json.dumps(dict(version=1, team="Ticker", instructions=instructions)))
        program = source / "ticker.omar"
        program.write_text("// Compiler-fixture input; the test exercises the runtime, not parsing.\n")
        compiler = shims / "omarc"
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\n"
                            f"shutil.copyfile({str(bytecode)!r}, sys.argv[2])\n")
        compiler.chmod(0o700)
        env["OMARC_BIN"] = str(compiler)
        runtime = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", "checkpoints", "--json"], cwd=source,
                                            env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
        server = runtime["tmux_server"]
        state_root = Path(runtime["directory"])

        # The EA id is whatever a fresh session allocates; find the record by name.
        class Deployment:
            def __truediv__(self, name):
                found = list(state_root.glob("ea/*/topologies/Ticker"))
                assert len(found) <= 1, found
                return (found[0] if found else state_root / "ea" / "missing" / "topologies" / "Ticker") / name
        deployment = Deployment()

        def omar(*args, timeout=120):
            result = subprocess.run([str(BIN), "-s", runtime["name"], *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        def status():
            return json.loads(omar("status", "Ticker"))

        def record():
            return json.loads((deployment / "deployment.json").read_text())

        def checkpoints():
            lines = [l for l in omar("checkpoint", "list", "Ticker").splitlines() if l.startswith("#")]
            return [l.split()[1] for l in lines], lines

        def worktree(instance="w"):
            return state_root / "workspaces" / record()["workspaces"][instance] / "worktree"

        def log_lines(tree):
            path = tree / "log.txt"
            return [int(l) for l in path.read_text().split()] if path.exists() else []

        def wait_until(predicate, what, timeout=60):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if predicate():
                    return
                time.sleep(0.2)
            raise AssertionError(f"timed out waiting for {what}")

        def agent_alive():
            return subprocess.run(["tmux", "-L", server, "has-session", "-t", record()["sessions"]["w.agent"]],
                                  capture_output=True).returncode == 0

        try:
          try:
            # 1. A run that checkpoints on its own every 2s of physical time,
            #    and takes a manual checkpoint on request while running.
            run_id = json.loads(omar("run", str(program), "--input", "w.go=1", "--checkpoint-period", "2s"))["run_id"]
            wait_until(lambda: (deployment / "deployment.json").exists() and record()["state"] == "RUNNING",
                       "the run to start")
            wait_until(lambda: len(log_lines(worktree())) >= 3, "three timer firings")
            omar("checkpoint", "create", "Ticker", "--wait")
            ids, listing = checkpoints()
            assert any("manual" in l for l in listing), listing
            wait_until(lambda: any("automatic" in l for l in checkpoints()[1]), "an automatic checkpoint", timeout=30)
            manual_id = next(l.split()[1] for l in checkpoints()[1] if "manual" in l)
            manual = json.loads((next((deployment / "checkpoints").glob(f"*-{manual_id}")) / "manifest.json").read_text())
            manual_state = json.loads((next((deployment / "checkpoints").glob(f"*-{manual_id}")) / "state.json").read_text())
            manual_count = manual_state["state_vars"]["w.count"]
            assert manual["trigger"] == "manual" and manual["policy"]["period_secs"] == 2, manual
            assert manual["workspaces"]["w"]["workspace_id"] == record()["workspaces"]["w"]
            assert manual["agents"]["w.agent"]["restoration"] == "fresh_conversation"
            # Checkpoints are complete even while the run is still writing files.
            omar("checkpoint", "verify", "Ticker", manual_id)

            # 2. A live period change is picked up without a restart.
            omar("checkpoint", "configure", "Ticker", "--period", "1h")
            wait_until(lambda: record().get("next_checkpoint_at", 0) > time.time() + 3000, "the new period")

            # 3. Pause: checkpoint, tear agents down, keep the run resumable.
            paused_run = json.loads(omar("pause", "Ticker", "--wait"))
            assert paused_run["status"] == "paused" and paused_run["run_id"] == run_id, paused_run
            paused = record()
            assert paused["state"] == "PAUSED" and paused["sessions_cleaned"], paused
            pause_id = paused["checkpoint"]
            paused_tree = worktree()
            before = log_lines(paused_tree)
            assert before == list(range(1, len(before) + 1)), before
            paused_state = json.loads((next((deployment / "checkpoints").glob(f"*-{pause_id}")) / "state.json").read_text())
            assert paused_state["state_vars"]["w.count"] == len(before), (paused_state, before)
            assert not agent_alive(), "agent survived the pause"
            # Nothing in flight at the pause: the stub's greeting was recorded.
            assert json.loads((deployment / "outputs.json").read_text())["w.greeting"]
            assert paused_run["outputs"]["w.greeting"], paused_run

            # 4. Resume under the same run id: files restored into a new
            #    workspace, the count continues, nothing repeats, the agent is back.
            resumed = json.loads(omar("resume", "Ticker"))
            assert resumed["run_id"] == run_id and resumed["status"] == "running", resumed
            wait_until(lambda: record()["state"] == "RUNNING" and record().get("resumed_from") == pause_id,
                       "the resumed run")
            resumed_tree = worktree()
            assert resumed_tree != paused_tree, "a resume restores into a new workspace"
            assert log_lines(resumed_tree)[: len(before)] == before, "restored files differ"
            wait_until(lambda: len(log_lines(resumed_tree)) >= len(before) + 3, "the clock to continue")
            after = log_lines(resumed_tree)
            assert after == list(range(1, len(after) + 1)), f"a tag ran twice or was skipped: {after}"
            assert log_lines(paused_tree) == before, "the paused workspace was written to"
            prompt = (deployment / "agents" / "w.agent" / "system.md").read_text()
            assert f"resumed from checkpoint {pause_id}" in prompt, prompt
            assert agent_alive(), "agent not respawned"
            omar("pause", "Ticker", "--wait")
            second = record()
            assert second["state"] == "PAUSED" and second["checkpoint"] != pause_id
            ids, listing = checkpoints()
            assert "resume point" in listing[-1] and ids[-1] == second["checkpoint"], listing
            latest = json.loads((next((deployment / "checkpoints").glob(f"*-{ids[-1]}")) / "manifest.json").read_text())
            assert latest["parent"] == ids[-2], latest

            # 5. Roll back to the manual checkpoint and resume from it: the
            #    files and the count are those of that moment, the later
            #    checkpoints stay on disk, and the run continues from there.
            rolled_back = json.loads(omar("rollback", "Ticker", "--checkpoint", manual_id))
            assert rolled_back["resume_point"] == manual_id and rolled_back["abandoned"], rolled_back
            ids_after, listing = checkpoints()
            assert ids_after == ids, "rollback deleted a checkpoint"
            assert next(l for l in listing if manual_id in l).endswith("<- resume point"), listing
            omar("resume", "Ticker")
            wait_until(lambda: record()["state"] == "RUNNING" and record().get("resumed_from") == manual_id,
                       "the rolled-back run")
            rolled_tree = worktree()
            wait_until(lambda: len(log_lines(rolled_tree)) >= manual_count + 2, "progress after rollback")
            rolled = log_lines(rolled_tree)
            assert rolled[:manual_count] == list(range(1, manual_count + 1)), rolled
            assert rolled[manual_count] == manual_count + 1, f"did not continue from the rolled-back count: {rolled}"
            omar("stop", "Ticker")
            wait_until(lambda: status()["status"] == "stopped", "the stop")
            final = record()
            assert final["state"] == "TERMINATED" and final["state_vars"]["w.count"] == len(log_lines(rolled_tree))
            # A stop captures nothing, so the resume point is still the
            # checkpoint the rollback chose; every checkpoint is still listed.
            final_ids, listing = checkpoints()
            assert final_ids == ids, listing
            assert next(l for l in listing if manual_id in l).endswith("<- resume point"), listing
            print("PASS: automatic and manual checkpoints, live period change, pause, resume under one run id "
                  "with restored files and no repeated tag, agent respawn, rollback to an older checkpoint, stop")
          except BaseException:
            log = state_root / "logs" / "runtime.log"
            if log.exists():
                print("runtime.log:", log.read_text()[-4000:])
            raise
        finally:
            subprocess.run([str(BIN), "down", "-s", runtime["name"], "--force", "--timeout", "5"], env=env, capture_output=True)
            subprocess.run(["tmux", "-L", server, "kill-server"], capture_output=True)


if __name__ == "__main__":
    main()
