#!/usr/bin/env python3
"""Bounded deterministic work for the live hierarchy benchmark (no orchestration)."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(__file__).resolve().parent
role = sys.argv[1]
assert role in ['left', 'right']
# Attribute execution to a real pane instead of accepting a model's self-report.
panes = subprocess.check_output(['tmux', '-L', os.environ['OMAR_TMUX_SERVER'],
    'list-panes', '-a', '-F', '#{pane_pid} #{session_name}'], text=True)
parents = dict(line.split(' ', 1) for line in panes.splitlines())
pid, actor = os.getpid(), None
for _ in range(24):
    if str(pid) in parents:
        actor = parents[str(pid)]; break
    result = subprocess.run(['ps', '-o', 'ppid=', '-p', str(pid)], capture_output=True, text=True)
    if not result.stdout.strip(): break
    pid = int(result.stdout.strip())
(root / f'{role}.ready').write_text(json.dumps({'actor': actor}))
deadline = time.monotonic() + 180
while not (root / 'release').exists():
    if time.monotonic() > deadline: raise SystemExit('fixture gate timed out')
    time.sleep(.1)
data = json.loads((root / f'{role}.input.json').read_text())
result = {'role': role, 'actor': actor, 'answer': sum(data['values']), 'nonce': data['nonce']}
raw = json.dumps(result, sort_keys=True)
tmp = root / f'{role}.output.tmp'
tmp.write_text(raw)
tmp.replace(root / f'{role}.output.json')
print(raw)
