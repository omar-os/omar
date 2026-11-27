"""A scratch directory for a test's OMAR session, torn down however the test ends.

On leaving, the session named in it is removed (`omar rm`: its runtime, tmux
server, record and state), then every process still naming the directory on
its command line (the session daemon, a fake worker) is killed, and the test
fails if any of them is still running. Its tmux sockets are made inside it
(TMUX_TMPDIR), so nothing of the session outlives the directory.
"""
import contextlib
import os
from pathlib import Path
import subprocess
import tempfile
import time


def left_running(root):
    """Processes, other than this one, whose command line names `root`."""
    names = {str(root), str(Path(root).resolve())}
    listing = subprocess.run(['ps', '-axww', '-o', 'pid=,command='], capture_output=True, text=True)
    found = []
    for line in listing.stdout.splitlines():
        pid, _, command = line.strip().partition(' ')
        if int(pid) != os.getpid() and any(name in command for name in names):
            found.append((int(pid), command))
    return found


@contextlib.contextmanager
def scratch(prefix, binary, session):
    """Yield a private directory. `session` is the OMAR session the test
    starts in it, with HOME set to `root/home` and TMUX_TMPDIR to `root/tmux`."""
    # Short: a Unix socket path is at most 104 bytes on macOS.
    root = Path(tempfile.mkdtemp(prefix=prefix, dir='/tmp')).resolve()
    tmux = root / 'tmux'
    tmux.mkdir()
    try:
        yield root
    finally:
        env = {k: v for k, v in os.environ.items()
               if not k.startswith('OMAR_') and k not in ('TMUX', 'TMUX_PANE')}
        env.update(HOME=str(root / 'home'), TMUX_TMPDIR=str(tmux))
        subprocess.run([str(binary), 'rm', '-s', session], env=env, capture_output=True,
                       timeout=90)
        # What `omar rm` stopped gets a moment to exit; what it did not is
        # killed, so it cannot outlive the test, and fails it.
        deadline = time.time() + 10
        while (left := left_running(root)) and time.time() < deadline:
            time.sleep(0.2)
        for pid, _ in left:
            subprocess.run(['kill', '-KILL', str(pid)], capture_output=True)
        subprocess.run(['rm', '-rf', str(root)])
        assert not left, f'still running once session {session} was removed: {left}'


def assert_none_left(prefix):
    """Nothing any scratch of `prefix` started is still running."""
    left = left_running(Path('/tmp') / prefix)
    assert not left, f'still running: {left}'
