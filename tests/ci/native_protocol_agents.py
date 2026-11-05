#!/usr/bin/env python3
"""Opt-in live Cursor/Antigravity protocol test using installed CLI credentials.
Runs two bounded, tool-free model turns per backend in a disposable directory.
Checks native conversation continuity and no Cursor prompt-recall history writes.
Usage: OMAR_LIVE_BACKENDS=1 python3 tests/ci/native_protocol_agents.py [cursor|agy]
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

BIN = Path(os.environ.get('OMAR_BIN', 'target/debug/omar')).resolve()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None

def main():
    if os.environ.get('OMAR_LIVE_BACKENDS') != '1':
        raise SystemExit('Set OMAR_LIVE_BACKENDS=1 to run authenticated live model calls')
    results = []
    for backend in sys.argv[1:] or ['cursor', 'agy']:
        with tempfile.TemporaryDirectory(prefix='omar-native-protocol-', dir='/tmp') as directory:
            root = Path(directory)
            context = root / 'context.json'
            context.write_text(json.dumps(dict(omar_dir=str(root), ea_id=0, agent_name='probe',
                session_prefix=root.name+'-', default_command='cat', default_workdir=str(root), health_idle_warning=15)))
            prompt = root / 'instructions.md'
            prompt.write_text('This is a protocol integration test. Do not use tools. Follow the requested response format exactly.')
            sock = root / 'agent.sock'
            config = root / 'runner.json'
            command = 'cursor agent --yolo --approve-mcps' if backend == 'cursor' else 'agy --dangerously-skip-permissions'
            config.write_text(json.dumps(dict(backend=backend, command=command,
                context_file=str(context), prompt_file=str(prompt), socket=str(sock))))
            inbox = config.with_suffix('.inbox.json')
            history = Path.home() / '.cursor/prompt_history.json'
            before_history = digest(history)
            log = (root / 'output.log').open('w+')
            env = dict(os.environ)
            for key in ['OMAR_MCP_CONTEXT_FILE', 'OMAR_AGENT_NAME', 'OMAR_EA_ID', 'OMAR_EVENT_SPOOL']:
                env.pop(key, None)
            proc = subprocess.Popen([str(BIN), 'backend-runner', '--backend', backend, '--config-file', str(config)],
                cwd=root, stdin=subprocess.DEVNULL, stdout=log, stderr=log, env=env, start_new_session=True)
            def output():
                log.flush(); log.seek(0); return log.read()
            def wait(predicate, label, timeout=90):
                end = time.monotonic()+timeout
                while time.monotonic()<end:
                    if predicate(): return
                    if proc.poll() is not None:
                        raise AssertionError(f'{label}: native runner exited ({proc.returncode}): '+output()[-1200:])
                    time.sleep(.1)
                raise AssertionError(f'{label}: timeout: '+output()[-1200:])
            def send(text):
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(5); client.connect(str(sock))
                    client.sendall(json.dumps({'text':text}).encode()+b'\n')
                    assert 'accepted' in json.loads(client.makefile('rb').readline())
                wait(lambda: inbox.exists() and not json.loads(inbox.read_text())['pending'], 'native turn did not finish')
            try:
                wait(sock.exists, 'native protocol initialization')
                send('Do not use tools. Remember verification word tessera for our next turn. Reply exactly PROTOCOL_ONE.')
                assert 'PROTOCOL_ONE' in output(), 'first native response missing'
                first_id = json.loads(inbox.read_text())['session']
                assert first_id
                time.sleep(.5)
                send('Do not use tools. What verification word did we agree earlier? Reply with only that word.')
                assert 'tessera' in output(), 'native conversation context was lost'
                assert json.loads(inbox.read_text())['session'] == first_id
                if backend == 'cursor':
                    assert digest(history) == before_history, 'ACP events changed operator prompt-recall history'
                results.append({'backend':backend,'status':'passed','native_session_persisted':True})
                print(f'PASS: installed {backend} completed two idle-separated native turns in the same conversation', flush=True)
            finally:
                if proc.poll() is None:
                    os.killpg(proc.pid, signal.SIGTERM)
                    proc.wait(timeout=5)
                log.close()
    print(json.dumps(results))

if __name__ == '__main__':
    main()
