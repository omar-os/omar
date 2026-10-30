#!/usr/bin/env python3
"""Native TUI double: a resumed Claude conversation. It repaints the old
history and its composer, never the banner, and opens its peer socket
shortly after launch, as the real one does."""
import json
import os
from pathlib import Path
import socket
import sys
import time
import tty

home, log_path = sys.argv[1:]
log = Path(log_path)
tty.setraw(sys.stdin.fileno())
history = "".join(f"  line {n} of an earlier answer\r\n" for n in range(1, 41))
sys.stdout.write("\x1b[2J\x1b[H" + history + "─" * 40 + "\r\n❯ \r\n" + "─" * 40)
sys.stdout.flush()
time.sleep(0.3)
sessions = Path(home) / ".claude" / "sessions"
sessions.mkdir(parents=True, exist_ok=True)
peer = socket.socket(socket.AF_UNIX)
peer.bind(str(Path(home) / f"peer-{os.getpid()}.sock"))
peer.listen()
(sessions / f"{os.getpid()}.json").write_text(json.dumps(
    {"sessionId": "resumed", "messagingSocketPath": peer.getsockname()}))
(sessions / f"{os.getpid()}.0.key").write_text(json.dumps({"peerToken": "token"}))
while True:
    # Nothing may be typed into the composer.
    data = os.read(sys.stdin.fileno(), 1024)
    with log.open("ab") as output:
        output.write(data)
