#!/usr/bin/env python3
"""Wait out native provider limits, preserving task id, deadline and attempt.

Codex/Pi/OpenCode are real CLIs against local HTTP providers. Claude and ACP
use deterministic protocol peers; no sign-in or credentials are needed.
"""
import argparse
import json
import re
import subprocess
import threading
import time
from http.server import ThreadingHTTPServer
from unittest.mock import patch

import worker_restart as workers
from scratch_session import scratch

WAIT = 16
WITHIN = 10


class Provider(workers.Provider):
    def do_POST(self):
        if self.server.reset is None:
            self.server.reset = int(time.time()) + WAIT
        credit = getattr(self.server, "credit", None)
        if credit or time.time() < self.server.reset:
            request = json.loads(self.rfile.read(int(self.headers.get('content-length', '0'))))
            self.server.limited.append(request)
            error = credit['error'] if credit else {'type': 'usage_limit_reached',
                'code': 'usage_limit_reached', 'message': 'Usage limit reached',
                'plan_type': 'plus', 'resets_at': self.server.reset}
            if credit and credit.get('responses'):
                # Codex's quota_exceeded.rs fixture uses this native Responses error.
                # Generic HTTP 429 bodies are discarded by Codex 0.154 itself.
                chunks = [
                    {'type': 'response.created', 'response': {'id': 'credit', 'status': 'in_progress', 'output': []}},
                    {'type': 'response.failed', 'response': {'id': 'credit', 'status': 'failed', 'error': error}},
                ]
                body = ''.join(f'data: {json.dumps(chunk)}\n\n' for chunk in chunks).encode()
                self.send_response(200)
                self.send_header('content-type', 'text/event-stream')
                self.send_header('content-length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            body = json.dumps({'error': error}).encode()
            self.send_response(credit['status'] if credit else 429)
            self.send_header('content-type', 'application/json')
            self.send_header('retry-after', str(max(1, self.server.reset - int(time.time()))))
            self.send_header('x-codex-primary-used-percent', '100')
            self.send_header('x-codex-primary-window-minutes', '300')
            self.send_header('x-codex-primary-reset-at', str(self.server.reset))
            self.send_header('content-length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            super().do_POST()


ANSWER = r'''
limited = False
limit_reset = None
def answer(text):
    global limited, limit_reset
    import time, shlex
    from datetime import datetime
    write({"event":"prompt", "text":text, "time":time.time()})
    credit = os.environ.get("LIMIT_CREDIT_ERROR")
    if credit:
        error = json.loads(credit)
        if flag("--mcp-config"):
            print("\x1b[2J\x1b[HAPI Error: " + error["message"], flush=True)
            return
        return {"code":-32603,"message":error["message"],"data":error}
    task = int(re.findall(r"task_id: (\d+)", text)[-1])
    if limit_reset is None: limit_reset = int(time.time()) + 16
    if not limited or (not flag("--mcp-config") and time.time() < limit_reset):
        limited = True
        reset = limit_reset
        write({"event":"limit", "reset":reset})
        if flag("--mcp-config"):
            print("\x1b[2J\x1b[HUsage limit reached. Continuing automatically at " + datetime.fromtimestamp(reset).astimezone().isoformat(), flush=True)
            # A real Claude Stop can occur while its automatic continuation waits.
            for entry in json.loads(flag("--settings")).get("hooks", {}).get("Stop", []):
                for handler in entry["hooks"]:
                    subprocess.run(shlex.split(handler["command"]), input='{"hook_event_name":"Stop"}', text=True, stdout=subprocess.DEVNULL, check=True)
            # Record every terminal byte: no Escape, Enter, or other keys are allowed.
            import select, termios, tty
            terminal = termios.tcgetattr(sys.stdin)
            tty.setraw(sys.stdin)
            while time.time() < reset:
                if select.select([sys.stdin], [], [], .05)[0]:
                    write({"event":"key", "bytes":os.read(sys.stdin.fileno(), 1024).hex()})
            termios.tcsetattr(sys.stdin, termios.TCSANOW, terminal)
            print("\x1b[2J\x1b[HContinuing task", flush=True)
        else:
            return {"code":-32603,"message":"Usage limit reached", "data":{"type":"usage_limit_reached", "resets_at":reset}}
    call("omar_set_port", {"task_id":task,"port":"result","value":"recovered"})
    call("omar_complete", {"task_id":task})
'''

MANAGED = workers.FAKE_MANAGED.replace(workers.FAKE_ANSWER, ANSWER).replace(
    '                answer(text)\n                result = {"stopReason": "end_turn"}',
    '\n'.join([
        '                error = answer(text)',
        '                if error:',
        '                    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"error":error}), flush=True)',
        '                    continue',
        '                result = {"stopReason":"end_turn"}',
    ]))
assert 'error = answer(text)' in MANAGED


def check_records(state, output, result):
    lines = [json.loads(line) for path in state.rglob('attempts.jsonl') for line in path.read_text().splitlines()]
    attempts = {item['attempt_id']: item for item in lines}
    detail = output + '\n' + json.dumps(lines)
    assert result == 0 and 'Output p.result = ' in output, detail
    assert len(attempts) == 1, detail
    attempt = next(iter(attempts.values()))
    assert attempt.get('retry', 0) == 0 and not attempt.get('reminders'), detail
    waits = attempt.get('usage_waits', [])
    assert waits and all(w.get('ended_at') for w in waits), detail
    assert len({w['task_id'] for w in waits}) == 1, detail
    from datetime import datetime
    waited = sum((datetime.fromisoformat(w['ended_at']) - datetime.fromisoformat(w['started_at'])).total_seconds() for w in waits)
    assert waited > WITHIN, detail
    assert any(item.get('usage_waits') and not item['usage_waits'][-1].get('ended_at') for item in lines), detail
    return waits


def check_credit(state, output, result, elapsed):
    lines = [json.loads(line) for path in state.rglob('attempts.jsonl') for line in path.read_text().splitlines()]
    detail = output + '\n' + json.dumps(lines)
    assert result != 0 and 'out of credit:' in output, detail
    assert len({a['attempt_id'] for a in lines}) == 1, detail
    assert all(a.get('retry', 0) == 0 and not a.get('reminders') and not a.get('usage_waits') for a in lines), detail
    assert lines[-1]['outcome'] == 'failed', detail
    assert 'out of credit:' in (lines[-1].get('failure_reason') or lines[-1].get('death_reason', '')), detail
    assert elapsed < WAIT, f'credit failure took {elapsed:.1f}s, expected less than {WAIT}s\n{detail}'


def execute(backend, unknown=False, timeout=False, credit=None):
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.requests, server.limited, server.reset = [], [], None
    server.credit = credit
    server.killed, server.deaths = set(), 0
    server.scenario, server.waited = None, set()
    server.kill, server.release = threading.Event(), threading.Event()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with scratch('omar-limit-', workers.BIN, 'limit-test') as root:
            home, env = workers.isolated_environment(root)
            if credit:
                env['LIMIT_CREDIT_ERROR'] = json.dumps(credit['error'])
            answer = ANSWER
            managed = MANAGED
            if unknown:
                managed = managed.replace('"resets_at":reset', '"resets_at":None')
            if timeout:
                managed = managed.replace('if not limited or (not flag("--mcp-config") and time.time() < limit_reset):', 'if True:')
            with patch.object(workers, 'FAKE_ANSWER', answer), patch.object(workers, 'FAKE_MANAGED', managed):
                workers.configure(root, home, env, backend, server, False)
            if backend == 'Stub':
                with (home / '.profile').open('a') as out:
                    out.write('export OMAR_STUB_USAGE_WAIT_MILLIS=16000\n')
            program = root / 'Limit.omar'
            program.write_text(workers.PROGRAM.replace('BACKENDLIFETIME', backend).replace(
                '-> result "PROMPT"', f'-> result within({30 if credit else WITHIN}s) "Return one word."'))
            up = subprocess.run([str(workers.BIN), 'up', '-s', 'limit-test', '--no-ea'], env=env, cwd=root, capture_output=True, text=True, timeout=90)
            assert up.returncode == 0, up.stdout + up.stderr
            launch = json.loads((home / '.omar/sessions/limit-test/launch.json').read_text())
            env['OMAR_TMUX_SERVER'] = launch['session']['tmux_server']
            began = time.monotonic()
            process = subprocess.Popen([str(workers.BIN), 'run', '-s', 'limit-test', str(program), '--input', 'p.start=go',
                '--timeout-seconds', '14' if timeout else '55', '--fg'], env=env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            screens = []
            def capture():
                while process.poll() is None:
                    screen = subprocess.run(['tmux', '-L', env['OMAR_TMUX_SERVER'], 'capture-pane', '-S', '-60', '-pt', 'omar-agent-0-p_coder'], env=env, capture_output=True, text=True)
                    if screen.returncode == 0:
                        screens.append(screen.stdout)
                    time.sleep(.2)
            monitor = threading.Thread(target=capture, daemon=True)
            monitor.start()
            try:
                stdout, stderr = process.communicate(timeout=100)
            except subprocess.TimeoutExpired:
                process.kill()
                stdout, stderr = process.communicate()
                raise AssertionError('run timed out: ' + stdout + stderr + '\n' + '\n'.join(screens[-2:]))
            monitor.join(timeout=2)
            output = stdout + stderr
            if credit:
                check_credit(home / '.omar', output + '\nLast screen:\n' + '\n'.join(screens[-2:]), process.returncode, time.monotonic() - began)
                print(f'PASS: {backend} credit exhaustion: failed fast, no wait/reminder/restart', flush=True)
                return
            if timeout:
                assert process.returncode != 0 and 'run-wide timeout' in output, output
                records = [json.loads(line) for path in home.rglob('attempts.jsonl') for line in path.read_text().splitlines()]
                assert records and len({a['attempt_id'] for a in records}) == 1 and not any(a.get('reminders') or a.get('retry', 0) for a in records), records
                assert records[-1]['usage_waits'][-1].get('ended_at'), records
                print(f'PASS: {backend}: unknown reset bounded by run-wide timeout', flush=True)
                return
            waits = check_records(home / '.omar', output + f'\nRun took {time.monotonic()-began:.1f}s\nLast screen:\n' + '\n'.join(screens[-2:]) + '\nProvider requests: ' + json.dumps([{'items':r.get('input', r.get('messages'))[-2:]} for r in server.requests]) + '\nLimited requests: ' + str(len(server.limited)), process.returncode)
            if backend == 'Claude':
                log = [json.loads(line) for line in (root / 'fake.jsonl').read_text().splitlines()]
                assert len([e for e in log if e['event'] == 'prompt']) == 1 and not any(e['event'] == 'key' for e in log), log
            if backend == 'Codex':
                assert any(re.search(r"usage limit|usage-limit", screen, re.I) for screen in screens), '\n'.join(screens)
                assert all(w['resets_at'] for w in waits), waits
            print(f'PASS: {backend}{" unknown reset" if unknown else ""}: waited past within, no reminder/restart, same task completed' + ('; automatic continue, no input' if backend == 'Claude' else ''), flush=True)
    finally:
        server.release.set()
        server.shutdown()
        server.server_close()


def execute_jev(credit_status=None):
    class JevProvider(workers.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers.get('content-length', '0')))
            first = self.server.reset is None
            if first:
                self.server.reset = int(time.time()) + WAIT
            status = credit_status or (429 if time.time() < self.server.reset else 200)
            response = {'error': {'type': 'insufficient_quota', 'message': 'insufficient credits'}} if credit_status else {'error': {'type': 'rate_limit'}} if status == 429 else {'model': 'jev-test', 'answers': {
                'decision': {'type': 'choice', 'choice': 'ok', 'confidence': .99, 'probabilities': {'ok': .99, 'no': .01}},
                'sufficient_context': {'type': 'noul', 'noul': .99}}}
            body = json.dumps(response).encode()
            self.send_response(status)
            self.send_header('Retry-After', str(max(1, self.server.reset - int(time.time()))))
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(('127.0.0.1', 0), JevProvider)
    server.reset = None
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with scratch('omar-limit-', workers.BIN, 'limit-test') as root:
            home, env = workers.isolated_environment(root)
            shims = root / 'shims'
            shims.mkdir()
            import shutil
            curl = shutil.which('curl')
            shim = shims / 'curl'
            shim.write_text('#!/usr/bin/env python3\nimport subprocess,sys\n' +
                f'config=sys.stdin.read().replace("https://api.typesafe.ai/v1/systemone", "http://127.0.0.1:{server.server_port}/")\n' +
                f'result=subprocess.run([{curl!r}, *sys.argv[1:]], input=config, text=True)\nsys.exit(result.returncode)\n')
            shim.chmod(0o700)
            env.update(PATH=str(shims) + ':' + env['PATH'], TYPESAFE_API_KEY='test-only', OMAR_JEV_MODEL='jev-test')
            (root / 'decision.json').write_text(json.dumps({'type': 'string', 'enum': ['ok', 'no']}))
            program = root / 'Limit.omar'
            program.write_text('type Decision from "./decision.json"\nteam Work[coder : Jev] {\n'
                ' input start : string\n output result : Decision\n'
                f' task coder(start) -> result within({WITHIN}s) "Choose ok."\n' +
                '}\nmain Restart { p = Work() }\n')
            up = subprocess.run([str(workers.BIN), 'up', '-s', 'limit-test', '--no-ea'], env=env, cwd=root, capture_output=True, text=True, timeout=90)
            assert up.returncode == 0, up.stdout + up.stderr
            began = time.monotonic()
            result = subprocess.run([str(workers.BIN), 'run', '-s', 'limit-test', str(program), '--input', 'p.start=go',
                '--timeout-seconds', '30', '--fg'], env=env, cwd=root, capture_output=True, text=True, timeout=60)
            if credit_status:
                check_credit(home / '.omar', result.stdout + result.stderr, result.returncode, time.monotonic() - began)
                print(f'PASS: Jev HTTP {credit_status} insufficient_quota: failed fast, no wait/reminder/retry', flush=True)
                return
            check_records(home / '.omar', result.stdout + result.stderr, result.returncode)
            print('PASS: Jev mock HTTP 429 + Retry-After: waited past within, no retry, same task completed', flush=True)
    finally:
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--credit-only', action='store_true')
    parser.add_argument('--backend', choices=['Claude', 'Cursor', 'Agy', 'Codex', 'Pi', 'OpenCode', 'Stub', 'Jev'])
    args = parser.parse_args()
    backends = [args.backend] if args.backend else ['Claude', 'Cursor', 'Agy', 'Codex', 'Pi', 'OpenCode', 'Stub', 'Jev']
    if not args.credit_only:
        for backend in backends:
            if backend == 'Jev':
                execute_jev()
            else:
                execute(backend)
        if not args.backend:
            execute('Cursor', unknown=True)
            execute('Cursor', unknown=True, timeout=True)
    for backend in backends:
        if backend == 'Stub':
            continue
        if backend == 'Jev':
            execute_jev(402)
            execute_jev(429)
            continue
        credit = {'status': 429, 'error': {'type': 'insufficient_quota', 'code': 'insufficient_quota',
            'message': 'You exceeded your current quota, please check your plan and billing details.'}}
        if backend == 'Codex':
            credit['responses'] = True
        if backend == 'Claude':
            credit['error']['message'] = 'Credit balance is too low'
        elif backend in ['Cursor', 'Agy']:
            credit['error']['message'] = 'Your spending limit has been reached'
        elif backend == 'OpenCode':
            credit = {'status': 402, 'error': {'code': 'insufficient_credits', 'message': 'insufficient credits'}}
        elif backend == 'Pi':
            credit['error']['message'] = 'out of credits'
        execute(backend, credit=credit)


if __name__ == '__main__':
    main()
