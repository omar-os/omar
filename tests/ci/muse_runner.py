#!/usr/bin/env python3
"""The real Muse CLI under OMAR's protocol runner, over ACP.

Muse's `echo` provider answers without a model or a Meta login, so this runs
the shipped `muse acp` end to end: an event sent to the runner's inbox becomes
a Muse turn, and after the runner is killed and restarted it reloads the same
Muse session instead of starting over. Nothing is typed into a terminal.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
MUSE = os.environ.get('MUSE_BIN') or shutil.which('muse')


def wait(predicate, label, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(.1)
    raise AssertionError(label)


def main():
    assert MUSE, 'muse is not installed (curl -fsSL https://dev.meta.ai/install.sh | bash)'
    with tempfile.TemporaryDirectory(prefix='omar-muse-') as directory:
        root = Path(directory)
        sock = Path('/tmp') / ('omar-muse-' + root.name.rsplit('-', 1)[-1] + '.sock')
        context = root / 'context.json'
        context.write_text(json.dumps(dict(omar_dir=str(root), ea_id=0, agent_name='worker',
            session_prefix='test-', default_command='muse --yolo', default_workdir=str(root),
            health_idle_warning=15)))
        (root / 'task-lifecycle.json').write_text(json.dumps({'tasks': {}}))
        prompt = root / 'prompt.md'
        prompt.write_text('You are an OMAR worker. Use OMAR tools to report your result.')
        config = root / 'runner.json'
        config.write_text(json.dumps(dict(backend='muse', command=f"'{MUSE}' --yolo --provider echo",
            context_file=str(context), prompt_file=str(prompt), socket=str(sock))))
        inbox = config.with_suffix('.inbox.json')
        log_path = root / 'runner.log'
        log = log_path.open('w+')
        processes = []

        def start():
            sock.unlink(missing_ok=True)
            proc = subprocess.Popen([str(BIN), 'backend-runner', '--backend', 'muse', '--config-file', str(config)],
                cwd=root, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
            processes.append(proc)
            wait(lambda: sock.exists() or proc.poll() is not None, 'runner never bound its socket')
            assert proc.poll() is None, log_path.read_text()
            return proc

        def send(text):
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(5)
                client.connect(str(sock))
                client.sendall(json.dumps({'text': text}).encode() + b'\n')
                return json.loads(client.makefile('rb').readline())['accepted']

        def state():
            return json.loads(inbox.read_text()) if inbox.exists() else {}

        try:
            proc = start()
            session = state().get('session')
            assert session, f'no Muse session saved: {state()}'
            send('first event: build the parser')
            wait(lambda: not state().get('pending'), 'Muse never finished the first turn')
            # A Muse turn echoes its prompt; the runner prints what the agent says.
            wait(lambda: 'echo: You are an OMAR worker' in log_path.read_text(), 'no reply from Muse')

            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)
            proc = start()
            assert state().get('session') == session, 'restart started a new Muse session'
            assert 'Could not resume' not in log_path.read_text(), log_path.read_text()
            send('second event: the parser is reviewed')
            wait(lambda: not state().get('pending'), 'Muse never finished the turn after restart')

            # Muse's own record of the session holds both turns, in order.
            exported = root / 'export.json'
            subprocess.run([MUSE, 'export', '--session', session, '--out', str(exported)],
                cwd=root, check=True, capture_output=True, timeout=60)
            text = exported.read_text()
            first, second = text.find('first event: build the parser'), text.find('second event: the parser is reviewed')
            assert 0 <= first < second, 'Muse session does not hold both turns in order'
            assert 'OMAR coordination event (not operator input)' in text
            print(f'PASS: muse {session} took an event over ACP, survived a runner restart by session/load, '
                  'and took the next event in the same session', flush=True)
        finally:
            for proc in processes:
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGTERM)
                    proc.wait(timeout=5)
            sock.unlink(missing_ok=True)
            log.close()


if __name__ == '__main__':
    main()
