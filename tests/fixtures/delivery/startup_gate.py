#!/usr/bin/env python3
"""Native TUI double: refuse readiness until the exact startup choice arrives."""
import os
from pathlib import Path
import select
import sys
import time
import tty

backend, log_path = sys.argv[1:]
log = Path(log_path)
tty.setraw(sys.stdin.fileno())
if backend == 'claude':
    dialog = ("Accessing workspace:\r\n/tmp/team/worktree\r\n"
              "Claude Code'll be able to read, edit, and execute files here.\r\n"
              "❯ No, exit\r\n  Yes, I trust this folder\r\n"
              "Enter to confirm · Esc to cancel")
    expected = b'\x1b[B\r'
    ready = "Claude Code\r\n❯ "
elif backend == 'codex':
    dialog = ("Update available · 1.0 → 2.0\r\n"
              "› 1. Update now (runs `brew upgrade codex`)\r\n"
              "  2. Skip\r\n  3. Skip until next version\r\n"
              "enter continue · esc skip")
    expected = b'\x1b'
    ready = "OpenAI Codex\r\n› "
else:
    sys.stdout.write('\x1b[2J\x1b[HApprove running a command?\r\n❯ Yes\r\n  No')
    sys.stdout.flush()
    while True:
        with log.open('ab') as output:
            output.write(os.read(sys.stdin.fileno(), 1024))
# Paint the gate after launch has returned, as real backends do.
time.sleep(0.2)
sys.stdout.write('\x1b[2J\x1b[H' + dialog)
sys.stdout.flush()
data = b''
if backend == 'claude':
    while len(data) < 3:
        data += os.read(sys.stdin.fileno(), 1024)
    log.write_bytes(data)
    if data != b'\x1b[B':
        raise RuntimeError(f'Confirmed before selection repainted: {data!r}')
    time.sleep(0.2)
    dialog = dialog.replace('❯ No, exit', '  No, exit').replace(
        '  Yes, I trust this folder', '❯ Yes, I trust this folder')
    sys.stdout.write('\x1b[2J\x1b[H' + dialog)
    sys.stdout.flush()
while len(data) < len(expected):
    data += os.read(sys.stdin.fileno(), 1024)
log.write_bytes(data)
if data != expected:
    raise RuntimeError(f'Wrong choice for {backend}: {data!r}')
# Leave the old dialog visible briefly: it must not be answered twice.
time.sleep(0.4)
sys.stdout.write('\x1b[2J\x1b[H' + ready)
sys.stdout.flush()
while True:
    if log.with_suffix('.again').exists():
        sys.stdout.write('\x1b[2J\x1b[H' + dialog)
        sys.stdout.flush()
        log.with_suffix('.again').unlink()
    if select.select([sys.stdin], [], [], 0.1)[0]:
        with log.open('ab') as output:
            output.write(os.read(sys.stdin.fileno(), 1024))
