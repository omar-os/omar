#!/usr/bin/env python3
"""Sandbox transport/lifecycle contracts. The sbx fixture is NOT VM isolation.

One runtime session runs every topology. The fake sbx runs under that
runtime, so fault injection reads a toggles file rather than per-call env.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

BIN = Path(os.environ.get("OMAR_BIN", "target/debug/omar")).resolve()
TEMPLATE = "docker.io/test/omar@sha256:" + "a" * 64
DEFAULT = "docker.io/lsk567/omar-sandbox@sha256:"
TICK_NS = 200_000_000


def main():
    with tempfile.TemporaryDirectory(prefix="omar-sandbox-contract-", dir="/tmp", ignore_cleanup_errors=True) as tmp:
        root = Path(tmp)
        home, source, boxes, shims = root / "home", root / "source", root / "boxes", root / "bin"
        for path in (home, source, boxes, shims):
            path.mkdir()
        (source / "seed.txt").write_text("stays in the operator's directory")
        calls = root / "calls.jsonl"
        toggles = root / "toggles.json"
        sbx = root / "sbx"
        sbx.write_text("""#!/usr/bin/env python3
import hashlib, json, os, signal, subprocess, sys
from pathlib import Path
try: faults = json.load(open(os.environ["FAKE_TOGGLES"]))
except FileNotFoundError: faults = {}
root = Path(faults.get("BOXES") or os.environ["FAKE_BOXES"])
args = sys.argv[1:]
with open(os.environ["FAKE_CALLS"], "a") as f: f.write(json.dumps(args)+"\\n")
if args[0] == "create":
    name = args[args.index("--name")+1]
    box = root / name
    if faults.get("FAIL_BEFORE_CREATE") and len(list(root.iterdir())) % 3 == 0: sys.exit(1)
    box.mkdir()
    (box/"mounts").write_text(json.dumps(args[-2:]))
    if faults.get("FAIL_CREATE") and len(list(root.iterdir())) % 3 == 2: sys.exit(1)
elif args[0] == "exec" and args[-2:-1] == ["-c"]:
    # The runtime installs its own binary as the worker.
    box = root / next(a for a in args if a.startswith("omar-team-"))
    (box/"worker-sha").write_text(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())
elif args[0] == "exec":
    name = next(a for a in args if a.startswith("omar-team-"))
    box = root / name
    (box/"pid").write_text(str(os.getpid()))
    # A VM sees none of the host runtime's environment.
    env = {k: v for k, v in os.environ.items() if not k.startswith("OMAR_")}
    env.update(HOME=str(box), OMAR_TMUX_SERVER=name)
    os.chdir(args[args.index("--workdir")+1])
    os.execve(os.environ["REAL_OMAR"], [os.environ["REAL_OMAR"], "sandbox-worker"], env)
elif args[0] == "stop":
    if faults.get("FAIL_STOP"): sys.exit(1)
    box = root/args[1]
    if not box.exists(): sys.exit(1)
    subprocess.run(["tmux", "-L", args[1], "kill-server"], capture_output=True)
    if (box/"pid").exists():
        try: os.kill(int((box/"pid").read_text()), signal.SIGTERM)
        except ProcessLookupError: pass
elif args[0] == "ls":
    print("\\n".join(p.name for p in root.iterdir()))
elif args[:2] == ["diagnose", "--json"]:
    status = "fail" if faults.get("SIGNED_OUT") else "pass"
    print(json.dumps({"checks": [{"name": "Authentication", "status": status,
                                  "message": "not signed in", "hint": "Run sbx login"}]}))
elif args[:2] == ["secret", "ls"]:
    print(json.dumps({"secrets": []}))
else: sys.exit(2)
""")
        sbx.chmod(0o700)

        def fault(**active):
            toggles.write_text(json.dumps(active))

        instructions = [{"op": "begin_plan", "team": "SandboxTeams"}]
        for instance, parent in [("a", ""), ("a.child", "a"), ("b", "")]:
            instructions.append(dict(op="declare_instance", name=instance, team="Writer", parent=parent))
            for kind, name, ty in [("input", "tick", "int"), ("output", "out", "string")]:
                instructions.append(dict(op="define_port", instance=instance, kind=kind,
                                         name=instance+"."+name, type=ty))
            # Distinct prompt agents are launched inside each VM worker.
            for agent in ("first", "second"):
                instructions.append(dict(op="spawn_agent", instance=instance, name=instance+"."+agent, backend="stub"))
            instructions.append(dict(op="install_reaction", instance=instance, id=instance+".write",
                agent="", triggers=[instance+".tick"], effects=[instance+".out"], contract=instance+".out", prompt="",
                body='std::fs::write("artifact", "first").unwrap(); out = Some("first".to_string());'))
            instructions.append(dict(op="install_reaction", instance=instance, id=instance+".read",
                agent="", triggers=[instance+".tick"], effects=[instance+".out"], contract=instance+".out", prompt="",
                body='assert_eq!(std::fs::read_to_string("artifact").unwrap(), "first"); std::fs::write("artifact", "second").unwrap(); out = Some("second".to_string());'))
            instructions.append(dict(op="install_reaction", instance=instance, id=instance+".agent",
                agent=instance+".second", triggers=[instance+".tick"], effects=[instance+".out"],
                contract=instance+".out", prompt="Return a string."))
        # A parent can react to a child's output without receiving child-private
        # definitions. Its worker still needs the referenced port's type.
        instructions.append(dict(op="define_port", instance="a", kind="output", name="a.observed", type="string"))
        instructions.append(dict(op="install_reaction", instance="a", id="a.observe-child", agent="",
            triggers=["a.child.out"], effects=["a.observed"], contract="a.observed", prompt="",
            body='observed = Some(out.unwrap());'))
        instructions.append(dict(op="define_port", instance="a", kind="action", name="a.artifact_path", type="path"))
        instructions.append(dict(op="define_port", instance="a", kind="output", name="a.file_result", type="string"))
        instructions.append(dict(op="install_reaction", instance="a", id="a.publish-path", agent="",
            triggers=["a.tick"], effects=["a.artifact_path"], contract="a.artifact_path", prompt="",
            body='artifact_path = Some("artifact".to_string());'))
        instructions.append(dict(op="install_reaction", instance="a", id="a.read-path", agent="",
            triggers=["a.artifact_path"], effects=["a.file_result"], contract="a.file_result", prompt="",
            body='let text = std::fs::read_to_string(artifact_path.unwrap()).unwrap(); assert_eq!(text, "second"); file_result = Some(text);'))
        instructions.append({"op": "commit_plan"})

        # A timer-driven team for pause/resume: each firing appends to log.txt.
        ticker = [
            {"op": "begin_plan", "team": "SandboxTicker"},
            {"op": "declare_instance", "name": "w", "team": "Writer", "parent": ""},
            {"op": "define_port", "instance": "w", "kind": "input", "name": "w.go", "type": "int"},
            {"op": "define_port", "instance": "w", "kind": "output", "name": "w.total", "type": "int"},
            {"op": "define_port", "instance": "w", "kind": "output", "name": "w.greeting", "type": "string"},
            {"op": "declare_state", "instance": "w", "name": "w.count", "type": "int", "initial": 0},
            {"op": "declare_timer", "instance": "w", "name": "w.beat", "offset": 0, "period": TICK_NS},
            {"op": "spawn_agent", "instance": "w", "name": "w.agent", "backend": "stub"},
            {"op": "install_reaction", "instance": "w", "id": "w.tick", "agent": "",
             "triggers": ["w.beat"], "effects": ["w.total"], "contract": "w.total", "prompt": "", "body": '''
self.count += 1;
use std::io::Write;
let mut log = std::fs::OpenOptions::new().create(true).append(true).open("log.txt").unwrap();
writeln!(log, "{}", self.count).unwrap();
total = Some(self.count);
'''},
            {"op": "install_reaction", "instance": "w", "id": "w.hello", "agent": "w.agent",
             "triggers": ["w.go"], "effects": ["w.greeting"], "contract": "w.greeting", "prompt": "Say hello."},
            {"op": "commit_plan"},
        ]
        for name, team, plan in [("program", "SandboxTeams", instructions), ("ticker", "SandboxTicker", ticker)]:
            (root / (name + ".json")).write_text(json.dumps(dict(version=1, team=team, instructions=plan)))
            (source / (name + ".omar")).write_text("// Compiler-fixture input\n")
        program, ticker_program = source / "program.omar", source / "ticker.omar"
        compiler = shims / "omarc"
        # omarc [options] <input> <output>: copy the bytecode named after the input.
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\nfrom pathlib import Path\n"
                            f"shutil.copyfile(Path({str(root)!r}) / (Path(sys.argv[-2]).stem + '.json'), sys.argv[-1])\n")
        compiler.chmod(0o700)
        env = dict(os.environ, HOME=str(home), PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   REAL_OMAR=str(BIN), OMAR_SBX_BIN=str(sbx), OMARC_BIN=str(compiler),
                   FAKE_BOXES=str(boxes), FAKE_CALLS=str(calls), FAKE_TOGGLES=str(toggles),
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home()/".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home()/".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        runtime = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", "sandboxes", "--json"], cwd=source,
                                            env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
        state_root = Path(runtime["directory"])
        runtimes = [runtime]

        def omar(*args, timeout=240, session=None):
            return subprocess.run([str(BIN), "-s", session or runtime["name"], *args], cwd=source, env=env,
                                  capture_output=True, text=True, timeout=timeout)

        def ok(*args, **kwargs):
            result = omar(*args, **kwargs)
            assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        args = ["run", str(program), "--sandbox-template", TEMPLATE, "--fast", "--fg", "--keep-workspaces"]
        for instance in ("a", "a.child", "b"):
            args += ["--input", instance+".tick=1"]

        def record_path(team):
            found = list(state_root.glob(f"ea/*/topologies/{team}/deployment.json"))
            assert len(found) <= 1, found
            return found[0] if found else None

        def record(team="SandboxTeams"):
            return json.loads(record_path(team).read_text())

        def entries(since=0):
            return [json.loads(line) for line in calls.read_text().splitlines()[since:]]

        def wait_until(predicate, what, timeout=60):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                if predicate():
                    return
                time.sleep(0.2)
            raise AssertionError(f"timed out waiting for {what}")

        try:
            fault()
            result = omar(*args)
            assert result.returncode == 0, (result.stdout, result.stderr, [(str(p), p.read_text()) for p in state_root.rglob("sandbox-logs/*.log")])
            creates = [e for e in entries() if e[0] == "create"]
            stops = [e for e in entries() if e[0] == "stop"]
            assert len(creates) == len(stops) == 3, entries()
            for create in creates:
                mounts = list(map(Path, create[-2:]))
                assert [p.name for p in mounts] == ["worktree", "temp"]
                assert mounts[0].parent == mounts[1].parent
                assert (mounts[0]/"artifact").read_text() == "second"
                # Workspaces start empty: nothing from the operator's directory.
                assert not (mounts[0]/"seed.txt").exists()
                assert not any("workspace-history" in a for a in create)
            assert not (source/"artifact").exists()
            binary = hashlib.sha256(BIN.read_bytes()).hexdigest()
            for create in creates:
                assert (boxes/create[create.index("--name")+1]/"worker-sha").read_text() == binary
            first = record()
            assert first["sessions"] == {} and first["sessions_cleaned"]
            assert first["sandbox_template"] == TEMPLATE
            assert set(first["sandboxes"]) == {"a", "a.child", "b"}
            for ws in first["workspaces"].values():
                assert len(list((state_root/"workspace-history"/ws/"snapshots").glob("*.json"))) == 2
            # No host compilation of untrusted reaction bodies.
            assert not list(state_root.rglob("src-gen/**/main.rs"))
            # Cleanup failure blocks final and manual snapshots.
            fault(FAIL_STOP=1)
            omar(*args, "--replace")
            fault()
            unclean = record()
            assert not unclean["sessions_cleaned"], unclean
            for ws in unclean["workspaces"].values():
                assert len(list((state_root/"workspace-history"/ws/"snapshots").glob("*.json"))) == 1
                refused = omar("workspace", "snapshot", ws)
                assert refused.returncode != 0 and "sandbox cleanup is unconfirmed" in refused.stderr, refused
            # Unique VM names must not let a new run overwrite unclean ownership.
            refused = omar(*args)
            assert refused.returncode != 0 and "unconfirmed sandbox cleanup" in (refused.stdout + refused.stderr), refused
            assert record() == unclean
            ok("kill", "SandboxTeams")
            # Partial creation failures stop every persisted VM, including the
            # VM whose create returned an error after allocating resources.
            before = len(entries())
            fault(FAIL_CREATE=1)
            refused = omar(*args, "--replace")
            fault()
            assert refused.returncode != 0, refused
            failed = record()
            assert failed["state"] == "FAILED" and failed["sessions_cleaned"], failed
            created = {e[e.index("--name")+1] for e in entries(before) if e[0] == "create"}
            stopped = {e[1] for e in entries(before) if e[0] == "stop"}
            assert len(created) == 2 and created <= stopped, entries(before)
            # Allocation failure before a VM exists must not strand ownership.
            # Use a fresh VM inventory to simulate that failure.
            empty_boxes = root / "empty-boxes"
            empty_boxes.mkdir()
            fault(FAIL_BEFORE_CREATE=1, BOXES=str(empty_boxes))
            refused = omar(*args, "--replace")
            fault()
            assert refused.returncode != 0, refused
            assert record()["sessions_cleaned"], record()
            # Invalid pin fails before another create.
            before = calls.read_text()
            refused = omar("run", str(program), "--sandbox-template", "test:latest", "--fg")
            assert refused.returncode != 0 and calls.read_text() == before, refused

            # Pause stops the team's VM; resume pins the same template, mounts
            # the restored workspace in a new VM, and tells its agent so.
            def log_lines(create):
                path = Path(create[-2]) / "log.txt"
                return [int(l) for l in path.read_text().split()] if path.exists() else []

            def creates_since(since):
                return [e for e in entries(since) if e[0] == "create"]

            before = len(entries())
            run_id = json.loads(ok("run", str(ticker_program), "--sandbox-template", TEMPLATE,
                                   "--input", "w.go=1", "--checkpoint-period", "off"))["run_id"]
            wait_until(lambda: record_path("SandboxTicker") and record("SandboxTicker")["state"] == "RUNNING",
                       "the sandboxed run to start")
            [paused_vm] = creates_since(before)
            wait_until(lambda: len(log_lines(paused_vm)) >= 3, "three timer firings in the VM's worktree")
            paused_run = json.loads(ok("pause", "SandboxTicker", "--wait"))
            assert paused_run["status"] == "paused" and paused_run["run_id"] == run_id, paused_run
            paused = record("SandboxTicker")
            assert paused["state"] == "PAUSED" and paused["sessions_cleaned"], paused
            name = paused_vm[paused_vm.index("--name")+1]
            assert ["stop", name] in entries(before), entries(before)
            logged = log_lines(paused_vm)

            before = len(entries())
            resumed = json.loads(ok("resume", "SandboxTicker"))
            assert resumed["run_id"] == run_id, resumed
            wait_until(lambda: record("SandboxTicker")["state"] == "RUNNING"
                       and record("SandboxTicker").get("resumed_from") == paused["checkpoint"], "the resumed run")
            [resumed_vm] = creates_since(before)
            assert resumed_vm[resumed_vm.index("--name")+1] != name, "a resume gets a new VM"
            assert TEMPLATE in resumed_vm, resumed_vm
            assert record("SandboxTicker")["sandbox_template"] == TEMPLATE
            assert Path(resumed_vm[-2]) != Path(paused_vm[-2]), "a resume restores into a new workspace"
            wait_until(lambda: len(log_lines(resumed_vm)) >= len(logged) + 2, "the clock to continue in the new VM")
            assert log_lines(resumed_vm)[: len(logged)] == logged, "restored files differ"
            assert log_lines(paused_vm) == logged, "the paused workspace was written to"
            vm = boxes / resumed_vm[resumed_vm.index("--name")+1]
            prompt = next(vm.rglob("agents/w.agent/system.md")).read_text()
            assert f"resumed from checkpoint {paused['checkpoint']}" in prompt, prompt
            ok("stop", "SandboxTicker")
            wait_until(lambda: record("SandboxTicker")["state"] == "TERMINATED", "the stop")
            assert record("SandboxTicker")["sessions_cleaned"]
            assert ["stop", vm.name] in entries(before), entries(before)

            # Sandboxes are opt-in: without a template or config, the host.
            plain = [a for a in args if a not in ("--sandbox-template", TEMPLATE)] + ["--replace"]
            before = len(entries())
            result = omar(*plain)
            assert result.returncode == 0 and "warning" not in result.stderr, result
            assert not creates_since(before) and record().get("sandbox_template") is None
            # A named template is required, never silently replaced by the host.
            fault(SIGNED_OUT=1)
            refused = omar(*plain, "--sandbox-template", TEMPLATE)
            assert refused.returncode != 0 and "not signed in" in refused.stdout + refused.stderr, refused
            assert not creates_since(before)
            fault()
            # `[sandbox] enabled = true`: the published kit, no flag needed.
            (home / ".omar").mkdir(exist_ok=True)
            (home / ".omar" / "config.toml").write_text("[sandbox]\nenabled = true\n")
            enabled = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", "sandboxes-on", "--json"], cwd=source,
                                                env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
            runtimes.append(enabled)
            before = len(entries())
            result = omar(*plain, session=enabled["name"])
            assert result.returncode == 0, result
            creates = creates_since(before)
            assert len(creates) == 3 and all(c[-3].startswith(DEFAULT) for c in creates), creates
            # Opting out overrides the config.
            before = len(entries())
            result = omar(*plain, "--no-sandbox", session=enabled["name"])
            assert result.returncode == 0, result
            assert not creates_since(before)
            print("PASS: per-team workers, shared file order, native backend delivery, exact mounts, protected history, cleanup, pause/resume, opt-in, required template, enabled default kit, opt-out")
        except BaseException:
            log = state_root / "logs" / "runtime.log"
            if log.exists():
                print("runtime.log:", log.read_text()[-4000:])
            raise
        finally:
            for started in runtimes:
                subprocess.run([str(BIN), "down", "-s", started["name"], "--force", "--timeout", "5"], env=env, capture_output=True)
                subprocess.run(["tmux", "-L", started["tmux_server"], "kill-server"], capture_output=True)
            for box in list(boxes.iterdir()) + list((root / "empty-boxes").glob("*")):
                subprocess.run([str(sbx), "stop", box.name], env=env, capture_output=True)


if __name__ == "__main__":
    main()
