#!/usr/bin/env python3
"""A team's `setup` runs in each instance's workspace before any agent launches.

Real `omarc` and `omar run`, the built-in stub backend, and a tmux shim that
records, at the moment an agent's session is created, whether the files setup
writes were already in its workspace. Needs `cargo build` and `lake build`.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
COMPILER = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()


def program(setup):
    return f"""team Seeded[agent : Stub] {{
    {setup}
    input go : string
    output out : string
    go -> out after 0
}}
main {{ s = Seeded() }}
"""


def main():
    tmux = shutil.which("tmux")
    assert tmux, "tmux is required"
    assert COMPILER.is_file(), "build omarc before this test"
    with tempfile.TemporaryDirectory(prefix="omar-setup-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source, shims = root / "home", root / "source", root / "bin"
        for path in (home, source, shims):
            path.mkdir()
        launches = root / "tmux.jsonl"
        wrapper = shims / "tmux"
        wrapper.write_text("#!/usr/bin/env python3\nimport json, os, sys\n"
                           "args = sys.argv[1:]\n"
                           "if 'new-session' in args:\n"
                           "    cwd = args[args.index('-c') + 1]\n"
                           f"    with open({str(launches)!r}, 'a') as f:\n"
                           "        f.write(json.dumps({'cwd': cwd, 'seeded': os.path.exists(os.path.join(cwd, 'seed.txt'))}) + '\\n')\n"
                           f"os.execv({tmux!r}, [{tmux!r}, *args])\n")
        wrapper.chmod(0o700)
        env = dict(os.environ, HOME=str(home), OMARC_BIN=str(COMPILER),
                   PATH=str(shims) + os.pathsep + os.environ["PATH"],
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        runtime = json.loads(subprocess.run([str(BIN), "up", "--no-ea", "-s", "setup", "--json"], cwd=source,
                                            env=env, text=True, capture_output=True, timeout=90, check=True).stdout)
        state = Path(runtime["directory"])

        def run(setup):
            path = root / "seeded.omar"
            path.write_text(program(setup))
            launches.write_text("")
            return subprocess.run([str(BIN), "-s", "setup", "run", str(path), "--input", "s.go=hi",
                                   "--fast", "--fg", "--keep-workspaces", "--replace"],
                                  cwd=source, env=env, text=True, capture_output=True, timeout=180)

        try:
            # Success: the files are there when the agent's session is created.
            ok = run('setup "echo seeded > seed.txt; echo \\"$OMAR_WORKTREE\\" > where.txt"')
            assert ok.returncode == 0, ok.stdout + ok.stderr
            started = [json.loads(line) for line in launches.read_text().splitlines()]
            assert len(started) == 1 and started[0]["seeded"], started
            worktree = Path(started[0]["cwd"])
            assert (worktree / "seed.txt").read_text() == "seeded\n"
            assert (worktree / "where.txt").read_text().strip() == str(worktree), "setup runs with a worker's environment"
            record = json.loads(next(state.glob("**/topologies/Seeded/setup/s.json")).read_text())
            assert record["exit_code"] == 0 and not record["timed_out"], record

            # Failure: the run fails with the command's output, before any launch.
            bad = run('setup "echo nope >&2; exit 3"')
            assert bad.returncode != 0, bad.stdout
            assert "exited 3" in bad.stdout + bad.stderr and "nope" in bad.stdout + bad.stderr, bad.stdout + bad.stderr
            assert launches.read_text() == "", "an agent launched after a failed setup"
            record = json.loads(next(state.glob("**/topologies/Seeded/setup/s.json")).read_text())
            assert record["exit_code"] == 3 and "nope" in record["output_tail"], record

            # An empty setup does not compile.
            empty = run('setup ""')
            assert empty.returncode != 0 and "setup command is empty" in empty.stdout + empty.stderr, empty.stdout + empty.stderr
            assert launches.read_text() == ""
            print("workspace setup: ok")
        finally:
            subprocess.run([str(BIN), "-s", "setup", "rm", "--force"], cwd=source, env=env,
                           capture_output=True, timeout=60)
            subprocess.run([str(BIN), "-s", "setup", "rm"], cwd=source, env=env, capture_output=True, timeout=60)


if __name__ == "__main__":
    main()
