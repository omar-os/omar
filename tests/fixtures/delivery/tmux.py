#!/usr/bin/python3
"""Deterministic tmux process double for delivery integration tests."""
import json
from pathlib import Path
import sys

path = Path(__file__).with_name('state.json')
state = json.loads(path.read_text())
args = sys.argv[1:]
state.setdefault('commands', []).append(args)
mode = state.get('mode', '')
code = 0
if args[0] == 'show-environment':
    if args[-1] == 'OMAR_BACKEND':
        print('OMAR_BACKEND=' + state.get('backend', 'codex'))
    elif state.get('channel'):
        print('OMAR_DELIVERY=' + state['channel'])
    else:
        code = 1
elif args[0] == 'display-message':
    print({'#{pane_width}': 160, '#{pane_pid}': 1,
           '#{window_activity}': state.get('submitted', 0),
           '#{cursor_flag} #{cursor_y} #{cursor_x}': '0 0 0'}.get(args[-1], ''))
elif args[0] == 'capture-pane':
    if state.get('unreadable'):
        print('busy redraw: no composer')
    else:
        print('OpenAI Codex\n\n› ' + state['input'].replace('\n', '\n  ') + '\n\n  model · directory')
elif args[0] == 'load-buffer':
    state['buffer'] = Path(args[-1]).read_text()
elif args[0] == 'paste-buffer':
    payload = state['buffer']
    event = '<UserPromptBegins:' in payload
    if event and mode == 'paste_before':
        code = 1
    else:
        state['input'] += payload
        if event and mode in ('paste_after', 'unreadable_after'):
            state['unreadable'] = mode == 'unreadable_after'
            code = 1
        elif not event and mode == 'restore_fail':
            state['input'] = ''
            code = 1
elif args[0] == 'send-keys':
    if args[-1] in ('C-u', 'C-k', 'C-c'):
        state['input'] = ''
        if mode == 'unreadable_clear':
            state['unreadable'] = True
    elif args[-1] == '0d':
        if mode == 'enter':
            code = 1
        else:
            state['submitted'] = state.get('submitted', 0) + 1
            state['input'] = ''
else:
    raise RuntimeError(args)
path.write_text(json.dumps(state))
if code:
    print('simulated tmux failure', file=sys.stderr)
sys.exit(code)
