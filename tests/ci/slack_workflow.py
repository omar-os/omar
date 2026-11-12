#!/usr/bin/env python3
"""Check the Slack messaging contract without a configuration workflow registry."""
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[2]
subprocess.run(["cargo", "test", "--locked", "--bin", "omar", "messaging::"],
               cwd=root, check=True)
