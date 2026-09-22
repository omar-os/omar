#!/usr/bin/env python3
"""A slow backend must remain addressable after its launcher exits.

Runs real Omar and tmux with a model-free OpenCode HTTP fixture. No vendor
installation, credentials, or user configuration is used.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

OMAR = str(Path(os.environ.get('OMAR_BIN', 'target/debug/omar')).resolve())
with tempfile.TemporaryDirectory(prefix='omar-channel-startup-') as folder:
    root = Path(folder)
    (root/'.omar').mkdir()
    backend = root/'opencode'
    backend.write_text('''#!/usr/bin/env python3
import json, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
# Longer than ensure_manager_running's old two-second startup sleep.
time.sleep(4)
port = int(sys.argv[sys.argv.index('--port') + 1])
log = Path(__file__).with_name('requests.jsonl')
class Handler(BaseHTTPRequestHandler):
    selections = 0
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with log.open('a') as out:
            out.write(json.dumps({'path': self.path, 'body': body}) + '\\n')
        status = 200
        if self.path == '/session':
            result = {'id': 'startup-session'}
        elif self.path == '/tui/select-session':
            Handler.selections += 1
            status = 503 if Handler.selections == 1 else 200
            result = {}
        elif self.path == '/session/startup-session/prompt_async':
            status, result = 204, None
        else:
            status, result = 404, {}
        raw = json.dumps(result).encode() if result is not None else b''
        self.send_response(status)
        self.send_header('Content-Length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)
    def log_message(self, *args): pass
HTTPServer(('127.0.0.1', port), Handler).serve_forever()
''')
    backend.chmod(0o700)
    (root/'.omar/config.toml').write_text(
        '[agent]\ndefault_command = '+json.dumps(str(backend))+
        '\ndefault_workdir = '+json.dumps(folder)+'\n')
    server = 'omar-startup-'+str(os.getpid())
    env = dict(os.environ, HOME=folder, OMAR_TMUX_SERVER=server)
    for key in ['TMUX', 'OMAR_DIR', 'OMAR_EA_ID']:
        env.pop(key, None)
    try:
        launched = subprocess.run([OMAR, 'manager', 'start'], cwd=folder, env=env,
                                  capture_output=True, text=True, timeout=30)
        # Non-TTY attach can fail after setup. Inspect the actual channel, not
        # that attach status, and only after the launcher has fully exited.
        stamp = subprocess.run(['tmux', '-L', server, 'show-environment', '-t',
                                'omar-agent-ea-0', 'OMAR_DELIVERY'],
                               capture_output=True, text=True)
        assert stamp.returncode == 0, (launched.stdout, launched.stderr, stamp.stderr)
        assert stamp.stdout.strip().endswith(':startup-session'), stamp.stdout
        context = dict(omar_dir=str(root/'.omar'), ea_id=0, session_prefix='omar-agent-',
                       default_command=str(backend), default_workdir=folder,
                       health_idle_warning=15, tmux_server=server, topology=None, serve=None)
        (root/'context.json').write_text(json.dumps(context))
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {
            'name': 'send_input', 'arguments': {
                'name': 'omar-agent-ea-0', 'text': 'AFTER_LAUNCHER_EXIT'}}}
        sent = subprocess.run([OMAR, 'mcp-server', '--context-file', str(root/'context.json')],
                              cwd=folder, env=env, input=json.dumps(request)+'\n',
                              capture_output=True, text=True, check=True, timeout=20)
        reply = json.loads(sent.stdout)
        assert 'error' not in reply and not reply['result'].get('isError'), reply
        recorded = [json.loads(line) for line in (root/'requests.jsonl').read_text().splitlines()]
        assert sum(r['path'] == '/session' for r in recorded) == 1, recorded
        assert sum(r['path'] == '/tui/select-session' for r in recorded) == 2, recorded
        deliveries = [r['body'] for r in recorded if r['path'].endswith('/prompt_async')]
        # No `noReply`: a bare prompt_async is what wakes an idle session, which
        # tests/ci/opencode_wake_contract.py pins against the installed OpenCode.
        assert deliveries == [{'parts': [
            {'type': 'text', 'text': 'AFTER_LAUNCHER_EXIT', 'synthetic': True}]}], deliveries
        print('PASS: slow backend setup survives launcher exit, retries TUI selection, and delivers once')
    finally:
        subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)
