#!/usr/bin/python3
"""Read-only tmux double: agent delivery must never touch terminal input."""
import json
from pathlib import Path
import sys

path = Path(__file__).with_name('state.json')
state = json.loads(path.read_text())
args = sys.argv[1:]
state.setdefault('commands', []).append(args)
path.write_text(json.dumps(state))
if args[0] == 'show-environment':
    key = 'backend' if args[-1] == 'OMAR_BACKEND' else 'channel'
    value = state.get(key)
    if value is None:
        sys.exit(1)
    print(args[-1] + '=' + value)
elif args[0] == 'display-message' and args[-1] == '#{pane_pid}':
    print(1)
else:
    raise RuntimeError('agent delivery touched terminal: ' + repr(args))
