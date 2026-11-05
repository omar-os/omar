#!/usr/bin/env python3
"""Real OMAR runner and durable inbox; deterministic native-protocol peers.
Tests idle wake, active-turn queuing, crash/restart replay, native session resume,
and freshly projected ownership. Never reads or submits a terminal composer.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
PEER = r'''
import json, sys, time
from pathlib import Path
root = Path(sys.argv[1])
backend = sys.argv[2]
with (root / 'launches.jsonl').open('a') as log: log.write(json.dumps(sys.argv) + '\n')
for line in sys.stdin:
    message = json.loads(line)
    with (root / 'requests.jsonl').open('a') as log:
        log.write(json.dumps(message) + '\n')
    if backend == 'cursor':
        method = message.get('method')
        if method == 'session/prompt':
            while (root / 'hold').exists(): time.sleep(.02)
            result = {'stopReason': 'cancelled' if (root / 'cancel').exists() else 'end_turn'}
        elif method == 'session/new': result = {'sessionId': 'native-session'}
        else: result = {}
        if 'id' in message:
            # Observed from the real Cursor CLI before its initialize reply.
            # Losing this reply strands startup even though the peer is ready.
            if method == 'initialize': sys.stdout.write('\x1bM\x1b[K')
            print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}), flush=True)
    else:
        print(json.dumps({'event':'init','conversation_id':'native-session'}), flush=True)
        while (root / 'hold').exists(): time.sleep(.02)
        print(json.dumps({'event':'result','result':{'status': 'FAILED' if (root / 'cancel').exists() else 'SUCCESS','response':'done','conversation_id':'native-session'}}), flush=True)
'''

def wait(predicate, label, timeout=12):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate(): return
        time.sleep(.03)
    raise AssertionError(label)


def main():
    for backend in ['cursor', 'agy']:
        with tempfile.TemporaryDirectory(prefix='omar-runner-') as directory:
            root = Path(directory)
            # Keep the unix path below native macOS limits.
            sock = Path('/tmp') / ('omar-runner-' + root.name.rsplit('-', 1)[-1] + '.sock')
            context = root / 'context.json'
            context.write_text(json.dumps(dict(omar_dir=str(root), ea_id=0, agent_name='worker',
                session_prefix='test-', default_command='cat', default_workdir=str(root), health_idle_warning=15)))
            ledger = root / 'task-lifecycle.json'
            task = dict(id='durable-proof', ea_id=0, agent='worker', session='test-worker', parent='ea',
                project_id=1, assignment='before compaction', status='running', result=None,
                acknowledged=False, retired=False, next_check_ms=9999999999999)
            ledger.write_text(json.dumps({'tasks': {'durable-proof': task}}))
            prompt = root / 'prompt.md'
            prompt.write_text('Preserve native coding policy. Use OMAR tools to finish the assigned task.')
            peer = root / 'peer.py'
            peer.write_text(PEER)
            config = root / 'runner.json'
            config.write_text(json.dumps(dict(backend=backend, command=f"python3 '{peer}' '{root}' {backend}",
                context_file=str(context), prompt_file=str(prompt), socket=str(sock))))
            inbox = config.with_suffix('.inbox.json')
            log = (root / 'runner.log').open('w+')
            processes = []

            def start():
                proc = subprocess.Popen([str(BIN), 'backend-runner', '--backend', backend, '--config-file', str(config)],
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
                processes.append(proc)
                wait(lambda: sock.exists() or proc.poll() is not None, 'runner failed to bind')
                assert proc.poll() is None, (log.seek(0), log.read())
                return proc

            def send(text):
                with socket.socket(socket.AF_UNIX) as client:
                    client.settimeout(3)
                    client.connect(str(sock))
                    client.sendall(json.dumps({'text': text}).encode() + b'\n')
                    return json.loads(client.makefile('rb').readline())['accepted']

            def requests():
                p = root / 'requests.jsonl'
                return [json.loads(line) for line in p.read_text().splitlines()] if p.exists() else []

            def prompts():
                return [v for v in requests() if v.get('method') == 'session/prompt' or v.get('event') == 'user']

            try:
                (root / 'hold').touch()
                proc = start()
                first = send('first task')
                wait(lambda: len(prompts()) == 1, 'idle backend did not start a native turn')
                wait(lambda: inbox.exists() and json.loads(inbox.read_text()).get('session') == 'native-session', 'native session ID not saved')
                second = send('follow-up during active turn')
                assert send('follow-up during active turn') == second, 'pending retries must coalesce'
                assert len(json.loads(inbox.read_text())['pending']) == 2
                assert len(prompts()) == 1, 'active turns must not overlap'
                # Stop the entire native process group after acceptance, before completion.
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
                sock.unlink(missing_ok=True)
                task['assignment'] = 'fresh after compaction and restart'
                ledger.write_text(json.dumps({'tasks': {'durable-proof': task}}))
                (root / 'hold').unlink()
                proc = start()
                wait(lambda: inbox.exists() and not json.loads(inbox.read_text())['pending'], 'restart lost accepted messages')
                replay = prompts()[1:]
                assert len(replay) == 2
                for p in replay:
                    text = p['params']['prompt'][0]['text'] if backend == 'cursor' else p['message']['content']
                    assert 'fresh after compaction and restart' in text and 'durable-proof' in text
                    assert 'OMAR coordination event (not operator input)' in text
                assert first in json.dumps(replay[0]) and second in json.dumps(replay[1])
                assert text.index('OMAR COORDINATION STATE') > text.index('OMAR coordination event (not operator input)')
                if backend == 'cursor':
                    assert any(p.get('method') == 'session/load' and p['params']['sessionId'] == 'native-session' for p in requests())
                if backend == 'agy':
                    launches = [json.loads(line) for line in (root / 'launches.jsonl').read_text().splitlines()]
                    assert launches[-1][-4:] == ['--input-format', 'stream-json', '--output-format', 'stream-json']
                    assert launches[-1][launches[-1].index('--conversation') + 1] == 'native-session'
                assert json.loads(inbox.read_text())['session'] == 'native-session'
                count = len(prompts())
                send('wake again after idle')
                wait(lambda: len(prompts()) == count + 1, 'second idle wake failed')
                wait(lambda: not json.loads(inbox.read_text())['pending'], 'final turn did not settle')
                (root / 'cancel').touch()
                rejected = send('retain this cancelled turn')
                wait(lambda: proc.poll() is not None, 'cancelled turn was silently accepted')
                assert proc.returncode != 0
                assert json.loads(inbox.read_text())['pending'][0]['id'] == rejected
                assert not sock.exists(), 'failed runner still advertises acceptance'
                (root / 'cancel').unlink()
                proc = start()
                wait(lambda: not json.loads(inbox.read_text())['pending'], 'cancelled message was not recovered')
                print(f'PASS: {backend} native protocol idle wake, serial turns, durable acceptance/restart, fresh ownership, and cancellation recovery', flush=True)
            finally:
                for proc in processes:
                    if proc.poll() is None:
                        os.killpg(proc.pid, signal.SIGTERM)
                        proc.wait(timeout=5)
                sock.unlink(missing_ok=True)
                log.close()

if __name__ == '__main__':
    main()
