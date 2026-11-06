#!/usr/bin/env python3
"""Real Pi runs an invocation-scoped agent in a new conversation every time.

`coder : Pi(scope = invocation)` is invoked twice, beside `keeper : Pi`
invoked twice, under a real `omar run` with real pi (pinned, or PI_BINARY)
and OMAR's own extension. The model is a local deterministic Responses
server, so no credentials are needed: it answers each invocation by calling
OMAR's tools, and records every request pi sent it, which is the whole
conversation the model was shown.

Checked:
- the second coder request holds nothing of the first: its sentinel is in
  the first coder's requests and in none of the second's;
- the keeper's second request still holds its first, so the check can tell;
- two attempts, each recording the id it was given, as pi's extension
  reported it from pi's own session manager, and stopped.
"""
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
OMARC = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()
PI_VERSION = '0.85.1'
UUID = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')

PROGRAM = '''team Pipe[coder : Pi(scope = invocation), keeper : Pi] {
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
    """The next tool call answering the latest task pi was handed, one per
    turn so they run in order: its port, then `omar_complete`, then a closing
    message."""
    text = json.dumps(request.get('input', []))
    last = request['input'][-1] if request['input'] else {}
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


class Responses(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get('content-length', '0'))))
        requests.append(request)
        # Pi names OMAR's tools as its extension does: `omar_` and the MCP
        # name, itself `omar_set_port` and so on.
        tools = {t['name'].removeprefix('omar_'): t['name'] for t in request.get('tools', [])
                 if t.get('name', '').startswith('omar_')}
        calls = [(kind, tools[name], args) for kind, name, args in turn(request)]
        n = len(requests)
        if calls:
            output = [{'id': f'fc_{n}', 'type': 'function_call', 'status': 'completed',
                       'call_id': f'{kind}_{n}', 'name': name, 'arguments': json.dumps(args)}
                      for kind, name, args in calls]
        else:
            output = [{'id': f'msg_{n}', 'type': 'message', 'role': 'assistant',
                       'status': 'completed',
                       'content': [{'type': 'output_text', 'text': 'done', 'annotations': []}]}]
        response = {'id': f'resp_{n}', 'object': 'response', 'created_at': int(time.time()),
                    'status': 'completed', 'model': 'tree', 'output': output,
                    'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2,
                              'input_tokens_details': {'cached_tokens': 0},
                              'output_tokens_details': {'reasoning_tokens': 0}}}
        events = [{'type': 'response.created',
                   'response': {**response, 'status': 'in_progress', 'output': []}}]
        for index, item in enumerate(output):
            events.append({'type': 'response.output_item.added', 'output_index': index,
                           'item': {**item, 'status': 'in_progress'}})
            if item['type'] == 'function_call':
                events.append({'type': 'response.function_call_arguments.done',
                               'output_index': index, 'item_id': item['id'],
                               'arguments': item['arguments']})
            else:
                events.append({'type': 'response.output_text.done', 'output_index': index,
                               'item_id': item['id'], 'content_index': 0, 'text': 'done'})
            events.append({'type': 'response.output_item.done', 'output_index': index,
                           'item': item})
        events.append({'type': 'response.completed', 'response': response})
        body = (''.join(f'data: {json.dumps(e)}\n\n' for e in events) + 'data: [DONE]\n\n').encode()
        self.send_response(200)
        self.send_header('content-type', 'text/event-stream')
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def pi_command(root):
    """The pi to run: PI_BINARY, or the pinned version installed under root."""
    if os.environ.get('PI_BINARY'):
        return [os.environ['PI_BINARY']]
    prefix = root / 'pi-runtime'
    subprocess.run(['npm', 'install', '--prefix', str(prefix), '--no-save', '--no-package-lock',
                    '--no-audit', '--no-fund', f'@earendil-works/pi-coding-agent@{PI_VERSION}'],
                   check=True, stdin=subprocess.DEVNULL, capture_output=True)
    cli = prefix / 'node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js'
    return [subprocess.check_output(['sh', '-c', 'command -v node'], text=True).strip(), str(cli)]


def main():
    server = ThreadingHTTPServer(('127.0.0.1', 0), Responses)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='omar-pi-scope-') as directory:
        root = Path(directory)
        home = root / 'home'
        shims = home / 'shims'
        agent_dir = home / '.pi' / 'agent'
        shims.mkdir(parents=True)
        agent_dir.mkdir(parents=True)
        # Nothing of the operator's pi: no packages, models or credentials.
        (agent_dir / 'settings.json').write_text(json.dumps({
            'defaultProvider': 'omar-pi-e2e',
            'defaultModel': 'tree',
            'extensions': [str(REPO / 'src/backend/pi/test/fixtures/deterministic-provider.mjs')],
        }))
        shim = shims / 'pi'
        shim.write_text(f'#!/bin/sh\nexec {shlex.join(pi_command(root))} "$@"\n')
        shim.chmod(0o700)
        version = subprocess.run([str(shim), '--version'], capture_output=True, text=True)
        print(f'pi {(version.stdout + version.stderr).strip()}')
        # Panes start under `sh -lc`, so this is what each pi sees.
        (home / '.profile').write_text(
            f'export PATH="{shims}:$PATH"\n'
            f'export PI_CODING_AGENT_DIR="{agent_dir}"\n'
            'export PI_OFFLINE=1\n'
            f'export PI_E2E_BASE_URL="http://127.0.0.1:{server.server_port}/v1"\n')
        program = root / 'Scoped.omar'
        program.write_text(PROGRAM)
        tmux = 'omar-pi-scope-' + root.name.rsplit('-', 1)[-1]
        inherited = {k: v for k, v in os.environ.items()
                     if not k.startswith(('OMAR_', 'PI_')) and k not in ('TMUX', 'TMUX_PANE')}
        environment = dict(inherited, HOME=str(home), OMAR_TMUX_SERVER=tmux,
                           OMARC_BIN=str(OMARC))
        session = ['-s', 'pi-scope-test']
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
    assert all(a['backend'] == 'Pi' and a['verification'] == 'chosen' for a in attempts), attempts
    print(f'PASS: real pi ran p.coder in two conversations ({ids[0]}, {ids[1]}); '
          'the second model request held nothing of the first; keeper kept its own')


if __name__ == '__main__':
    main()
