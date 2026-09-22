#!/usr/bin/env python3
"""Real OMAR spawn/delivery and Codex exec, with a local Responses provider.
Covers profile/config/search compatibility, two idle turns, native resume and
provider-visible flags. No credentials or model calls; isolated home and tmux.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from legacy_supervision import MCP, BIN

requests = []
class Mock(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        requests.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
        item = {'id': 'msg_' + str(len(requests)), 'type': 'message', 'role': 'assistant',
                'status': 'completed', 'content': [{'type': 'output_text', 'text': 'EXEC_ACK', 'annotations': []}]}
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for event in [
            {'type': 'response.created', 'response': {'id': 'resp_probe', 'status': 'in_progress', 'output': []}},
            {'type': 'response.output_item.added', 'output_index': 0, 'item': dict(item, status='in_progress', content=[])},
            {'type': 'response.output_text.delta', 'item_id': item['id'], 'output_index': 0, 'content_index': 0, 'delta': 'EXEC_ACK'},
            {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
            {'type': 'response.completed', 'response': {'id': 'resp_probe', 'status': 'completed', 'output': [item],
                'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}},
        ]:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.flush()

def main():
    http = ThreadingHTTPServer(('127.0.0.1', 0), Mock)
    threading.Thread(target=http.serve_forever, daemon=True).start()
    server = 'omar-exec-' + uuid.uuid4().hex[:10]
    parent = None
    with tempfile.TemporaryDirectory(prefix='omar-exec-', dir='/tmp') as directory:
        root = Path(directory)
        home = root / 'home'; codex_home = home / '.codex'; codex_home.mkdir(parents=True)
        # Named config layers are native Codex profiles in the pinned CLI.
        (codex_home / 'config.toml').write_text('model="wrong-without-profile"\n')
        (codex_home / 'probe.config.toml').write_text(f'''model="gpt-6-astra"
model_provider="probe"
[model_providers.probe]
name="probe"
base_url="http://127.0.0.1:{http.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
''')
        state = root / ('deep-state-' + 'd' * 90); state.mkdir()
        context = state / 'parent.json'
        context.write_text(json.dumps(dict(omar_dir=str(state), ea_id=0, session_prefix='reg-',
            default_command='cat', default_workdir=str(root), health_idle_warning=15, tmux_server=server)))
        env = dict(os.environ, HOME=str(home), CODEX_HOME=str(codex_home), OMAR_TMUX_SERVER=server)
        for key in ['TMUX', 'OMAR_AGENT_NAME', 'OMAR_EA_ID', 'OMAR_MCP_CONTEXT_FILE']:
            env.pop(key, None)
        def tmux(*args):
            return subprocess.run(['tmux', '-L', server, *args], env=env, text=True, capture_output=True, check=True).stdout
        def wait(check, label):
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                if check(): return
                time.sleep(.1)
            raise AssertionError(label + '\n' + tmux('capture-pane', '-p', '-t', 'reg-0-worker'))
        try:
            tmux('new-session', '-d', '-s', 'reg-ea-0', 'cat')
            parent = MCP(context, env)
            project = parent.call('add_project', name='native exec delivery')['project_id']
            child = parent.call('spawn_agent', name='worker', project_id=project, task='FIRST_EXEC_SENTINEL',
                command='codex --profile probe --search -c model_reasoning_effort=\'"low"\'')
            wait(lambda: list(state.rglob('protocol-*.json')), 'managed config missing')
            config_file = next(p for p in state.rglob('protocol-*.json') if not p.name.endswith('.inbox.json'))
            config = json.loads(config_file.read_text())
            inbox = config_file.with_suffix('.inbox.json')
            wait(lambda: inbox.exists() and requests and not json.loads(inbox.read_text())['pending'], 'startup did not complete')
            session_id = json.loads(inbox.read_text())['session']
            assert session_id
            assert len(str(config['socket']).encode()) < 96
            stamp = tmux('show-environment', '-t', 'reg-0-worker', 'OMAR_DELIVERY').strip()
            assert stamp == 'OMAR_DELIVERY=managed:' + config['socket'], stamp
            parent.call('send_input', name='worker', text='SECOND_EXEC_SENTINEL')
            wait(lambda: len(requests) >= 2 and not json.loads(inbox.read_text())['pending'], 'follow-up did not complete')
            assert json.loads(inbox.read_text())['session'] == session_id
            assert all(r['model'] == 'gpt-6-astra' for r in requests), requests
            assert all(r.get('reasoning', {}).get('effort') == 'low' for r in requests)
            assert 'web_search="live"' in shlex.split(config['command'])
            assert 'FIRST_EXEC_SENTINEL' in json.dumps(requests[-1])
            assert 'SECOND_EXEC_SENTINEL' in json.dumps(requests[-1])
            assert 'EXEC_ACK' in json.dumps(requests[-1]), 'native conversation not resumed'
            assert not (codex_home / 'history.jsonl').exists(), 'events entered prompt-recall history'
            print('PASS: real Codex profile/search/config launch, startup and idle follow-up, same native conversation, short socket, no prompt history', flush=True)
        finally:
            if parent: parent.stop()
            subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)
            http.shutdown(); http.server_close()

if __name__ == '__main__': main()
