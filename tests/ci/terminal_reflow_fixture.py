"""Offline TUI for real tmux/PTY resize and Escape regressions (no model needed)."""
import os
import signal
import sys
import termios
import tty

original = termios.tcgetattr(sys.stdin)
escaped = False


def draw(*_):
    cols, rows = os.get_terminal_size()
    screen = f"\x1b[2J\x1b[HFRAME {cols}x{rows}"
    screen += f"\x1b[2;1H{'x' * (cols - 1)}R"
    if escaped:
        screen += "\x1b[3;1HESCAPE-RECEIVED"
    screen += f"\x1b[{rows};1HBOTTOM"
    sys.stdout.write(screen)
    sys.stdout.flush()


try:
    tty.setraw(sys.stdin)
    signal.signal(signal.SIGWINCH, draw)
    sys.stdout.write("\x1b[?1049h")
    draw()
    while data := os.read(sys.stdin.fileno(), 1):
        if data == b"\x1b":
            escaped = True
            draw()
finally:
    sys.stdout.write("\x1b[?1049l")
    sys.stdout.flush()
    termios.tcsetattr(sys.stdin, termios.TCSADRAIN, original)
