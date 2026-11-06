#!/usr/bin/env python3
"""Real Codex runs an invocation-scoped agent in a new thread every time.

`coder : Codex(scope = invocation)` is invoked twice, beside `keeper : Codex`
invoked twice, under a real `omar run` with the real codex (0.154.0 in CI,
or whichever is first on PATH) and its own app-server and TUI. The model is
a local deterministic Responses server, so no credentials are needed: it
answers each invocation by calling OMAR's tools, and records every request
codex sent it, which is the whole conversation the model was shown. HOME and
CODEX_HOME are disposable; nothing of the operator's codex is read.

Checked:
- the second coder request holds nothing of the first: its sentinel is in
  the first coder's requests and in none of the second's;
- the keeper's second request still holds its first, so the check can tell;
- two attempts, each recording the thread codex minted for it (a UUID, new,
  and verified as `reported`), and stopped, each with an app-server of its
  own; the config turns memories on, which the attempts' servers override
  (that they do is checked against the fake in invocation_scope.py).
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
OMARC = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()
UUID = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')

PROGRAM = '''team Pipe[coder : Codex(scope = invocation), keeper : Codex] {
    input start : string
    action mid : string
    action kmid : string
    output result : string
    output kept : string

    task coder(start) -> mid "Step one. The secret is SENTINEL-$(start)."
    task coder(mid) -> result "Step two. Got $(mid)."
    task keeper(start) -> kmid "Keeper one $(start)"
    task keeper(kmid) -> kept "Keeper two $(kmid)"
}

main Scoped {
    p = Pipe()
}
'''

requests = []


def turn(request):
    """The next tool call answering the latest task codex was handed, one per
    turn so they run in order: its port, then `omar_complete`, then a closing
    message."""
    items = request.get('input', [])
    text = json.dumps(items)
    last = items[-1] if items else {}
    done = last.get('call_id', '') if last.get('type') == 'function_call_output' else ''
    if done.startswith('call_complete'):
        return []
    task = int(re.findall(r'task_id: (\d+)', text)[-1])
    if done.startswith('call_set'):
        return [('call_complete', 'omar_complete', {'task_id': task})]
    # The latest task is the one named last.
    step = max([m for m in ['Step two', 'Step one', 'Keeper two', 'Keeper one'] if m in text],
               key=text.rindex)
    port, value = {
        'Step one': ('mid', 'ok'),
        'Step two': ('result', 'leaked' if 'SENTINEL' in text else 'fresh'),
        'Keeper one': ('kmid', 'k'),
        'Keeper two': ('kept', 'remembered' if 'Keeper one' in text else 'forgot'),
    }[step]
    return [('call_set', 'omar_set_port', {'task_id': task, 'port': port, 'value': value})]


def tool_names(request):
    """OMAR's tools as codex offers them to the model: by MCP name, flat or
    under a namespace, whichever this codex sends."""
    names = {}
    for tool in request.get('tools', []):
        for inner in [tool, *tool.get('tools', [])]:
            name = inner.get('name', '')
            for bare in ['omar_set_port', 'omar_complete']:
                if name.endswith(bare):
                    names[bare] = (tool.get('name') if inner is not tool else None, name)
    return names


class Responses(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get('content-length', '0'))))
        requests.append(request)
        n = len(requests)
        try:
            calls = turn(request)
        except (IndexError, ValueError):
            # Not a task: a title or other side request.
            calls = []
        tools = tool_names(request)
        output = []
        for kind, name, args in calls:
            namespace, full = tools[name]
            item = {'id': f'fc_{n}', 'type': 'function_call', 'status': 'completed',
                    'call_id': f'{kind}_{n}', 'name': full, 'arguments': json.dumps(args)}
            if namespace:
                item['namespace'] = namespace
            output.append(item)
        if not output:
            output = [{'id': f'msg_{n}', 'type': 'message', 'role': 'assistant',
                       'status': 'completed',
                       'content': [{'type': 'output_text', 'text': 'done', 'annotations': []}]}]
        response = {'id': f'resp_{n}', 'object': 'response', 'created_at': int(time.time()),
                    'status': 'completed', 'model': 'scope-test', 'output': output,
                    'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2,
                              'input_tokens_details': {'cached_tokens': 0},
                              'output_tokens_details': {'reasoning_tokens': 0}}}
        events = [{'type': 'response.created',
                   'response': {**response, 'status': 'in_progress', 'output': []}}]
        for index, item in enumerate(output):
            events.append({'type': 'response.output_item.added', 'output_index': index,
                           'item': {**item, 'status': 'in_progress'}})
            events.append({'type': 'response.output_item.done', 'output_index': index,
                           'item': item})
        events.append({'type': 'response.completed', 'response': response})
        body = ''.join(f'data: {json.dumps(e)}\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('content-type', 'text/event-stream')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    server = ThreadingHTTPServer(('127.0.0.1', 0), Responses)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    # Short: codex binds no socket path of 96 bytes or more.
    with tempfile.TemporaryDirectory(prefix='omar-cx-scope-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        home = root / 'home'
        codex_home = home / '.codex'
        codex_home.mkdir(parents=True)
        # Memories on, as an operator may have them: an attempt's server must
        # turn them off regardless.
        (codex_home / 'config.toml').write_text(f'''check_for_update_on_startup=false
model="scope-test"
model_provider="scope_test"
[features]
memories=true
[model_providers.scope_test]
name="scope test"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
[projects.{json.dumps(str(home))}]
trust_level="trusted"
''')
        version = subprocess.run(['codex', '--version'], capture_output=True, text=True)
        print((version.stdout + version.stderr).strip())
        program = root / 'Scoped.omar'
        program.write_text(PROGRAM)
        tmux = 'omar-cx-scope-' + root.name.rsplit('-', 1)[-1]
        inherited = {k: v for k, v in os.environ.items()
                     if not k.startswith(('OMAR_', 'CODEX_')) and k not in ('TMUX', 'TMUX_PANE')}
        environment = dict(inherited, HOME=str(home), CODEX_HOME=str(codex_home),
                           OMAR_TMUX_SERVER=tmux, OMARC_BIN=str(OMARC))
        session = ['-s', 'cx-scope-test']
        try:
            up = subprocess.run([str(BIN), 'up', *session, '--no-ea'], env=environment,
                                capture_output=True, text=True, timeout=120)
            assert up.returncode == 0, up.stdout + up.stderr
            result = subprocess.run(
                [str(BIN), 'run', *session, str(program), '--input', 'p.start=alpha',
                 '--timeout-seconds', '90', '--replace', '--fg'],
                env=environment, capture_output=True, text=True, timeout=400)
            attempts = [json.loads(line) for path in home.rglob('attempts.jsonl')
                        for line in path.read_text().splitlines()]
            servers = list(home.rglob('codex-runtime/*/server.log'))
        finally:
            subprocess.run([str(BIN), 'down', *session], env=environment, capture_output=True,
                           timeout=60)
            subprocess.run(['tmux', '-L', tmux, 'kill-server'], capture_output=True)
            server.shutdown()

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    # What the model was shown, per invocation.
    shown = [json.dumps(r.get('input', [])) for r in requests]
    first = [s for s in shown if 'Step one' in s]
    second = [s for s in shown if 'Step two' in s]
    assert first and second, shown
    assert all('SENTINEL-alpha' in s for s in first), first
    assert not any('SENTINEL' in s for s in second), second
    assert 'Output p.result = "fresh"' in output, output
    # The keeper's one conversation still held its first task.
    assert 'Output p.kept = "remembered"' in output, output

    finished = [a for a in attempts if a['outcome'] != 'started']
    assert [(a['outcome'], a['cleanup_status']) for a in finished] == \
        [('completed', 'stopped')] * 2, attempts
    ids = [a['native_session_id'] for a in finished]
    assert all(UUID.match(i or '') for i in ids) and ids[0] != ids[1], ids
    assert all(a['backend'] == 'Codex' and a['verification'] == 'reported'
               for a in attempts), attempts
    # One server per worker: the keeper's and each attempt's.
    assert len(servers) == 3, servers
    print(f'PASS: real codex ran p.coder in two threads ({ids[0]}, {ids[1]}); '
          'the second model request held nothing of the first; keeper kept its own')


if __name__ == '__main__':
    main()
