#!/usr/bin/env python3
"""Real topology launch, per-instance cwd/env, file snapshots, and CLI restore.

No model credentials or Lean compiler required: a compiler fixture emits fixed
bytecode; real Rust bodies and the built-in stub backend execute it.
One runtime session runs every topology here, on its own tmux server. The
tmux fixture on PATH reads its fault injection from a file, since the runtime,
not this test, is the process that launches agents.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import shlex
import uuid

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()


def main():
    tmux = shutil.which("tmux")
    assert tmux, "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-workspaces-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source, shims = root / "home", root / "source", root / "bin"
        for path in (home, source, shims):
            path.mkdir()
        (source / "seed.txt").write_text("stays in the operator's directory")
        launches = root / "tmux.jsonl"
        sidecar = root / "sidecar.py"
        sidecar.write_text("import os, signal, time\nfrom pathlib import Path\n"
                           "signal.signal(signal.SIGHUP, signal.SIG_IGN)\n"
                           "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                           "path = Path(os.environ['OMAR_TEMP']) / ('sidecar-' + str(os.getpid()))\n"
                           "while True:\n path.write_text(str(time.monotonic_ns()))\n time.sleep(0.02)\n")
        wrapper = shims / "tmux"
        toggles = root / "toggles.json"
        wrapper.write_text("#!/usr/bin/env python3\nimport json, os, sys, subprocess\n"
                           f"with open({str(launches)!r}, 'a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n"
                           f"try: faults = json.load(open({str(toggles)!r}))\n"
                           "except FileNotFoundError: faults = {}\n"
                           f"if faults.get('SERVER_VANISH') and '#{{pane_pid}}' in sys.argv: subprocess.run([{tmux!r}, '-L', os.environ['OMAR_TMUX_SERVER'], 'kill-server'], capture_output=True)\n"
                           "if faults.get('KILL_FAIL') and ('kill-session' in sys.argv or '#{pane_pid}' in sys.argv): sys.exit(1)\n"
                           f"if 'new-session' in sys.argv and (not faults.get('KILL_FAIL') or faults.get('REPLACE_SIDECAR')): sys.argv[-1] = {('python3 ' + shlex.quote(str(sidecar)) + ' & ')!r} + sys.argv[-1]\n"
                           f"os.execv({tmux!r}, [{tmux!r}, *sys.argv[1:]])\n")
        wrapper.chmod(0o700)
        env = dict(os.environ, HOME=str(home),
                   PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)

        def fault(**active):
            toggles.write_text(json.dumps(active))

        def omar(*args, **kwargs):
            return subprocess.run([str(BIN), "-s", runtime["name"], *args], cwd=source, env=env,
                                  text=True, capture_output=True, timeout=kwargs.get("timeout", 180))

        def run(*args):
            if args[0] == "run":
                args = (*args, "--wait")
            result = omar(*args)
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
std::process::Command::new("sh").args(["-c", r#"trap '' TERM HUP; while :; do printf x >> "$OMAR_TEMP/reaction-heartbeat"; sleep 0.02; done"#])
    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
let heartbeat = std::path::Path::new(&temp).join("reaction-heartbeat");
while !heartbeat.exists() { std::thread::sleep(std::time::Duration::from_millis(10)); }

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
        # omarc [options] <input> <output>: the output is the last argument.
        compiler.write_text("#!/usr/bin/env python3\nimport shutil, sys\n"
                            f"shutil.copyfile({str(bytecode)!r}, sys.argv[-1])\n")
        compiler.chmod(0o700)
        env["OMARC_BIN"] = str(compiler)
        runtime = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "--name", "workspaces", "--json"], cwd=source,
                                            env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
        server = runtime["tmux_server"]
        try:
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            workspaces = json.loads(run("workspace", "list"))
            assert len(workspaces) == 3, workspaces
            by_instance = {w["instance"]: w for w in workspaces}
            root_state = Path(runtime["directory"])
            paths = {name: root_state / "workspaces" / ws["id"] / "worktree" for name, ws in by_instance.items()}
            for path in paths.values():
                assert (path / "artifact.bin").read_bytes() == b"\0\xff\r\n"
                # A worktree starts empty: the operator's directory is not copied in.
                assert sorted(p.name for p in path.iterdir()) == [".git", "artifact.bin"], list(path.iterdir())
            assert not (source / "artifact.bin").exists()
            heartbeats = {p: p.read_bytes() for tree in paths.values()
                          for p in (tree.parent / "temp").glob("sidecar-*")}
            assert len(heartbeats) == 3, heartbeats
            reaction_heartbeats = {tree.parent / "temp" / "reaction-heartbeat":
                                   (tree.parent / "temp" / "reaction-heartbeat").read_bytes()
                                   for tree in paths.values()}
            time.sleep(0.2)
            assert all(p.read_bytes() == value for p, value in heartbeats.items()), "sidecar survived teardown"
            assert all(p.read_bytes() == value for p, value in reaction_heartbeats.items()), "reaction descendant survived final snapshot"
            spawned = [json.loads(line) for line in launches.read_text().splitlines() if "new-session" in json.loads(line)]
            assert len(spawned) == 3, spawned
            cwd_counts = {}
            for args in spawned:
                cwd = args[args.index("-c") + 1]
                cwd_counts[cwd] = cwd_counts.get(cwd, 0) + 1
                assert f"OMAR_WORKTREE={cwd}" in args
                assert f"OMAR_TEMP={Path(cwd).parent / 'temp'}" in args
            assert cwd_counts == {str(paths["left"]): 2, str(paths["right"]): 1}, cwd_counts
            prompts = list(root_state.glob("**/agents/*/system.md"))
            assert len(prompts) == 3, prompts
            for prompt in prompts:
                text = prompt.read_text()
                assert "inspect, create, and edit files and run commands" in text, text
                assert "For topology communication, use omar_set_port only" in text, text
                assert "then call omar_complete to finish" in text, text
                assert "use only omar_set_port" not in text, text
                assert any(f"workspace is {path}." in text for path in paths.values()), text
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
            fault(KILL_FAIL=True)
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            later = [w for w in json.loads(run("workspace", "list")) if w["id"] not in existing]
            assert len(later) == 3
            for workspace in later:
                snapshots = json.loads(run("workspace", "show", workspace["id"]))["snapshots"]
                assert [s["label"] for s in snapshots] == ["Initial workspace"], snapshots
                refused = omar("workspace", "snapshot", workspace["id"], timeout=30)
                assert refused.returncode != 0, refused.stdout
                assert "stop remaining topology sessions" in refused.stderr, refused.stderr
            # Force cleanup must target the launch server, never some other server.
            fault()
            records = list(root_state.glob("**/topologies/Workspaces/deployment.json"))
            assert len(records) == 1, records
            record = json.loads(records[0].read_text())
            other_server = server + "-other"
            for session in record["sessions"].values():
                subprocess.run([tmux, "-L", other_server, "new-session", "-d", "-s", session,
                                "sleep 300"], check=True, capture_output=True)
            cleanup = omar("kill", "Workspaces", timeout=30)
            assert cleanup.returncode == 0, cleanup.stderr
            for session in record["sessions"].values():
                assert subprocess.run([tmux, "-L", server, "has-session", "-t", session],
                                      capture_output=True).returncode != 0
                assert subprocess.run([tmux, "-L", other_server, "has-session", "-t", session],
                                      capture_output=True).returncode == 0, "killed caller's session"
            run("workspace", "snapshot", later[0]["id"], "--label", "after recorded-server cleanup")
            subprocess.run([tmux, "-L", other_server, "kill-server"], check=True, capture_output=True)

            # (Every run here shares the session's tmux server, so a second
            # deployment cannot hide an older one on another server.)
            # Replacing leftover sessions must also terminate their sidecars.
            fault(KILL_FAIL=True, REPLACE_SIDECAR=True)
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            replaced = json.loads(records[0].read_text())
            old_heartbeats = [p for workspace_id in replaced["workspaces"].values()
                              for p in (root_state / "workspaces" / workspace_id / "temp").glob("sidecar-*")]
            assert len(old_heartbeats) == 3, old_heartbeats
            before = {p: p.read_bytes() for p in old_heartbeats}
            time.sleep(0.1)
            assert all(p.read_bytes() != value for p, value in before.items()), "fixture writers did not start"
            fault()
            # Simulate a crashed active run, then replace it.
            replaced["state"] = "RUNNING"
            replaced["pid"] = 4294967295
            records[0].write_text(json.dumps(replaced))
            fault(KILL_FAIL=True)
            refused = omar("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                           "--input", "right.tick=1", "--replace", "--fast", "--wait", timeout=60)
            assert refused.returncode != 0 and "cannot replace deployment" in refused.stderr, refused
            assert json.loads(records[0].read_text()) == replaced, "failed cleanup retired the old run"
            fault()
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast", "--replace")
            after = {p: p.read_bytes() for p in old_heartbeats}
            time.sleep(0.2)
            assert all(p.read_bytes() == value for p, value in after.items()), "replacement left old workspace writers alive"
            run("workspace", "snapshot", replaced["workspaces"]["left"], "--label", "after replacement cleanup")
            archived = records[0].parent / "deployments" / (replaced["deployment_id"] + ".json")
            assert json.loads(archived.read_text())["state"] == "CANCELLED"
            for session in replaced["sessions"].values():
                assert subprocess.run([tmux, "-L", other_server, "has-session", "-t", session],
                                      capture_output=True).returncode != 0

            # Missing legacy identity must not become an explicit default server.
            current = records[0].read_text()
            legacy = json.loads(current)
            del legacy["tmux_server"]
            records[0].write_text(json.dumps(legacy))
            for args in [("kill", "Workspaces"),
                         ("workspace", "snapshot", legacy["workspaces"]["left"]),
                         ("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                          "--input", "right.tick=1", "--replace", "--fast", "--wait")]:
                refused = omar(*args, timeout=60)
                assert refused.returncode != 0 and "no recorded tmux server" in refused.stderr, refused
                assert json.loads(records[0].read_text()) == legacy
            records[0].write_text(current)
            # Losing the server during teardown is not proof that its sidecars stopped.
            fault(SERVER_VANISH=True)
            run("run", str(program), "--input", "left.tick=1", "--input", "left.child.tick=1",
                "--input", "right.tick=1", "--fast")
            vanished = json.loads(records[0].read_text())
            assert not vanished["sessions_cleaned"]
            for workspace_id in vanished["workspaces"].values():
                versions = json.loads(run("workspace", "show", workspace_id))["snapshots"]
                assert [v["label"] for v in versions] == ["Initial workspace"]
                refused = omar("workspace", "snapshot", workspace_id, timeout=30)
                assert refused.returncode != 0 and "tmux session state is unknown" in refused.stderr, refused
            print("PASS: team workspaces, nested ownership, agent launch, Rust cwd/env, snapshots, and CLI restore")
        except BaseException:
            log = root_state / "logs" / "runtime.log"
            if log.exists():
                print("runtime.log:", log.read_text()[-4000:])
            raise
        finally:
            subprocess.run([str(BIN), "down", "-s", runtime["name"], "--force", "--timeout", "5"], env=env, capture_output=True)
            subprocess.run([tmux, "-L", server, "kill-server"], capture_output=True)
            subprocess.run([tmux, "-L", server + "-other", "kill-server"], capture_output=True)
            for socket in (root / "tmux-default").glob("tmux-*/*"):
                subprocess.run([tmux, "-S", str(socket), "kill-server"], capture_output=True)


if __name__ == "__main__":
    main()
