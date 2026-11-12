#!/usr/bin/env python3
"""Kill an owned workflow worker after buffered writes; retry or fail after two retries.

Local providers and deterministic protocol peers; no credentials without --live.
--live uses the real CLI credential stores in place; only OMAR state is private.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from scratch_session import scratch
from codex_invocation_scope import BIN, OMARC as DEFAULT_OMARC, tool_names
from pi_invocation_scope import pi_command
from invocation_scope import COMMON, FAKE

REPO = Path(__file__).resolve().parents[2]
OMARC = Path(os.environ.get('OMARC_BIN', DEFAULT_OMARC)).resolve()
PROGRAM = '''team Work[coder : BACKENDLIFETIME] {
    input start : string
    output result : string
    task coder(start) -> result "PROMPT"
}
main Restart {
    p = Work()
}
'''


class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get('content-length', '0'))))
        self.server.requests.append(request)
        scenario = getattr(self.server, 'scenario', None)
        chat = 'messages' in request
        items = request.get('messages' if chat else 'input', [])
        text = json.dumps(items)
        tasks = re.findall(r'task_id: (\d+)', text)
        if not tasks:
            calls = []
        else:
            task = int(tasks[-1])
            last = items[-1] if items else {}
            done = last.get('tool_call_id' if chat else 'call_id', '')
            reminded = re.search(r'OMAR REMINDER.{0,20}task_id: ' + str(task) + r'\D', text)
            if scenario == 'ignore' or (scenario == 'complete' and not reminded):
                calls = []
            elif done.startswith('call_set'):
                if task not in self.server.killed and len(self.server.killed) < self.server.deaths:
                    self.server.killed.add(task)
                    self.server.kill.set()
                    self.server.release.wait(30)
                    self.server.release.clear()
                    # The socket belongs to the dead worker.
                    calls = []
                else:
                    calls = [('call_complete', 'omar_complete', {'task_id': task})]
            elif done.startswith('call_complete'):
                calls = []
            else:
                if scenario == 'working' and task not in self.server.waited:
                    self.server.waited.add(task)
                    time.sleep(6)
                calls = [('call_set', 'omar_set_port', {'task_id': task, 'port': 'result',
                           'value': 'recovered' if scenario or 'This is retry' in text else 'discard-me'})]
        n = len(self.server.requests)
        if chat:
            names = {bare: t['function']['name'] for t in request.get('tools', [])
                     for bare in ['omar_set_port', 'omar_complete']
                     if t.get('function', {}).get('name', '').endswith(bare)}
            delta = {'role': 'assistant', 'tool_calls': [
                {'index': i, 'id': f'{kind}_{n}', 'type': 'function',
                 'function': {'name': names[name], 'arguments': json.dumps(args)}}
                for i, (kind, name, args) in enumerate(calls)]} if calls else {'role': 'assistant', 'content': 'done'}
            chunks = [{'id': f'chat-{n}', 'object': 'chat.completion.chunk', 'created': int(time.time()),
                       'model': 'mock', 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                      {'id': f'chat-{n}', 'object': 'chat.completion.chunk', 'created': int(time.time()),
                       'model': 'mock', 'choices': [{'index': 0, 'delta': {},
                                                   'finish_reason': 'tool_calls' if calls else 'stop'}]}]
        else:
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
                output = [{'id': f'msg_{n}', 'type': 'message', 'role': 'assistant', 'status': 'completed',
                           'content': [{'type': 'output_text', 'text': 'done', 'annotations': []}]}]
            response = {'id': f'resp_{n}', 'object': 'response', 'created_at': int(time.time()),
                        'status': 'completed', 'model': 'tree', 'output': output,
                        'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2,
                                  'input_tokens_details': {'cached_tokens': 0},
                                  'output_tokens_details': {'reasoning_tokens': 0}}}
            chunks = [{'type': 'response.created', 'response': {**response, 'status': 'in_progress', 'output': []}}]
            for index, item in enumerate(output):
                chunks.append({'type': 'response.output_item.added', 'output_index': index,
                               'item': {**item, 'status': 'in_progress'}})
                if item['type'] == 'function_call':
                    chunks.append({'type': 'response.function_call_arguments.done', 'output_index': index,
                                   'item_id': item['id'], 'arguments': item['arguments']})
                chunks.append({'type': 'response.output_item.done', 'output_index': index, 'item': item})
            chunks.append({'type': 'response.completed', 'response': response})
        body = (''.join(f'data: {json.dumps(chunk)}\n\n' for chunk in chunks) + 'data: [DONE]\n\n').encode()
        try:
            self.send_response(200)
            self.send_header('content-type', 'text/event-stream')
            self.send_header('content-length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


FAKE_ANSWER = r'''
def answer(text):
    import time
    scenario = os.environ.get("REMIND_SCENARIO")
    if scenario:
        write({"event": "prompt", "text": text})
        if scenario == "working": time.sleep(6)
        elif scenario == "ignore" or "OMAR REMINDER" not in text: return
    task = int(re.findall(r"task_id: (\d+)", text)[-1])
    call("omar_set_port", {"task_id": task, "port": "result", "value": "recovered" if scenario or "This is retry" in text else "discard-me"})
    count = Path(os.environ["RESTART_COUNT"])
    attempt = int(count.read_text()) + 1 if count.exists() else 1
    count.write_text(str(attempt))
    if attempt <= int(os.environ["RESTART_DEATHS"]):
        Path(os.environ["RESTART_READY"]).touch()
        while True: time.sleep(.1)
    call("omar_complete", {"task_id": task})
'''

FAKE_ANSWER += r'''
_answer = answer
def answer(text):
    import shlex
    def hook(event):
        if flag("--settings"):
            for entry in json.loads(flag("--settings")).get("hooks", {}).get(event, []):
                for handler in entry["hooks"]:
                    subprocess.run(shlex.split(handler["command"]), input=json.dumps({"hook_event_name": event}),
                                   text=True, stdout=subprocess.DEVNULL, check=True)
    hook("UserPromptSubmit")
    _answer(text)
    hook("Stop")
'''

FAKE_MANAGED = COMMON + r'''
context = json.loads(Path(os.environ["OMAR_MCP_CONTEXT_FILE"]).read_text())
server = [BINARY, "mcp-server", "--context-file", os.environ["OMAR_MCP_CONTEXT_FILE"]]
topology = context["topology"]
session = "native-session"
''' + FAKE_ANSWER + r'''
for line in sys.stdin:
    request = json.loads(line)
    if "acp" in argv:
        method = request.get("method")
        if method in ["session/new", "session/load"]:
            assert request["params"]["mcpServers"][0]["name"] == "omar"
            assert request["params"]["mcpServers"][0]["args"][-1] == os.environ["OMAR_MCP_CONTEXT_FILE"]
        result = {"sessionId": session} if method == "session/new" else {}
        if method == "session/prompt":
            answer(request["params"]["prompt"][0]["text"])
            result = {"stopReason": "end_turn"}
        if "id" in request:
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    else:
        assert "-p=" in argv, "Antigravity stream input requires print mode"
        print(json.dumps({"event": "init", "conversation_id": session}), flush=True)
        answer(request["message"]["content"])
        print(json.dumps({"event": "result", "result": {"status": "SUCCESS"}}), flush=True)
'''


def isolated_environment(root):
    home = root / 'home'
    home.mkdir()
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('OMAR_', 'CODEX_', 'PI_', 'OPENCODE_', 'CLAUDE_',
                                'CURSOR_', 'GEMINI_', 'JETSKI_', 'AGY_', 'XDG_', 'AGENT_'))
           and not any(word in k.upper() for word in ['API_KEY', 'TOKEN', 'SECRET', 'PASSWORD'])
           and k not in ('TMUX', 'TMUX_PANE')}
    env.update(HOME=str(home), CODEX_HOME=str(home / '.codex'),
               OMAR_TMUX_SERVER='restart-' + root.name.rsplit('-', 1)[-1],
               TMUX_TMPDIR=str(root / 'tmux'), OMARC_BIN=str(OMARC),
               XDG_CONFIG_HOME=str(home / '.config'), XDG_DATA_HOME=str(home / '.local/share'),
               XDG_STATE_HOME=str(home / '.local/state'), XDG_CACHE_HOME=str(home / '.cache'))
    return home, env


LIVE_MODELS = {'Pi': 'arena-gateway/gpt-6-luna',
               'OpenCode': 'opencode/deepseek-v4.1-flash-free',
               'Agy': 'gemini-3.8-flash-low'}


def live_environment(root):
    # OAuth refresh tokens must never be cloned: refreshing a copy revokes
    # the operator's original. Keep all native credential locations in place.
    env = {k: v for k, v in os.environ.items()
           if not k.startswith('OMAR_') and k not in ('TMUX', 'TMUX_PANE')}
    env.update(OMAR_HOME=str(root / 'omar'), TMUX_TMPDIR=str(root / 'tmux'),
               OMAR_TMUX_SERVER='restart-' + root.name, OMARC_BIN=str(OMARC))
    return env


def preflight(root, env, backend, phase='preflight'):
    commands = {
        'Claude': ['claude', '-p', '--no-session-persistence', '--tools', '', 'Reply with exactly: OK'],
        'Codex': ['codex', 'exec', '--skip-git-repo-check', 'Reply with exactly: OK'],
        'Cursor': ['cursor-agent', 'status', '--format', 'json'],
        'Agy': ['agy', 'models'],
        'OpenCode': ['opencode', 'run', '-m', LIVE_MODELS['OpenCode'], '--format', 'json', 'Reply with exactly: OK'],
        'Pi': ['pi', '--model', LIVE_MODELS['Pi'], '-p', 'Reply with exactly: OK'],
    }
    process = subprocess.Popen(commands[backend], cwd=root, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=120)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.communicate()
        raise AssertionError(f'{backend} {phase} timed out')
    passed = process.returncode == 0
    if backend == 'Cursor':
        passed = passed and json.loads(stdout).get('isAuthenticated') is True
    elif backend != 'Agy':
        passed = passed and 'OK' in stdout
    # Never persist or dump auth responses, environment values or CLI logs.
    if not passed:
        raise AssertionError(f'{backend} {phase} failed (exit {process.returncode}); '
                             'real CLI did not confirm its sign-in')
    print(f'PASS: {backend} {phase}: real credential store', flush=True)


@contextmanager
def live_scratch(backend):
    with tempfile.TemporaryDirectory(prefix='omar-live-', dir='/tmp') as directory:
        root = Path(directory).resolve()
        (root / 'tmux').mkdir()
        env = live_environment(root)
        preflight(root, env, backend)
        try:
            yield root, dict(env)
        finally:
            try:
                if (root / 'omar/sessions/restart-test/launch.json').exists():
                    result = subprocess.run([str(BIN), 'rm', '-s', 'restart-test'], cwd=root,
                                            env=env, capture_output=True, timeout=90)
                    assert result.returncode == 0, f'{backend}: isolated OMAR cleanup failed'
            finally:
                preflight(root, env, backend, 'post-run sign-in check')


def configure_live(root, env, backend):
    # Select inexpensive models through command arguments, never user settings.
    if backend in LIVE_MODELS:
        command = {'Pi': 'pi', 'OpenCode': 'opencode', 'Agy': 'agy'}[backend]
        executable = shutil.which(command, path=env['PATH'])
        shims = root / 'shims'
        shims.mkdir()
        shim = shims / command
        args = f'--model {LIVE_MODELS[backend]} "$@"' if backend != 'OpenCode' else f'"$@" --model {LIVE_MODELS[backend]}'
        shim.write_text(f'#!/bin/sh\nexec {shlex.quote(executable)} {args}\n')
        shim.chmod(0o700)
        # macOS login shells reorder PATH. Reapply the test's model shim in
        # only the panes this private tmux server starts, without a profile.
        tmux = shutil.which('tmux', path=env['PATH'])
        launcher = shims / 'tmux'
        prefix = 'export PATH=' + shlex.quote(str(shims)) + ':"$PATH"; '
        launcher.write_text('#!/usr/bin/env python3\nimport os,sys\na=sys.argv[1:]\n'
                            'if "new-session" in a and "-lc" in a:\n'
                            f' i=a.index("-lc")+1; a[i]={prefix!r}+a[i]\n'
                            f'os.execv({tmux!r}, [{tmux!r}, *a])\n')
        launcher.chmod(0o700)
        env['PATH'] = str(shims) + ':' + env['PATH']


def configure(root, home, env, backend, server, live):
    if live:
        configure_live(root, env, backend)
        return
    profile = []
    if getattr(server, 'scenario', None):
        env['REMIND_SCENARIO'] = server.scenario
        if backend == 'Stub':
            env['OMAR_STUB_STOP_TURNS'] = '0' if server.scenario == 'working' else '1' if server.scenario == 'complete' else '100'
            env['OMAR_STUB_WORK_MILLIS'] = '6000' if server.scenario == 'working' else '0'
            profile += [f'export {name}={env[name]}' for name in ['OMAR_STUB_STOP_TURNS', 'OMAR_STUB_WORK_MILLIS']]
    if backend in ['Claude', 'Cursor', 'Agy']:
        shims = home / 'shims'
        shims.mkdir()
        fake = FAKE.replace('def serve(conn):', FAKE_ANSWER + '\ndef serve(conn):') if backend == 'Claude' else FAKE_MANAGED
        command = {'Claude': 'claude', 'Cursor': 'cursor-agent', 'Agy': 'agy'}[backend]
        shim = shims / command
        shim.write_text('#!/usr/bin/env python3\n' + f'LOG={str(root / "fake.jsonl")!r}\nBINARY={str(BIN)!r}\n' + fake)
        shim.chmod(0o700)
        env['PATH'] = str(shims) + ':' + env['PATH']
        env.update(RESTART_READY=str(root / 'ready'), RESTART_COUNT=str(root / 'count'), RESTART_DEATHS=str(server.deaths))
        profile.append(f'export PATH="{shims}:$PATH"')
    elif backend == 'Codex':
        codex_home = home / '.codex'
        codex_home.mkdir()
        (codex_home / 'config.toml').write_text(f'''check_for_update_on_startup=false
model="tree"
model_provider="restart_test"
[model_providers.restart_test]
name="restart test"
base_url="http://127.0.0.1:{server.server_port}/v1"
wire_api="responses"
requires_openai_auth=false
supports_websockets=false
[projects.{json.dumps(str(home))}]
trust_level="trusted"
''')
    elif backend == 'Pi':
        agent = home / '.pi/agent'
        agent.mkdir(parents=True)
        (agent / 'settings.json').write_text(json.dumps({
            'defaultProvider': 'omar-pi-e2e', 'defaultModel': 'tree',
            'extensions': [str(REPO / 'src/backend/pi/test/fixtures/deterministic-provider.mjs')]}))
        shims = home / 'shims'
        shims.mkdir()
        shim = shims / 'pi'
        shim.write_text(f'#!/bin/sh\nexec {shlex.join(pi_command(root))} "$@"\n')
        shim.chmod(0o700)
        profile += [f'export PATH="{shims}:$PATH"', f'export PI_CODING_AGENT_DIR="{agent}"',
                    'export PI_OFFLINE=1', f'export PI_E2E_BASE_URL="http://127.0.0.1:{server.server_port}/v1"']
    elif backend == 'OpenCode':
        config = home / '.config/opencode'
        config.mkdir(parents=True)
        (config / 'opencode.json').write_text(json.dumps({
            'enabled_providers': ['restart_mock'], 'model': 'restart_mock/mock',
            'provider': {'restart_mock': {'npm': '@ai-sdk/openai-compatible', 'name': 'local',
                'options': {'baseURL': f'http://127.0.0.1:{server.server_port}/v1', 'apiKey': 'local'},
                'models': {'mock': {'name': 'Mock', 'limit': {'context': 32000, 'output': 1000}}}}}}))
        env.update(OPENCODE_DISABLE_DEFAULT_PLUGINS='1', OPENCODE_DISABLE_MODELS_FETCH='1')
    if backend == 'Stub':
        # Delay only our Stub's pane so it can be killed after delivery.
        tmux = shutil.which('tmux')
        shims = root / 'shims'
        shims.mkdir()
        shim = shims / 'tmux'
        shim.write_text('#!/usr/bin/env python3\nimport os,sys\na=sys.argv[1:]\n'
                        'if "new-session" in a:\n'
                        ' for i,v in enumerate(a):\n'
                        '  if " stub-agent " in v: a[i]="sleep 3; "+v\n'
                        f'os.execv({tmux!r}, [{tmux!r}, *a])\n')
        shim.chmod(0o700)
        env['PATH'] = str(shims) + ':' + env['PATH']
    (home / '.profile').write_text('\n'.join(profile) + '\n')


def execute(backend, per=False, deaths=1, live=False, scenario=None):
    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    server.requests, server.killed, server.deaths = [], set(), deaths
    server.scenario, server.waited = scenario, set()
    server.kill, server.release = threading.Event(), threading.Event()
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with (live_scratch(backend) if live else scratch('omar-restart-', BIN, 'restart-test')) as workspace:
            if live:
                root, env = workspace
                home = root / 'omar'
                state = home
            else:
                root = workspace
                home, env = isolated_environment(root)
                state = home / '.omar'
            configure(root, home, env, backend, server, live)
            marker = root / 'ready'
            prompt = ('First execute this exact shell command: touch .restart-ready; sleep 8. '
                      'Then set result to recovered and complete. Answer one word.') if live else 'Return one word.'
            if live and scenario:
                prompt = ('First reply exactly Ready? and end your turn waiting for confirmation. '
                          'Do not call any tools in that first turn. On a later message, set result to recovered and complete.')
            program = root / 'Restart.omar'
            program.write_text(PROGRAM.replace('BACKEND', backend).replace('LIFETIME', ' per task' if per else '').replace('PROMPT', prompt))
            up = subprocess.run([str(BIN), 'up', '-s', 'restart-test', '--no-ea'],
                                env=env, cwd=root, capture_output=True, text=True, timeout=90)
            assert up.returncode == 0, up.stdout + up.stderr
            launch = json.loads((state / 'sessions/restart-test/launch.json').read_text())
            env['OMAR_TMUX_SERVER'] = launch['session']['tmux_server']
            began = time.monotonic()
            process = subprocess.Popen([str(BIN), 'run', '-s', 'restart-test', str(program),
                                        '--input', 'p.start=go', '--timeout-seconds', '180' if live else '90', '--replace', '--fg'],
                                       env=env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            killed = []
            errors = []
            screens = []

            def kill_workers():
                try:
                    end = time.monotonic() + (240 if live else 150)
                    while process.poll() is None and time.monotonic() < end:
                        if live:
                            capture = subprocess.run(['tmux', '-L', env['OMAR_TMUX_SERVER'], 'capture-pane', '-S', '-120', '-pt', 'omar-agent-0-p_coder'], env=env, capture_output=True, text=True)
                            if capture.returncode == 0 and capture.stdout.strip():
                                screens[:] = [re.sub(r'https?://\S+', '<url>', capture.stdout[-2400:])]
                        if len(killed) >= deaths:
                            time.sleep(.1)
                            continue
                        ready = (any(state.rglob('worktree/.restart-ready')) if live else
                                 marker.exists() if backend in ['Claude', 'Cursor', 'Agy'] else server.kill.is_set())
                        if backend == 'Stub':
                            ready = any('OMAR INVOCATION' in path.read_text() for path in state.rglob('events/*.jsonl'))
                        if not ready:
                            time.sleep(.05)
                            continue
                        panes = subprocess.run(['tmux', '-L', env['OMAR_TMUX_SERVER'], 'list-panes', '-a', '-F',
                                                '#{session_name}|#{pane_pid}|#{pane_dead}'],
                                               env=env, capture_output=True, text=True)
                        owned = [line.split('|') for line in panes.stdout.splitlines() if 'coder' in line]
                        if not owned or owned[0][2] == '1':
                            time.sleep(.05)
                            continue
                        pid = int(owned[0][1])
                        if pid in killed:
                            time.sleep(.05)
                            continue
                        # Only a pane created by this private server, never a name-pattern kill.
                        if not live:
                            time.sleep(.3)
                        try:
                            os.kill(pid, signal.SIGKILL)
                        except ProcessLookupError:
                            continue
                        killed.append(pid)
                        if not live:
                            marker.unlink(missing_ok=True)
                        server.kill.clear()
                        server.release.set()
                except Exception as error:
                    errors.append(error)

            killer = threading.Thread(target=kill_workers, daemon=True)
            if not scenario:
                killer.start()
            try:
                stdout, stderr = process.communicate(timeout=300 if live else 240)
            except subprocess.TimeoutExpired:
                process.kill()
                stdout, stderr = process.communicate()
                raise AssertionError('run timed out: ' + stdout + stderr)
            if not scenario:
                killer.join(timeout=2)
            output = stdout + stderr
            attempts = {}
            started_attempts = []
            for path in state.rglob('attempts.jsonl'):
                for line in path.read_text().splitlines():
                    record = json.loads(line)
                    if record['outcome'] == 'started' and record.get('retry', 0) > 0:
                        started_attempts.append(record['attempt_id'])
                    attempts[record['attempt_id']] = record
            records = list(attempts.values())
            assert len(started_attempts) == len(set(started_attempts)), started_attempts
            detail = output + '\n' + json.dumps(records) + ('\nLast worker screen:\n' + ''.join(screens) if live else '')
            if not live and backend == 'Agy':
                assert not (home / '.gemini/config').exists(), 'workflow MCP must be session-scoped'
            assert not errors, f'{errors}\n{detail}'
            if scenario:
                reminders = [r for a in records for r in a.get('reminders', [])]
                assert all(len(a.get('reminders', [])) <= 1 for a in records), detail
                if scenario == 'ignore':
                    assert len(records) == 3 and len(reminders) == 3, detail
                    assert process.returncode != 0, detail
                    assert 'after 2 retries' in output and 'stopped without finishing after a reminder' in output, detail
                    assert all(a['death_reason'] == 'stopped without finishing after a reminder' for a in records), detail
                    assert all(a['cleanup_status'] == 'stopped' for a in records), detail
                    assert len({r['task_id'] for r in reminders}) == 3, detail
                    outcome = 'one reminder per attempt, failed after 2 retries'
                else:
                    assert process.returncode == 0 and 'Output p.result = ' in output, detail
                    assert (not records if scenario == 'working' and not per else len(records) == 1 and records[0]['retry'] == 0), detail
                    assert len(reminders) == (0 if scenario == 'working' else 1), detail
                    if scenario == 'working':
                        assert time.monotonic() - began >= 6, 'working fixture did not stay active'
                    outcome = 'still working, no reminder' if scenario == 'working' else 'one reminder, completed'
                print(f'PASS: {backend}{" per task" if per else ""} {"live" if live else "local provider"}: {outcome}', flush=True)
                return
            assert len(killed) == deaths, detail
            assert len([a for a in records if a['outcome'] == 'died']) == deaths, detail
            assert len(records) == min(deaths + 1, 3), detail
            if deaths <= 2:
                assert process.returncode == 0, detail
                expected = 'stub' if backend == 'Stub' else 'recovered'
                assert ('Output p.result = ' in output if live else f'Output p.result = "{expected}"' in output), detail
                assert records[-1]['retry'] == deaths, detail
                if per:
                    assert len({a['native_session_id'] for a in records}) == len(records), detail
                elif backend != 'Stub':
                    assert records[-1]['conversation_mode'] == 'resumed native conversation', detail
                    assert records[0]['native_session_id'] == records[-1]['native_session_id'], detail
            else:
                assert process.returncode != 0, detail
                for expected in ['p.coder', 'task ', 'after 2 retries', 'worker process exited']:
                    assert expected in output, detail
            print(f'PASS: {backend}{" per task" if per else ""} {"live" if live else "local provider"}: '
                  f'{deaths} worker death(s), ' + ('completed' if deaths <= 2 else 'failed after 2 retries'), flush=True)
    finally:
        server.release.set()
        server.shutdown()
        server.server_close()


def execute_jev(deaths):
    with scratch('omar-restart-', BIN, 'restart-test') as root:
        home, env = isolated_environment(root)
        shims = root / 'shims'
        shims.mkdir()
        count = root / 'calls'
        curl = shims / 'curl'
        response = {'model': 'jev-test', 'answers': {
            'decision': {'type': 'choice', 'choice': 'ok', 'confidence': .99, 'probabilities': {'ok': .99, 'no': .01}},
            'sufficient_context': {'type': 'noul', 'noul': .99}}}
        curl.write_text('#!/usr/bin/env python3\nimport sys\nfrom pathlib import Path\n'
                        'sys.stdin.read()\n' + f'p=Path({str(count)!r})\n'
                        'n=int(p.read_text())+1 if p.exists() else 1\np.write_text(str(n))\n'
                        f'print("unavailable\\n503" if n <= {deaths} else {json.dumps(response)!r}+"\\n200", end="")\n')
        curl.chmod(0o700)
        env.update(PATH=str(shims) + ':' + env['PATH'], TYPESAFE_API_KEY='test-only', OMAR_JEV_MODEL='jev-test')
        (root / 'decision.json').write_text(json.dumps({'type': 'string', 'enum': ['ok', 'no']}))
        program = root / 'Jev.omar'
        program.write_text('''type Decision from "./decision.json"
team Work[coder : Jev] {
    input start : string
    output result : Decision
    task coder(start) -> result "Choose ok."
}
main Restart { p = Work() }
''')
        up = subprocess.run([str(BIN), 'up', '-s', 'restart-test', '--no-ea'], env=env, capture_output=True, text=True, timeout=90)
        assert up.returncode == 0, up.stdout + up.stderr
        result = subprocess.run([str(BIN), 'run', '-s', 'restart-test', str(program), '--input', 'p.start=go',
                                 '--timeout-seconds', '30', '--fg'], env=env, capture_output=True, text=True, timeout=90)
        output = result.stdout + result.stderr
        assert int(count.read_text()) == min(deaths + 1, 3), output
        if deaths < 3:
            assert result.returncode == 0 and 'Output p.result = "ok"' in output, output
        else:
            assert result.returncode != 0 and 'after 2 retries' in output and 'HTTP 503' in output, output
        attempts = [json.loads(line) for path in home.rglob('attempts.jsonl') for line in path.read_text().splitlines()]
        assert len([item for item in attempts if item['outcome'] == 'died']) == deaths, attempts
        print(f'PASS: Jev mock HTTP: {deaths} failure(s), ' + ('completed' if deaths < 3 else 'failed after 2 retries'), flush=True)


def verify_live_isolation():
    from unittest.mock import patch
    # A failed workflow still checks the original store and removes its state.
    with tempfile.TemporaryDirectory(prefix='omar-auth-test-', dir='/tmp') as directory:
        fixture = Path(directory)
        native = fixture / 'native'
        native.mkdir()
        shims = fixture / 'bin'
        shims.mkdir()
        codex = shims / 'codex'
        codex.write_text('#!/usr/bin/env python3\nimport os\nfrom pathlib import Path\n'
                         'p=Path(os.environ["HOME"])/"checks"\n'
                         'p.write_text(p.read_text()+"check\\n" if p.exists() else "check\\n")\n'
                         'print("OK")\n')
        codex.chmod(0o700)
        pi = shims / 'pi'
        pi.write_text('#!/bin/sh\nexit 0\n')
        pi.chmod(0o700)
        names = ['HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'CURSOR_CONFIG_DIR',
                 'GEMINI_DIR', 'JETSKI_APP_DATA_DIR', 'XDG_CONFIG_HOME',
                 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'PI_CODING_AGENT_DIR']
        inherited = {name: str(native) for name in names}
        inherited.update(PATH=str(shims) + ':' + os.environ['PATH'], ARENA_API_KEY='test-only-secret')
        with patch.dict(os.environ, inherited, clear=True), patch('builtins.print'):
            try:
                with live_scratch('Codex') as (root, env):
                    assert all(env[name] == inherited[name] for name in names)
                    assert env['ARENA_API_KEY'] == inherited['ARENA_API_KEY']
                    configure_live(root, env, 'Pi')
                    assert all('test-only-secret' not in p.read_text() for p in root.rglob('*') if p.is_file())
                    raise RuntimeError('intentional workflow failure')
            except RuntimeError as error:
                assert str(error) == 'intentional workflow failure'
            assert not root.exists()
            assert (native / 'checks').read_text().splitlines() == ['check', 'check']
            (shims / 'codex').write_text('#!/bin/sh\nexit 1\n')
            try:
                preflight(fixture, inherited, 'Codex', 'post-run sign-in check')
            except AssertionError as error:
                assert 'post-run sign-in check failed' in str(error)
            else:
                raise AssertionError('a failed post-run sign-in check was ignored')
    print('PASS: live auth isolation and failure cleanup regression', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend', choices=['Claude', 'Codex', 'Cursor', 'Agy', 'OpenCode', 'Pi', 'Stub', 'Jev'])
    parser.add_argument('--live', action='store_true')
    args = parser.parse_args()
    if not args.live:
        verify_live_isolation()
    backends = [args.backend] if args.backend else (['Claude', 'Codex', 'Cursor', 'Agy', 'OpenCode', 'Pi'] if args.live else ['Claude', 'Cursor', 'Agy', 'Codex', 'Pi', 'OpenCode', 'Stub', 'Jev'])
    failed = []
    for backend in backends:
        if args.live and backend == 'Claude':
            with tempfile.TemporaryDirectory(prefix='omar-preflight-', dir='/tmp') as directory:
                root = Path(directory)
                try:
                    preflight(root, live_environment(root), backend)
                except AssertionError as error:
                    print(f'SKIP: Claude live: {error}', flush=True)
                    continue
        if backend == 'Jev':
            execute_jev(2)
            execute_jev(3)
            continue
        command = {'Claude': 'claude', 'Codex': 'codex', 'Cursor': 'cursor-agent', 'Agy': 'agy',
                   'OpenCode': 'opencode', 'Pi': 'pi', 'Stub': None}[backend]
        if args.live and command and not shutil.which(command):
            print(f'SKIP: {backend}: {command} missing')
            continue
        try:
            execute(backend, live=args.live)
            if not args.live:
                execute(backend, deaths=3)
                if backend in ['Codex', 'Pi', 'OpenCode']:
                    execute(backend, per=True)
        except Exception as error:
            if not args.live:
                raise
            detail = str(error)
            for name, value in os.environ.items():
                if value and any(word in name.upper() for word in ['KEY', 'TOKEN', 'SECRET', 'PASSWORD']):
                    detail = detail.replace(value, '<redacted>')
            print(f'FAIL: {backend}: {detail}', flush=True)
            failed.append(backend)
    if failed:
        raise SystemExit('Failed: ' + ', '.join(failed))


if __name__ == '__main__':
    main()
