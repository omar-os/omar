#!/usr/bin/env python3
"""Replay the guided journey through real native plumbing using a local test double."""
import argparse
import json
import os
import shutil
import sys
from pathlib import Path
from unittest.mock import patch

import designer_journey as journey
from evaluation_protocol import ROOT, REPO, file_digest, write


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--omar", type=Path, default=REPO / "target/debug/omar")
    parser.add_argument("--compiler", type=Path, default=REPO / "lang/.lake/build/bin/omarc")
    args = parser.parse_args()
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=False)
    binary_dir = output / "bin"; binary_dir.mkdir()
    stub = binary_dir / "codex"
    stub.write_text("#!" + sys.executable + "\n" + (ROOT / "scripted_designer_backend.py").read_text())
    stub.chmod(0o700)
    for name in ["tmux"]:
        executable = shutil.which(name)
        if not executable: raise RuntimeError(f"{name} is required")
        (binary_dir / name).symlink_to(executable)
    # Exclude all installed provider CLI directories. There is no real Codex
    # executable on this PATH. The test double imports no provider client.
    env = {"PATH": str(binary_dir) + ":/usr/bin:/bin:/usr/sbin:/sbin", "SHELL": "/bin/sh",
        "OMAR_DESIGNER_FIXTURE_ROOT": str(ROOT), "OMAR_DESIGNER_FIXTURE_DIR": str(output), "OMAR_DESIGNER_FIXTURE_BINARY": str(args.omar.resolve())}
    original = journey.command
    with patch.dict(os.environ, env), patch.object(journey, "command", lambda p: str(stub) + original(p)[len("codex"):]):
        protocol = journey.prepare(output / "packet", args.omar, args.compiler)
        result = journey.execute(output / "packet/protocol.json", authorized=True)
    write(output / "verification.json", {"mode": "scripted native protocol test, not model behavior/access/quality", "model_calls": 0,
        "stub_digest": file_digest(stub), "protocol_digest": protocol["digest"], "status": result["status"], "quality": result["quality"], "human_acceptance": None})
    print(json.dumps(result, indent=2))
    if result["status"] != "completed" or not (result["quality"] or {}).get("deterministic_pass"): raise SystemExit(1)


if __name__ == "__main__": main()
