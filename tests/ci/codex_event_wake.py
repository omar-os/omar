#!/usr/bin/env python3
"""Check OMAR's event contract against an installed Codex app-server.

Run: python3 tests/ci/codex_event_wake.py
Uses an isolated CODEX_HOME and a loopback-only mock provider; no model calls
or user credentials are needed. The ordinary Rust socket tests exercise OMAR's
implementation; this optional integration check verifies the server semantics.
"""
import json, os, selectors, subprocess, tempfile, threading
from http.server import HTTPServer, BaseHTTPRequestHandler

entered = threading.Event()
release = threading.Event()
class Mock(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        entered.set()
        release.wait(15)
        self.send_response(503)
        self.end_headers()
    def log_message(self, *args): pass
http = HTTPServer(('127.0.0.1', 0), Mock)
threading.Thread(target=http.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='omar-wake-check-') as work:
    env = dict(os.environ, CODEX_HOME=work)
    args = ['codex', 'app-server', '--listen', 'stdio://', '-c', 'model_provider="wake_test"', '-c', 'model="wake-test"', '-c', 'model_providers.wake_test.name="Local wake test"', '-c', f'model_providers.wake_test.base_url="http://127.0.0.1:{http.server_port}/v1"', '-c', 'model_providers.wake_test.wire_api="responses"', '-c', 'model_providers.wake_test.requires_openai_auth=false', '-c', 'model_providers.wake_test.supports_websockets=false']
    err = open(work+'/stderr', 'w')
    proc = subprocess.Popen(args, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err)
    sel = selectors.DefaultSelector(); sel.register(proc.stdout, selectors.EVENT_READ)
    pending = b''
    def send(data):
        proc.stdin.write((json.dumps(data)+'\n').encode()); proc.stdin.flush()
    def call(i, method, params):
        global pending
        send({'id': i, 'method': method, 'params': params})
        while True:
            while b'\n' in pending:
                line, pending = pending.split(b'\n', 1)
                value = json.loads(line)
                if value.get('id') == i:
                    if 'error' in value: raise RuntimeError(value)
                    return value['result']
            if not sel.select(15): raise TimeoutError(method)
            chunk = os.read(proc.stdout.fileno(), 65536)
            if not chunk: raise RuntimeError('server exited')
            pending += chunk
    try:
        call(1, 'initialize', {'clientInfo': {'name':'omar_wake_test', 'version':'1'}})
        send({'method':'initialized','params':{}})
        thread = call(2, 'thread/start', {'cwd':work, 'ephemeral':True, 'approvalPolicy':'never', 'sandbox':'read-only'})['thread']['id']
        params = {'threadId':thread, 'input':[], 'toolOutput':{'name':'omar_event','namespace':'omar','output':'test event'}}
        first = call(3, 'turn/start', params)
        assert first['turn']['status'] == 'inProgress', first
        second = call(4, 'turn/start', params)
        assert second['turn']['id'] == first['turn']['id'], (first, second)
        print('PASS: installed Codex started an idle tool-output turn; second event queued on the same active turn.')
        print('Both acknowledgements reference the same turn:', first['turn']['id'])
    finally:
        release.set()
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        http.shutdown()
        http.server_close()
        err.close()
