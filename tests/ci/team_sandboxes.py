#!/usr/bin/env python3
"""Sandbox transport/lifecycle contracts. The sbx fixture is NOT VM isolation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

BIN = Path(os.environ.get("OMAR_BIN", "target/debug/omar")).resolve()
TEMPLATE = "docker.io/test/omar@sha256:" + "a" * 64


def main():
    with tempfile.TemporaryDirectory(prefix="omar-sandbox-contract-") as tmp:
        root = Path(tmp)
        home, source, boxes = root / "home", root / "source", root / "boxes"
        for path in (home, source, boxes):
            path.mkdir()
        (source / "seed.txt").write_text("seed")
        history = home / ".omar"
        calls = root / "calls.jsonl"
        sbx = root / "sbx"
        sbx.write_text("""#!/usr/bin/env python3
import json, os, signal, subprocess, sys
from pathlib import Path
root = Path(os.environ["FAKE_BOXES"])
args = sys.argv[1:]
with open(os.environ["FAKE_CALLS"], "a") as f: f.write(json.dumps(args)+"\\n")
if args[0] == "create":
    name = args[args.index("--name")+1]
    box = root / name
    if os.environ.get("FAIL_BEFORE_CREATE") and len(list(root.iterdir())) % 3 == 0: sys.exit(1)
    box.mkdir()
    (box/"mounts").write_text(json.dumps(args[-2:]))
    if os.environ.get("FAIL_CREATE") and len(list(root.iterdir())) % 3 == 2: sys.exit(1)
elif args[0] == "exec":
    name = next(a for a in args if a.startswith("omar-team-"))
    box = root / name
    (box/"pid").write_text(str(os.getpid()))
    env = dict(os.environ, HOME=str(box), OMAR_TMUX_SERVER=name)
    os.chdir(args[args.index("--workdir")+1])
    os.execve(os.environ["REAL_OMAR"], [os.environ["REAL_OMAR"], "sandbox-worker"], env)
elif args[0] == "stop":
    if os.environ.get("FAIL_STOP"): sys.exit(1)
    box = root/args[1]
    if not box.exists(): sys.exit(1)
    subprocess.run(["tmux", "-L", args[1], "kill-server"], capture_output=True)
    if (box/"pid").exists():
        try: os.kill(int((box/"pid").read_text()), signal.SIGTERM)
        except ProcessLookupError: pass
elif args[0] == "ls":
    print("\\n".join(p.name for p in root.iterdir()))
else: sys.exit(2)
""")
        sbx.chmod(0o700)
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
        instructions.append({"op": "commit_plan"})
        bytecode = root / "program.json"
        bytecode.write_text(json.dumps(dict(version=1, team="SandboxTeams", instructions=instructions)))
        program = root / "program.omar"
        program.write_text("// Fixture compiler input\n")
        compiler = root / "omarc"
        compiler.write_text("#!/usr/bin/env python3\nimport shutil,sys\nshutil.copyfile("+repr(str(bytecode))+", sys.argv[2])\n")
        compiler.chmod(0o700)
        env = dict(os.environ, HOME=str(home), REAL_OMAR=str(BIN), OMAR_SBX_BIN=str(sbx),
                   FAKE_BOXES=str(boxes), FAKE_CALLS=str(calls), OMARC_BIN=str(compiler),
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home()/".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home()/".rustup")))
        args = ["run", str(program), "--sandbox-template", TEMPLATE, "--fast"]
        for instance in ("a", "a.child", "b"):
            args += ["--input", instance+".tick=1"]

        def run(*args, extra=None):
            return subprocess.run([str(BIN), *args], cwd=source, env={**env, **(extra or {})},
                                  capture_output=True, text=True, timeout=240)
        try:
            result = run(*args)
            assert result.returncode == 0, (result.stdout, result.stderr, [(str(p), p.read_text()) for p in history.rglob("sandbox-logs/*.log")])
            entries = [json.loads(line) for line in calls.read_text().splitlines()]
            creates = [e for e in entries if e[0] == "create"]
            stops = [e for e in entries if e[0] == "stop"]
            assert len(creates) == len(stops) == 3, entries
            for create in creates:
                mounts = list(map(Path, create[-2:]))
                assert [p.name for p in mounts] == ["worktree", "temp"]
                assert mounts[0].parent == mounts[1].parent
                assert (mounts[0]/"artifact").read_text() == "second"
                assert (mounts[0]/"seed.txt").read_text() == "seed"
                assert not any("workspace-history" in a for a in create)
            assert not (source/"artifact").exists()
            records_found = list(history.rglob("deployment.json"))
            assert len(records_found) == 1, records_found
            record = json.loads(records_found[0].read_text())
            assert record["sessions"] == {} and record["sessions_cleaned"]
            assert record["sandbox_template"] == TEMPLATE
            assert set(record["sandboxes"]) == {"a", "a.child", "b"}
            for ws in record["workspaces"].values():
                assert len(list((history/"workspace-history"/ws/"snapshots").glob("*.json"))) == 2
            # No host compilation of untrusted reaction bodies.
            assert not list((root/"src-gen").rglob("main.rs"))
            # Cleanup failure blocks final and manual snapshots.
            result = run(*args, "--replace", extra={"FAIL_STOP": "1"})
            assert "skipped final snapshot" in result.stderr, result
            record = json.loads(records_found[0].read_text())
            assert not record["sessions_cleaned"]
            for ws in record["workspaces"].values():
                assert len(list((history/"workspace-history"/ws/"snapshots").glob("*.json"))) == 1
                refused = run("workspace", "snapshot", ws)
                assert refused.returncode != 0 and "sandbox cleanup is unconfirmed" in refused.stderr, refused
            # Unique VM names must not let a new run overwrite unclean ownership.
            refused = run(*args)
            assert refused.returncode != 0 and "unconfirmed sandbox cleanup" in refused.stderr, refused
            assert json.loads(records_found[0].read_text()) == record
            assert run("kill", "SandboxTeams").returncode == 0
            # Partial creation failures stop every persisted VM, including the
            # VM whose create returned an error after allocating resources.
            before_entries = len(calls.read_text().splitlines())
            refused = run(*args, "--replace", extra={"FAIL_CREATE": "1"})
            assert refused.returncode != 0, refused
            failed = json.loads(records_found[0].read_text())
            assert failed["state"] == "FAILED" and failed["sessions_cleaned"], failed
            entries = [json.loads(line) for line in calls.read_text().splitlines()[before_entries:]]
            created = {e[e.index("--name")+1] for e in entries if e[0] == "create"}
            stopped = {e[1] for e in entries if e[0] == "stop"}
            assert len(created) == 2 and created <= stopped, entries
            # Allocation failure before a VM exists must not strand ownership.
            # Use a fresh VM inventory to simulate that failure.
            empty_boxes = root / "empty-boxes"
            empty_boxes.mkdir()
            refused = run(*args, "--replace", extra={"FAIL_BEFORE_CREATE": "1", "FAKE_BOXES": str(empty_boxes)})
            assert refused.returncode != 0, refused
            failed = json.loads(records_found[0].read_text())
            assert failed["sessions_cleaned"], failed
            # Invalid pin fails before another create.
            before = calls.read_text()
            refused = run("run", str(program), "--sandbox-template", "test:latest")
            assert refused.returncode != 0 and calls.read_text() == before
            print("PASS: per-team workers, shared file order, native backend delivery, exact mounts, protected history, cleanup")
        finally:
            for box in boxes.iterdir():
                subprocess.run([str(sbx), "stop", box.name], env=env, capture_output=True)


if __name__ == "__main__":
    main()
