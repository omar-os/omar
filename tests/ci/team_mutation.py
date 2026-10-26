#!/usr/bin/env python3
"""Mutation: change a running program and have the run continue as the new one.

A counter feeds an echo agent. The new version disconnects the echo and wires
the counter into a keeper it adds. `omar mutate` pauses the run, applies the
diff to the pause checkpoint, and resumes: the counter carries on from its
count, the keeper starts receiving, and a `mutation` checkpoint sits on the
timeline between the pause and the resumed run. Rolling back to the pause
checkpoint goes back to the old program. A program that renames `main` is
refused without touching the run, and a paused run takes a mutation and stays
paused.

The real compiler, Rust bodies and the stub backend; no model.
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
OMARC = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()

COUNTER = '''
team Counter {
    timer beat(0, 300ms)
    state count : int = 0
    output total : int
    reaction(beat) -> total {=
        self.count += 1;
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open("log.txt").unwrap();
        writeln!(log, "{}", self.count).unwrap();
        total = Some(self.count);
    =}
}
team Echo[agent : Stub] {
    input inp : int
    output out : int
    task agent(inp) -> out "echo $(inp)"
}
'''
V1 = COUNTER + '''
main Morph {
    c = Counter()
    e = Echo()
    c.total -> e.inp
}
'''
V2 = COUNTER + '''
team Keeper {
    input seen : int
    reaction(seen) -> {=
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open("seen.txt").unwrap();
        writeln!(log, "{}", seen.unwrap()).unwrap();
    =}
}
main Morph {
    c = Counter()
    e = Echo()
    k = Keeper()
    c.total -> k.seen
}
'''


def main():
    assert shutil.which("tmux"), "tmux is required"
    assert OMARC.exists(), f"{OMARC}: build the compiler with `cd lang && lake build`"
    with tempfile.TemporaryDirectory(prefix="omar-mutation-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source = root / "home", root / "source"
        for path in (home, source):
            path.mkdir()
        env = dict(os.environ, HOME=str(home), OMARC_BIN=str(OMARC),
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        v1, v2, renamed = source / "morph.omar", source / "morph_v2.omar", source / "renamed.omar"
        v1.write_text(V1)
        v2.write_text(V2)
        renamed.write_text(V2.replace("main Morph", "main Other"))
        runtime = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", "mutation", "--json"], cwd=source,
                                            env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
        server = runtime["tmux_server"]
        state_root = Path(runtime["directory"])

        def deployment(name):
            found = list(state_root.glob("ea/*/topologies/Morph"))
            assert len(found) == 1, found
            return found[0] / name

        def omar(*args, timeout=120, ok=True):
            result = subprocess.run([str(BIN), "-s", runtime["name"], *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=timeout)
            if ok:
                assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result

        def record():
            return json.loads(deployment("deployment.json").read_text())

        def status():
            return json.loads(omar("status", "Morph").stdout)

        def manifest(checkpoint):
            return json.loads((next(deployment("checkpoints").glob(f"*-{checkpoint}")) / "manifest.json").read_text())

        def program(checkpoint):
            found = next(deployment("checkpoints").glob(f"*-{checkpoint}")) / "program.json"
            return {i["name"] for i in json.loads(found.read_text())["instructions"] if i["op"] == "declare_instance"}

        def lines(instance, name):
            path = state_root / "workspaces" / record()["workspaces"][instance] / "worktree" / name
            return [int(l) for l in path.read_text().split()] if path.exists() else []

        def wait_until(predicate, what, timeout=90):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if predicate():
                    return
                time.sleep(0.2)
            raise AssertionError(f"timed out waiting for {what}")

        try:
          try:
            run_id = json.loads(omar("run", str(v1)).stdout)["run_id"]
            wait_until(lambda: deployment("deployment.json").exists() and record()["state"] == "RUNNING"
                       and len(lines("c", "log.txt")) >= 3, "three counts")

            # 1. A dry run reports the plan and changes nothing.
            plan = json.loads(omar("mutate", "Morph", str(v2), "--dry-run").stdout)["changes"]
            for change in ("disconnect_ports c.total -> e.inp", "declare_instance k = Keeper",
                           "define_port k.seen : int (input)", "connect_ports c.total -> k.seen"):
                assert change in plan, plan
            assert not any(" c." in change or " e." in change for change in plan
                           if not change.startswith(("disconnect_ports", "connect_ports"))), plan
            assert status()["status"] == "running" and "mutation" not in status(), status()

            # 2. A renamed `main` is another program: refused, run untouched.
            refused = omar("mutate", "Morph", str(renamed), ok=False)
            assert refused.returncode != 0 and "keeps the program's main" in refused.stderr, refused
            assert status()["status"] == "running", status()

            # 3. The mutation: pause, apply, resume, under the same run id.
            mutated = json.loads(omar("mutate", "Morph", str(v2), "--wait", timeout=480).stdout)
            assert mutated["run_id"] == run_id and mutated["mutation"]["status"] == "applied", mutated
            mutation_id = mutated["mutation"]["checkpoint"]
            wait_until(lambda: status()["status"] == "running" and record()["state"] == "RUNNING"
                       and record().get("resumed_from") == mutation_id, "the mutated run")
            change = manifest(mutation_id)
            assert change["trigger"] == "mutation", change
            pause_id = change["parent"]
            assert manifest(pause_id)["trigger"] == "pause", manifest(pause_id)
            assert {"op": "disconnect_ports", "source": "c.total", "target": "e.inp"} in change["mutation"], change
            assert program(pause_id) == {"c", "e"} and program(mutation_id) == {"c", "e", "k"}
            # Kept instances keep their files; the new one starts empty.
            assert change["workspaces"]["c"] == manifest(pause_id)["workspaces"]["c"], change
            assert change["workspaces"]["k"] != manifest(pause_id)["workspaces"].get("k"), change
            listing = omar("checkpoint", "list", "Morph").stdout
            assert "mutation" in listing, listing

            # The counter continues from its count; the keeper hears it.
            wait_until(lambda: len(lines("k", "seen.txt")) >= 2, "the keeper to receive")
            counts = lines("c", "log.txt")
            assert counts == list(range(1, len(counts) + 1)), f"the count restarted or skipped: {counts}"
            seen = lines("k", "seen.txt")
            paused_at = json.loads((next(deployment("checkpoints").glob(f"*-{pause_id}")) / "state.json")
                                   .read_text())["state_vars"]["c.count"]
            assert seen[0] > paused_at, f"keeper saw {seen} but the count was {paused_at} at the pause"

            # 4. A paused run takes a mutation and stays paused; rolling back
            #    to the pause checkpoint goes back to the old program.
            omar("pause", "Morph", "--wait", timeout=480)
            back = json.loads(omar("mutate", "Morph", str(v1)).stdout)
            assert back["status"] == "paused" and back["mutation"]["status"] == "applied", back
            assert program(back["mutation"]["checkpoint"]) == {"c", "e"}
            omar("rollback", "Morph", "--checkpoint", mutation_id)
            omar("resume", "Morph")
            wait_until(lambda: record()["state"] == "RUNNING" and record().get("resumed_from") == mutation_id,
                       "the run resumed at the mutation")
            assert set(record()["workspaces"]) == {"c", "e", "k"}, record()["workspaces"]
            omar("pause", "Morph", "--wait", timeout=480)
            omar("rollback", "Morph", "--checkpoint", pause_id)
            omar("resume", "Morph")
            wait_until(lambda: record()["state"] == "RUNNING" and record().get("resumed_from") == pause_id,
                       "the run rolled back before the mutation")
            assert set(record()["workspaces"]) == {"c", "e"}, record()["workspaces"]
            # A mutation is diffed against what the run executes now, v1 again.
            again = json.loads(omar("mutate", "Morph", str(v2), "--dry-run").stdout)["changes"]
            assert again == plan, (again, plan)
            omar("stop", "Morph")
            print("PASS: dry run, renamed main refused, mutate pauses/applies/resumes under one run id, "
                  "mutation checkpoint between pause and resume, state and files carried over, new instance "
                  "receives, paused mutation stays paused, rollback across a mutation restores the old program")
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
