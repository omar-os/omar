#!/usr/bin/env python3
"""Real topology launch, per-instance cwd/env, file snapshots, and CLI restore.

No model credentials or Lean compiler required: a compiler fixture emits fixed
bytecode; real Rust bodies and the built-in stub backend execute it.
Every tmux command uses a private server.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import uuid

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()


def main():
    tmux = shutil.which("tmux")
    assert tmux, "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-workspaces-", dir="/tmp") as directory:
        root = Path(directory)
        home, source, shims = root / "home", root / "source", root / "bin"
        for path in (home, source, shims):
            path.mkdir()
        (source / "seed.txt").write_text("original source")
        launches = root / "tmux.jsonl"
        wrapper = shims / "tmux"
        wrapper.write_text("#!/usr/bin/env python3\nimport json, os, sys\n"
                           f"with open({str(launches)!r}, 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                           "if 'kill-session' in sys.argv and os.environ.get('OMAR_TEST_KILL_FAIL'): sys.exit(1)\n"
                           f"os.execv({tmux!r}, [{tmux!r}, *sys.argv[1:]])\n")
        wrapper.chmod(0o700)
        server = "omar-workspace-" + uuid.uuid4().hex[:12]
        env = dict(os.environ, HOME=str(home), OMAR_TMUX_SERVER=server,
                   PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))

        def run(*args):
            result = subprocess.run([str(BIN), *args], cwd=source, env=env,
                                    text=True, capture_output=True, timeout=180)
            assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result.stdout

        instructions = [{"op": "begin_plan", "team": "Workspaces"}]
        for instance, parent in [("left", ""), ("left.child", "left"), ("right", "")]:
            instructions.append(dict(op="declare_instance", name=instance, team="Writer", parent=parent))
            for kind, name, ty in [("input", "tick", "int"), ("output", "out", "string")]:
                instructions.append(dict(op="define_port", kind=kind, name=f"{instance}.{name}", type=ty, instance=instance))
            instructions.append(dict(op="install_reaction", id=f"{instance}.reaction", instance=instance,
                                     agent="", triggers=[f"{instance}.tick"], effects=[f"{instance}.out"],
                                     contract=f"{instance}.out", prompt="", body='''
std::fs::write("artifact.bin", b"\\0\\xff\\r\\n").unwrap();
let temp = std::env::var("OMAR_TEMP").unwrap();
assert_eq!(std::env::var("TMPDIR").unwrap(), temp);
std::fs::write(std::path::Path::new(&temp).join("discard"), "temporary").unwrap();
let cwd = std::env::current_dir().unwrap();
assert_eq!(cwd, std::path::PathBuf::from(std::env::var("OMAR_WORKTREE").unwrap()).canonicalize().unwrap());
out = Some(cwd.display().to_string());
'''))
        for instance, name in [("left", "a"), ("left", "b"), ("right", "a")]:
            instructions.append(dict(op="spawn_agent", name=f"{instance}.{name}", instance=instance, backend="stub"))
        instructions.append({"op": "commit_plan"})
        bytecode = root / "program.json"
        bytecode.write_text(json.dumps(dict(version=1, team="Workspaces", instructions=instructions)))
        program = root / "program.omar"
        program.write_text("// Compiler-fixture input; the test exercises the runtime, not parsing.\n")
        compiler = shims / "omarc"
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\n"
                            f"shutil.copyfile({str(bytecode)!r}, sys.argv[2])\n")
        compiler.chmod(0o700)
        env["OMARC_BIN"] = str(compiler)
        try:
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            workspaces = json.loads(run("workspace", "list"))
            assert len(workspaces) == 3, workspaces
            by_instance = {w["instance"]: w for w in workspaces}
            root_state = home / ".omar"
            paths = {name: root_state / "workspaces" / ws["id"] / "worktree" for name, ws in by_instance.items()}
            for path in paths.values():
                assert (path / "artifact.bin").read_bytes() == b"\0\xff\r\n"
                assert (path / "seed.txt").read_text() == "original source"
            assert not (source / "artifact.bin").exists()
            spawned = [json.loads(line) for line in launches.read_text().splitlines() if "new-session" in json.loads(line)]
            assert len(spawned) == 3, spawned
            cwd_counts = {}
            for args in spawned:
                cwd = args[args.index("-c") + 1]
                cwd_counts[cwd] = cwd_counts.get(cwd, 0) + 1
                assert f"OMAR_WORKTREE={cwd}" in args
                assert f"OMAR_TEMP={Path(cwd).parent / 'temp'}" in args
            assert cwd_counts == {str(paths["left"]): 2, str(paths["right"]): 1}, cwd_counts
            selected = by_instance["left"]["id"]
            snapshot = json.loads(run("workspace", "snapshot", selected, "--label", "known files"))
            (paths["left"] / "artifact.bin").write_bytes(b"newer files")
            restored = json.loads(run("workspace", "restore", selected, snapshot["id"]))
            assert Path(restored["worktree"]).joinpath("artifact.bin").read_bytes() == b"\0\xff\r\n"
            assert not list(Path(restored["temp"]).iterdir())
            assert (paths["left"] / "artifact.bin").read_bytes() == b"newer files"
            show = json.loads(run("workspace", "show", selected))
            assert {s["label"] for s in show["snapshots"]} >= {"Initial workspace", "Final topology files", "known files"}
            # A failed cleanup is not evidence that agents stopped writing.
            existing = {w["id"] for w in json.loads(run("workspace", "list"))}
            env["OMAR_TEST_KILL_FAIL"] = "1"
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            later = [w for w in json.loads(run("workspace", "list")) if w["id"] not in existing]
            assert len(later) == 3
            for workspace in later:
                snapshots = json.loads(run("workspace", "show", workspace["id"]))["snapshots"]
                assert [s["label"] for s in snapshots] == ["Initial workspace"], snapshots
            print("PASS: team workspaces, nested ownership, agent launch, Rust cwd/env, snapshots, and CLI restore")
        finally:
            subprocess.run([tmux, "-L", server, "kill-server"], capture_output=True)


if __name__ == "__main__":
    main()
