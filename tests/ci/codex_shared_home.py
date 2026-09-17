#!/usr/bin/env python3
"""Real Codex/OMAR shared-home check, with a local Responses provider.

Requires tmux, Codex 0.154.0+, and websocket-client. Uses a disposable HOME,
CODEX_HOME, and tmux server; never reads user credentials or calls a model.
Run after cargo build: python3 tests/ci/codex_shared_home.py
"""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import websocket

OMAR = str(Path(os.environ.get('OMAR_BIN', 'target/debug/omar')).resolve())
requests = []

class Mock(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        item = {'id': 'msg_probe', 'type': 'message', 'role': 'assistant',
                'status': 'completed', 'content': [{'type': 'output_text', 'text': 'PROBE_ACK', 'annotations': []}]}
        events = [
            {'type': 'response.created', 'response': {'id': 'resp_probe', 'status': 'in_progress', 'output': []}},
            {'type': 'response.output_item.added', 'output_index': 0, 'item': dict(item, status='in_progress', content=[])},
            {'type': 'response.output_text.delta', 'item_id': 'msg_probe', 'output_index': 0, 'content_index': 0, 'delta': 'PROBE_ACK'},
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': {'id': 'resp_probe', 'status': 'completed', 'output': [item], 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}},
        ]
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for event in events:
            self.wfile.write(('data: '+json.dumps(event)+'\n\n').encode())
        self.wfile.flush()

class Rpc:
    def __init__(self, endpoint):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(str(endpoint))
        self.ws = websocket.create_connection('ws://localhost/', socket=sock, timeout=10)
        self.id = 0
        self.call('initialize', {'clientInfo': {'name': 'omar_shared_home_test', 'version': '1'}})
        self.ws.send(json.dumps({'method': 'initialized', 'params': {}}))

    def call(self, method, params):
        self.id += 1
        self.ws.send(json.dumps({'id': self.id, 'method': method, 'params': params}))
        while True:
            reply = json.loads(self.ws.recv())
            if reply.get('id') == self.id:
                assert 'error' not in reply, reply
                return reply['result']

    def close(self):
        self.ws.close()


def until(check, description, seconds=30):
    deadline = time.monotonic()+seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError('Timed out: '+description)

http = ThreadingHTTPServer(('127.0.0.1', 0), Mock)
threading.Thread(target=http.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='omar-cx-', dir='/tmp') as work:
    work = Path(work).resolve()
    home = work/'home'
    codex_home = home/'.codex'
    codex_home.mkdir(parents=True)
    config = f'''model="gpt-6-astra"
model_provider="probe"
[model_providers.probe]
name="probe"
base_url="http://127.0.0.1:{http.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
'''
    (codex_home/'config.toml').write_text(config)
    (work/'.git').mkdir()
    server = 'omar-shared-home-'+str(os.getpid())
    env = dict(os.environ, HOME=str(home), CODEX_HOME=str(codex_home), OMAR_TMUX_SERVER=server)
    env.pop('TMUX', None)
    # The tool runner disables colors; this fixture explicitly tests RGB UI.
    env.pop('NO_COLOR', None)
    clients = []
    daemon = None
    def tmux(*args, check=True):
        return subprocess.run(['tmux', '-L', server, *args], env=env, capture_output=True, text=True, check=check).stdout
    def pane(name):
        return tmux('capture-pane', '-p', '-t', name)
    try:
        for index in [1, 2]:
            if index == 2:
                tmux('set-option', '-gw', 'window-style', 'fg=#abcdef,bg=#112233')
                tmux('set-environment', '-g', 'CODEX_HOME', str(work/'stale-home'))
                tmux('set-environment', '-g', 'NO_COLOR', '1')
            if index == 1:
                # Exercise the real cold dashboard exec inside a PTY. It must
                # reuse the already allocated EA rather than allocate twice.
                command = shlex.join(['env', '-u', 'TMUX', OMAR, '-a', 'codex'])
                tmux('new-session', '-d', '-s', 'cold-launch', '-x', '110', '-y', '35', '-c', str(work), command)
                until(lambda: 'omar-dashboard' in tmux('list-sessions'), 'cold dashboard')
            else:
                # Non-TTY attach can fail after the new manager starts.
                subprocess.run([OMAR, '-a', 'codex'], cwd=work, env=env, capture_output=True, text=True, timeout=30)
            session = 'omar-agent-ea-'+str(index)
            paths = until(lambda: list((home/'.omar/codex-runtime').glob('*/app.sock')), 'socket')
            start = tmux('display-message', '-p', '-t', session, '#{pane_start_command}')
            endpoint = next(p for p in paths if str(p) in start)
            assert tmux('show-environment', '-t', session, 'OMAR_DELIVERY').strip() == 'OMAR_DELIVERY=codex:'+str(endpoint)
            rpc = Rpc(endpoint)
            clients.append(rpc)
            ids = until(lambda: rpc.call('thread/loaded/list', {}).get('data'), 'TUI thread')
            assert len(ids) == 1, ids
            thread = ids[0]
            status = until(lambda: rpc.call('mcpServerStatus/list', {'threadId': thread}).get('data'), 'MCP tools')
            omar_status = next(s for s in status if s['name'] == 'omar')
            assert omar_status.get('tools'), omar_status
            assert str(index) == json.loads((home/'.omar/eas.json').read_text())[-1]['name']
            assert 'export CODEX_HOME=' not in start, start
            assert str(work) == tmux('display-message', '-p', '-t', session, '#{pane_current_path}').strip()
            if index == 1:
                until(lambda: any(dot in pane(session) for dot in '⠁⠂⠄⠈⠐⠠⡀⢀'), 'Astra composer starfield', seconds=8)
                print('PASS: Astra starfield renders in the detached OMAR pane', flush=True)
                first_thread, first_session, first_rpc = thread, session, rpc
            else:
                assert thread != first_thread
                assert tmux('display-message', '-p', '-t', session, '#{window-style}').strip() == 'fg=#abcdef,bg=#112233'
                tmux('set-option', '-gw', 'window-style', 'default')
                assert tmux('show-environment', '-t', session, 'CODEX_HOME').strip() == 'CODEX_HOME='+str(codex_home)
                until(lambda: any(dot in pane(session) for dot in '⠁⠂⠄⠈⠐⠠⡀⢀'), 'starfield despite stale server NO_COLOR', seconds=8)
                tmux('set-environment', '-g', 'CODEX_HOME', str(codex_home))
                tmux('set-environment', '-gu', 'NO_COLOR')
        print('PASS: real OMAR launches two EAs with distinct sockets, working MCP, shared history home, and correct cwd', flush=True)
        tmux('send-keys', '-t', first_session, '-l', 'UNSENT_DRAFT')
        until(lambda: 'UNSENT_DRAFT' in pane(first_session), 'draft')
        first_rpc.call('turn/start', {'threadId': first_thread, 'input': [], 'toolOutput': {'name': 'omar_event', 'namespace': 'omar', 'output': 'EVENT_SENTINEL'}})
        until(lambda: requests, 'local provider request')
        until(lambda: 'PROBE_ACK' in pane(first_session), 'event response')
        assert 'UNSENT_DRAFT' in pane(first_session), pane(first_session)
        assert 'EVENT_SENTINEL' in json.dumps(requests), requests
        assert clients[1].call('thread/loaded/list', {})['data'] != [first_thread]
        print('PASS: event wakes the right agent without submitting its draft', flush=True)
        # Kill the entire original pane; a normal Codex invocation must resume
        # the stored conversation without an OMAR home or server.
        tmux('kill-session', '-t', first_session)
        command = shlex.join(['env', 'CODEX_HOME='+str(codex_home), 'codex', 'resume', first_thread, '--dangerously-bypass-approvals-and-sandbox', '-C', str(work)])
        tmux('new-session', '-d', '-s', 'plain-resume', '-x', '110', '-y', '35', '-c', str(work), command)
        until(lambda: ('Do you trust' in pane('plain-resume')) or ('PROBE_ACK' in pane('plain-resume')), 'ordinary Codex startup')
        if 'Do you trust' in pane('plain-resume'):
            tmux('send-keys', '-t', 'plain-resume', 'Enter')
        until(lambda: 'PROBE_ACK' in pane('plain-resume'), 'ordinary Codex resume')
        assert 'No saved chat' not in pane('plain-resume')
        assert not (home/'.omar/codex').exists()
        print('PASS: ordinary codex resume restores the OMAR conversation after its pane exits', flush=True)
        tmux('kill-session', '-t', 'plain-resume')
        tmux('set-option', '-g', 'remain-on-exit', 'on')
        resume_config = work/'resume.toml'
        resume_config.write_text('[agent]\ndefault_command="codex resume '+first_thread+'"\n')
        result = subprocess.run([OMAR, '-c', str(resume_config), '--ea', '1', 'manager', 'start'], cwd=work, env=env, capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, result.stderr
        start = tmux('display-message', '-p', '-t', first_session, '#{pane_start_command}')
        endpoint = next(p for p in (home/'.omar/codex-runtime').glob('*/app.sock') if str(p) in start)
        resumed = Rpc(endpoint)
        clients.append(resumed)
        ids = until(lambda: resumed.call('thread/loaded/list', {}).get('data'), 'OMAR resume thread')
        assert ids == [first_thread], ids
        status = until(lambda: resumed.call('mcpServerStatus/list', {'threadId': first_thread}).get('data'), 'resumed MCP tools')
        assert next(s for s in status if s['name'] == 'omar').get('tools'), status
        before = len(requests)
        resumed.call('turn/start', {'threadId': first_thread, 'input': [], 'toolOutput': {'name': 'omar_event', 'namespace': 'omar', 'output': 'RESUMED_EVENT_SENTINEL'}})
        until(lambda: len(requests) > before, 'resumed event request')
        assert 'RESUMED_EVENT_SENTINEL' in json.dumps(requests[-1])
        print('PASS: resume inside OMAR preserves the thread and restores MCP plus event delivery', flush=True)
        second_pane = tmux('display-message', '-p', '-t', 'omar-agent-ea-2', '#{pane_id}')
        with socket.socket() as port_socket:
            port_socket.bind(('127.0.0.1', 0))
            port = port_socket.getsockname()[1]
        daemon_log = open(work/'serve.log', 'w')
        daemon = subprocess.Popen([OMAR, '-a', 'codex', '--ea', 'ServeTrial', 'serve', '--address', f'127.0.0.1:{port}'], cwd=work, env=env, stdout=daemon_log, stderr=daemon_log)
        until(lambda: len(json.loads((home/'.omar/eas.json').read_text())) == 3, 'serve allocates new EA')
        registry = json.loads((home/'.omar/eas.json').read_text())
        assert registry[-1]['id'] == 3 and registry[-1]['name'] == 'ServeTrial', registry
        served = 'omar-agent-ea-3'
        until(lambda: 'Ask Codex' in tmux('capture-pane', '-p', '-t', served, check=False), 'served EA composer')
        tmux('send-keys', '-t', served, '-l', 'SERVE_UNSENT_DRAFT')
        until(lambda: 'SERVE_UNSENT_DRAFT' in pane(served), 'served draft')
        subprocess.run([OMAR, '--ea', '3', 'event', 'schedule', '--receiver', 'ea', '--payload', 'LIVE_SCHEDULER_SENTINEL', '--in-seconds', '0'], cwd=work, env=env, check=True, capture_output=True, text=True)
        until(lambda: 'LIVE_SCHEDULER_SENTINEL' in json.dumps(requests), 'real scheduler event')
        assert 'SERVE_UNSENT_DRAFT' in pane(served), pane(served)
        assert tmux('display-message', '-p', '-t', 'omar-agent-ea-2', '#{pane_id}') == second_pane
        print('PASS: serve creates a named EA and the real scheduler delivers through its socket without consuming the draft', flush=True)
        # Exercise Mission Control through the same shared delivery path used by
        # initial tasks and MCP follow-ups, while a native-terminal draft exists.
        import urllib.request
        body = json.dumps({'text': 'MISSION_CONTROL_SENTINEL'}).encode()
        request = urllib.request.Request(f'http://127.0.0.1:{port}/v1/chat', data=body,
                                         headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=30) as response:
            assert response.status == 202
        until(lambda: 'MISSION_CONTROL_SENTINEL' in json.dumps(requests), 'Mission Control delivery')
        assert 'SERVE_UNSENT_DRAFT' in pane(served), pane(served)
        mcp_messages = [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
                'protocolVersion': '2024-11-05', 'capabilities': {},
                'clientInfo': {'name': 'channel-regression', 'version': '1'}}},
            {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {
                'name': 'send_input', 'arguments': {'name': served, 'text': 'MCP_FOLLOWUP_SENTINEL', 'enter': True}}},
        ]
        mcp = subprocess.run([OMAR, '--ea', '3', 'mcp-server'], cwd=work,
                             env=dict(env, OMAR_EA_ID='3', OMAR_DIR=str(home/'.omar')),
                             input=''.join(json.dumps(m)+'\n' for m in mcp_messages),
                             capture_output=True, text=True, timeout=30, check=True)
        replies = [json.loads(line) for line in mcp.stdout.splitlines()]
        reply = next(r for r in replies if r.get('id') == 2)
        assert 'error' not in reply and not reply.get('result', {}).get('isError'), reply
        until(lambda: 'MCP_FOLLOWUP_SENTINEL' in json.dumps(requests), 'MCP follow-up delivery')
        assert 'SERVE_UNSENT_DRAFT' in pane(served), pane(served)
        history = codex_home/'history.jsonl'
        history_text = history.read_text() if history.exists() else ''
        for sentinel in ['EVENT_SENTINEL', 'LIVE_SCHEDULER_SENTINEL', 'MISSION_CONTROL_SENTINEL', 'MCP_FOLLOWUP_SENTINEL']:
            assert sentinel not in history_text, history_text
        for request_body in requests:
            for item in request_body.get('input', []):
                if item.get('role') == 'user':
                    assert 'SENTINEL' not in json.dumps(item), item
        print('PASS: Mission Control, MCP follow-ups, and scheduler messages preserve the draft and stay out of user prompt history', flush=True)



    except Exception:
        if (work/'serve.log').exists():
            print('serve log:', (work/'serve.log').read_text()[-2000:])
        for path in (home/'.omar/codex-runtime').glob('*/server.log'):
            print(path, path.read_text()[-2000:])
        print(tmux('list-sessions', check=False))
        print('palette:', tmux('show-options', '-w', '-t', 'omar-agent-ea-1', 'window-style', check=False))
        print('color env:', tmux('show-environment', '-t', 'omar-agent-ea-1', 'COLORTERM', check=False))
        for name in ['omar-agent-ea-1', 'omar-agent-ea-2', 'omar-agent-ea-3', 'plain-resume']:
            print(name, tmux('capture-pane', '-p', '-t', name, check=False))
        raise
    finally:
        if daemon:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
            daemon_log.close()
        for rpc in clients:
            rpc.close()
        tmux('kill-server', check=False)
        http.shutdown()
        http.server_close()
