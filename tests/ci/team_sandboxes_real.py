#!/usr/bin/env python3
"""Real microVM smoke test. Requires signed-in sbx and a pinned OMAR template.
OMAR_SANDBOX_TEMPLATE=<image@sha256:...> python3 tests/ci/team_sandboxes_real.py
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

BIN = Path(os.environ.get("OMAR_BIN", "target/debug/omar")).resolve()
SBX = os.environ.get("OMAR_SBX_BIN", "sbx")


def main():
    template = os.environ["OMAR_SANDBOX_TEMPLATE"]
    with tempfile.TemporaryDirectory(prefix="omar-real-sandbox-") as tmp:
        root = Path(tmp).resolve()
        home, source = root / "home", root / "source"
        home.mkdir()
        source.mkdir()
        secret = root / "host-only.txt"
        secret.write_text("host-only")
        (source / "seed.txt").write_text("seed")
        state_root = home / ".omar"
        # Source paths embedded in the reaction are deliberately not mounts.
        body = f"""
assert!(std::fs::read_to_string({json.dumps(str(secret))}).is_err());
assert!(std::fs::read_dir({json.dumps(str(state_root / "workspace-history"))}).is_err());
assert!(std::fs::read_to_string({json.dumps(str(source / "seed.txt"))}).is_err());
assert_eq!(std::fs::read_to_string("seed.txt").unwrap(), "seed");
assert!(std::fs::write({json.dumps(str(secret))}, "changed").is_err());
std::fs::write("artifact", "inside sandbox").unwrap();
out = Some("isolated".to_string());
"""
        instructions = [{"op": "begin_plan", "team": "IsolationSmoke"}]
        for instance, parent in [("parent", ""), ("parent.child", "parent")]:
            instructions += [
                dict(op="declare_instance", name=instance, parent=parent, team="Writer"),
                dict(op="define_port", instance=instance, kind="input", name=instance+".tick", type="int"),
                dict(op="define_port", instance=instance, kind="output", name=instance+".out", type="string"),
                dict(op="spawn_agent", instance=instance, name=instance+".agent", backend="stub"),
                dict(op="install_reaction", instance=instance, id=instance+".write", agent="",
                     triggers=[instance+".tick"], effects=[instance+".out"], contract=instance+".out", prompt="", body=body),
                dict(op="install_reaction", instance=instance, id=instance+".prompt", agent=instance+".agent",
                     triggers=[instance+".tick"], effects=[instance+".out"], contract=instance+".out", prompt="Return a string."),
            ]
        instructions.append({"op": "commit_plan"})
        bytecode = root / "program.json"
        bytecode.write_text(json.dumps(dict(version=1, team="IsolationSmoke", instructions=instructions)))
        program = root / "program.omar"
        program.write_text("// runtime isolation fixture\n")
        compiler = root / "omarc"
        compiler.write_text("#!/usr/bin/env python3\nimport shutil,sys\nshutil.copyfile("+repr(str(bytecode))+", sys.argv[2])\n")
        compiler.chmod(0o700)
        # Keep sbx's real HOME for its Docker login and daemon. OMAR uses a
        # dedicated HOME; this adapter restores only sbx's host environment.
        adapter = root / "sbx"
        adapter.write_text("#!/usr/bin/env python3\nimport os,sys\nos.environ['HOME']="+repr(str(Path.home()))+
                           "\nos.execvp("+repr(SBX)+", ["+repr(SBX)+", *sys.argv[1:]])\n")
        adapter.chmod(0o700)
        env = dict(os.environ, HOME=str(home), OMAR_SBX_BIN=str(adapter), OMARC_BIN=str(compiler))
        try:
            result = subprocess.run([str(BIN), "run", str(program), "--sandbox-template", template,
                "--input", "parent.tick=1", "--input", "parent.child.tick=1", "--fast"],
                cwd=source, env=env, text=True, capture_output=True, timeout=600)
            assert result.returncode == 0, (result.stdout, result.stderr,
                [(str(p), p.read_text()) for p in state_root.rglob("sandbox-logs/*.log")])
            record = json.loads(next(state_root.rglob("deployment.json")).read_text())
            assert record["sessions_cleaned"] and len(record["sandboxes"]) == 2
            assert secret.read_text() == "host-only" and not (source/"artifact").exists()
            for ws in record["workspaces"].values():
                tree = state_root/"workspaces"/ws/"worktree"
                assert (tree/"artifact").read_text() == "inside sandbox"
                assert len(list((state_root/"workspace-history"/ws/"snapshots").glob("*.json"))) == 2
            print("PASS: real Docker Sandbox execution, parent/child separation, denied host/history access, native backend, snapshots")
        finally:
            for path in state_root.rglob("deployment.json"):
                for name in json.loads(path.read_text()).get("sandboxes", {}).values():
                    subprocess.run([SBX, "stop", name], capture_output=True, timeout=60)
                    subprocess.run([SBX, "rm", name], capture_output=True, timeout=60)


if __name__ == "__main__":
    main()
