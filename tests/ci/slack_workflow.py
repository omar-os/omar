#!/usr/bin/env python3
"""Exercise messaging against OMAR's saved workflow and runtime lifecycle APIs."""
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[2]
env = dict(os.environ)
env["OMARC_BIN"] = str(root / "lang/.lake/build/bin/omarc")
subprocess.run([
    "cargo", "test", "--locked", "--bin", "omar",
    "proposed_workflow_is_registered_only_after_approval_and_runs_through_messaging",
    "--", "--ignored", "--nocapture",
], cwd=root, env=env, check=True)
subprocess.run([
    "cargo", "test", "--locked", "--bin", "omar",
    "messaging::connect::tests::", "--", "--nocapture",
], cwd=root, env=env, check=True)
print("PASS: saved proposal approval -> runtime workflow API -> messaging -> real workflow output")
