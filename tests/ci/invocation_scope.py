#!/usr/bin/env python3
"""An invocation-scoped agent gets a new worker and conversation every time.

`coder : Claude(scope = invocation)` is invoked twice, beside an ordinary
`keeper : Claude` invoked twice. A fake `claude` on PATH stands in for the
CLI the way an operator's own is found: it paints a banner, registers a peer
socket under the `--session-id` it was given, logs everything it is handed,
and answers through the real `omar mcp-server` named in its `--mcp-config`.
No model, no credentials.

Checked, from what the fake logged and the run recorded:
- the two coder invocations ran in two processes, each opening the new
  conversation its attempt named; the keeper's two ran in one;
- a sentinel in the first coder prompt is nowhere in what the second was
  handed: not its launch line, its system prompt, nor its message;
- a file the first coder wrote is what the second read;
- the first coder's process was gone before the second started, and every
  attempt is recorded as stopped;
- a worker that opens some other conversation fails the run, and is never
  handed the invocation;
- the same for fake `pi` (an in-memory conversation by `--no-session
  --session-id`, reported over OMAR's extension socket) and fake `opencode`
  (a session created by id over `/api/session`, unknown before, empty after),
  and each fails closed: a pi that opens another conversation or ignores the
  id; an opencode whose `/api/session` hands back an older session, one that
  already holds messages, or is missing, with no fallback to `/session`;
- the model-free `Stub(scope = invocation)` does the same for real, with no
  fake and no credentials: two attempts, each reporting the new conversation
  its attempt named, the first stopped before the second started;
- a backend that cannot promise a fresh conversation, or that omarc does not
  know, is refused by omarc itself, which writes no bytecode or plan.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
OMARC = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()

# What every fake backend shares: its log, and answering an invocation through
# the real `omar mcp-server` its launch names (`server`, set by each fake's
# startup below, as are `topology` and `session`).
COMMON = r'''
import json, os, re, socket, subprocess, sys, threading, uuid
from pathlib import Path

log = Path(LOG)
argv = sys.argv[1:]
def flag(name):
    return argv[argv.index(name) + 1] if name in argv else None
def write(entry):
    entry["pid"] = os.getpid()
    with log.open("a") as out:
        out.write(json.dumps(entry) + "\n")

def call(tool, arguments):
    # opencode lists OMAR's tools by their bare names.
    if "--bare-tool-names" in server:
        tool = tool.removeprefix("omar_")
    process = subprocess.Popen(server, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    for request in [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": tool, "arguments": arguments}},
    ]:
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()
        reply = json.loads(process.stdout.readline())
    process.stdin.close()
    process.wait()
    return reply

def alive(pid):
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    return bool(state.stdout.strip()) and not state.stdout.strip().startswith("Z")

history = []
def answer(text):
    task = int(re.search(r"task_id: (\d+)", text).group(1))
    write({"event": "message", "agent": topology["agent"], "session_id": session,
           "history_before": len(history), "text": text})
    history.append(text)
    if "Step one" in text:
        Path("note.txt").write_text("note from step one")
        writes = {"mid": "ok"}
    elif "Step two" in text:
        earlier = [json.loads(line) for line in log.read_text().splitlines()]
        first = next(e["pid"] for e in earlier
                     if e["event"] == "message" and "Step one" in e["text"])
        writes = {"result": Path("note.txt").read_text()}
        write({"event": "observed", "first_coder_alive": alive(first)})
    elif "Keeper one" in text:
        writes = {"kmid": "k"}
    else:
        writes = {"kept": f"history={len(history) - 1}"}
    for port, value in writes.items():
        write({"event": "reply", "reply": call("omar_set_port",
               {"task_id": task, "port": port, "value": value})})
    write({"event": "reply", "reply": call("omar_complete", {"task_id": task})})
'''

FAKE = COMMON + r'''
mcp = json.loads(Path(flag("--mcp-config")).read_text())["mcpServers"]["omar"]
server = [mcp["command"], *mcp["args"]]
context = json.loads(Path(mcp["args"][mcp["args"].index("--context-file") + 1]).read_text())
topology = context["topology"]
session = flag("--session-id") or "chosen-by-claude"
if os.environ.get("FAKE_WRONG_SESSION") and flag("--session-id"):
    session = "an-old-conversation"
write({"event": "launch", "agent": topology["agent"], "attempt": topology.get("attempt"),
       "session_id": flag("--session-id"), "argv": " ".join(argv),
       "system_prompt": flag("--append-system-prompt")})
print("\x1b[2J\x1b[H Claude Code v0\n" + "─" * 40 + "\n❯ \n" + "─" * 40, flush=True)

home = Path(os.environ["HOME"])
sessions = home / ".claude" / "sessions"
sessions.mkdir(parents=True, exist_ok=True)
peer = socket.socket(socket.AF_UNIX)
peer.bind(str(home / f"peer-{os.getpid()}.sock"))
peer.listen()
(sessions / f"{os.getpid()}.json").write_text(json.dumps(
    {"sessionId": session, "messagingSocketPath": peer.getsockname()}))
(sessions / f"{os.getpid()}.0.key").write_text(json.dumps({"peerToken": "token"}))

def serve(conn):
    for frame in map(json.loads, conn.makefile().read().splitlines()):
        if frame.get("type") == "user":
            answer(frame["message"]["content"])

while True:
    threading.Thread(target=serve, args=(peer.accept()[0],), daemon=True).start()
'''

# pi, as OMAR's extension makes it look: a socket at OMAR_PI_SOCKET that
# reports the conversation and takes each message. The conversation is the
# `--session-id` it was given under `--no-session`, as pi >= 0.80.3 opens one.
FAKE_PI = COMMON + r'''
context = json.loads(Path(os.environ["OMAR_MCP_CONTEXT_FILE"]).read_text())
server = [os.environ["OMAR_BINARY"], "mcp-server", "--context-file",
          os.environ["OMAR_MCP_CONTEXT_FILE"]]
topology = context["topology"]
session = flag("--session-id") if "--no-session" in argv else None
if os.environ.get("FAKE_WRONG_SESSION") and flag("--session-id"):
    session = "an-old-conversation"
if os.environ.get("FAKE_IGNORE_SESSION_ID") or session is None:
    # An older pi, which opens a conversation of its own choosing.
    session = str(uuid.uuid4())
write({"event": "launch", "agent": topology["agent"], "attempt": topology.get("attempt"),
       "session_id": flag("--session-id"), "argv": " ".join(argv),
       "system_prompt": flag("--system-prompt")})

channel = socket.socket(socket.AF_UNIX)
channel.bind(os.environ["OMAR_PI_SOCKET"])
channel.listen()
while True:
    conn = channel.accept()[0]
    message = json.loads(conn.makefile().readline())
    if message.get("session") is True:
        conn.sendall((json.dumps({"session": None, "id": session}) + "\n").encode())
        conn.close()
    else:
        conn.sendall(b'{"accepted":true}\n')
        conn.close()
        threading.Thread(target=answer, args=(message["text"],), daemon=True).start()
'''

# opencode's loopback server, as much of it as OMAR uses. Sessions live in a
# store shared by every fake process, as opencode's global database is, and
# `/api/session` hands back an existing session rather than refuse, as the
# real one does.
FAKE_OPENCODE = COMMON + r'''
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

config = json.loads(os.environ["OPENCODE_CONFIG_CONTENT"])
server = config["mcp"]["omar"]["command"]
context = json.loads(Path(server[server.index("--context-file") + 1]).read_text())
topology = context["topology"]
session = None
store = Path(os.environ["HOME"]) / "opencode-sessions.json"
lock = threading.Lock()
def sessions():
    return json.loads(store.read_text()) if store.exists() else {}
def save(all):
    store.write_text(json.dumps(all))
if os.environ.get("FAKE_IGNORE_ID"):
    with lock:
        save(dict(sessions(), ses_old=["an earlier conversation"]))

write({"event": "launch", "agent": topology["agent"], "attempt": topology.get("attempt"),
       "session_id": os.environ.get("OMAR_OPENCODE_SESSION"), "argv": " ".join(argv)})
print("\x1b[2J\x1b[H opencode\n tab agents   ctrl+p commands", flush=True)

class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)
    def handle_one(self, method):
        global session
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"null")
        write({"event": "request", "agent": topology["agent"], "method": method,
               "path": self.path})
        parts = self.path.strip("/").split("/")
        with lock:
            all = sessions()
            if method == "POST" and self.path == "/api/session":
                if os.environ.get("FAKE_NO_API_SESSION"):
                    return self.reply(404, {"error": "not found"})
                id = "ses_old" if os.environ.get("FAKE_IGNORE_ID") else body["id"]
                all.setdefault(id, ["preseeded"] if os.environ.get("FAKE_PRESEEDED") else [])
                save(all)
                session = id
                return self.reply(200, {"data": {"id": id}})
            if method == "POST" and self.path == "/session":
                id = f"ses_{uuid.uuid4().hex}"
                all[id] = []
                save(all)
                session = id
                return self.reply(200, {"id": id})
            if method == "POST" and self.path == "/tui/select-session":
                return self.reply(200, True)
            if parts[0] == "session" and len(parts) >= 2 and parts[1] in all:
                id = parts[1]
                if method == "GET" and len(parts) == 2:
                    return self.reply(200, {"id": id})
                if method == "GET" and parts[2:] == ["message"]:
                    return self.reply(200, [{"text": text} for text in all[id]])
                if method == "DELETE" and len(parts) == 2:
                    del all[id]
                    save(all)
                    return self.reply(200, True)
                if method == "POST" and parts[2:] == ["prompt_async"]:
                    text = body["parts"][0]["text"]
                    all[id].append(text)
                    save(all)
                    self.reply(204)
                    threading.Thread(target=answer, args=(text,), daemon=True).start()
                    return
        self.reply(404, {"name": "NotFoundError"})
    def do_GET(self):
        self.handle_one("GET")
    def do_POST(self):
        self.handle_one("POST")
    def do_DELETE(self):
        self.handle_one("DELETE")
    def log_message(self, *args):
        pass

ThreadingHTTPServer(("127.0.0.1", int(flag("--port"))), Handler).serve_forever()
'''

FAKES = {'claude': FAKE, 'pi': FAKE_PI, 'opencode': FAKE_OPENCODE}

PROGRAM = '''team Pipe[coder : {coder}, keeper : Claude] {{
    input start : string
    action mid : string
    action kmid : string
    output result : string
    output kept : string

    task coder(start) -> mid "Step one. The secret is SENTINEL-$(start). Leave a note."
    task coder(mid) -> result "Step two. Got $(mid). Read the note."
    task keeper(start) -> kmid "Keeper one $(start)"
    task keeper(kmid) -> kept "Keeper two $(kmid)"
}}

main Scoped {{
    p = Pipe()
}}
'''


# The same pipeline on the real `omar stub-agent`, which answers every string
# with "stub".
STUB_PROGRAM = PROGRAM.replace('keeper : Claude', 'keeper : Stub')

OTHER = object()

# Each faked backend as a program spells it.
SPELLED = {'claude': 'Claude', 'pi': 'Pi', 'opencode': 'OpenCode'}


def supported():
    """The backends omarc names as able to run an invocation scope."""
    table = (REPO / 'lang/Omar/Backends.lean').read_text()
    return ', '.join(re.findall(r'#\["([^"]+)"', table))


def run(root, coder='Claude(scope = invocation)', env=None, program_text=PROGRAM,
        backend='claude'):
    home = root / 'home'
    shims = home / 'shims'
    shims.mkdir(parents=True, exist_ok=True)
    log = root / f'{backend}.jsonl'
    fake = root / f'fake_{backend}.py'
    fake.write_text(f'LOG = {str(log)!r}\n' + FAKES[backend])
    shim = shims / backend
    shim.write_text(f"#!/bin/sh\nexec python3 '{fake}' \"$@\"\n")
    shim.chmod(0o700)
    (home / '.profile').write_text(f'export PATH="{shims}:$PATH"\n')
    program = root / 'Scoped.omar'
    program.write_text(program_text.format(coder=coder))
    server = 'omar-scope-' + root.name.rsplit('-', 1)[-1]
    # Nothing of an enclosing OMAR session or tmux client may leak in.
    inherited = {k: v for k, v in os.environ.items()
                 if not k.startswith('OMAR_') and k not in ('TMUX', 'TMUX_PANE')}
    environment = dict(inherited, HOME=str(home), OMAR_TMUX_SERVER=server,
                       OMARC_BIN=str(OMARC), **(env or {}))
    # A session of its own, so the run's records outlive the run: one that
    # `omar run` makes for itself is removed with it.
    session = ['-s', 'scope-test']
    try:
        up = subprocess.run([str(BIN), 'up', *session, '--no-ea'], env=environment,
                            capture_output=True, text=True, timeout=120)
        assert up.returncode == 0, up.stdout + up.stderr
        result = subprocess.run(
            [str(BIN), 'run', *session, str(program), '--input', 'p.start=alpha',
             '--timeout-seconds', '60', '--replace', '--fg'],
            env=environment, capture_output=True, text=True, timeout=300)
        attempts = [json.loads(line) for path in home.rglob('attempts.jsonl')
                    for line in path.read_text().splitlines()]
    finally:
        subprocess.run([str(BIN), 'down', *session], env=environment, capture_output=True,
                       timeout=60)
        subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)
    events = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    return result, events, attempts


def on(backend):
    """`run`'s arguments for the pipeline with both agents on `backend`."""
    spelled = SPELLED[backend]
    return {'coder': f'{spelled}(scope = invocation)', 'backend': backend,
            'program_text': PROGRAM.replace('keeper : Claude', f'keeper : {spelled}')}


def fresh_worker_per_invocation(backend='claude'):
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        result, events, attempts = run(Path(directory), **on(backend))
        output = result.stdout + result.stderr
        assert result.returncode == 0, output
        assert 'Output p.result = "note from step one"' in output, output
        launches = [e for e in events if e['event'] == 'launch']
        messages = [e for e in events if e['event'] == 'message']
        coder = [e for e in launches if e['agent'] == 'p.coder']
        keeper = [e for e in launches if e['agent'] == 'p.keeper']

        # Two invocations, two workers, two conversations, each the one its
        # attempt named; the agent-scoped keeper kept one of each.
        assert len(coder) == 2 and len(keeper) == 1, launches
        assert coder[0]['pid'] != coder[1]['pid'], coder
        assert coder[0]['session_id'] != coder[1]['session_id'], coder
        for launch in coder:
            assert launch['session_id'] == launch['attempt']['native_session'], launch
        assert keeper[0]['session_id'] is None and keeper[0]['attempt'] is None, keeper
        coder_messages = [m for m in messages if m['agent'] == 'p.coder']
        keeper_messages = [m for m in messages if m['agent'] == 'p.keeper']
        assert [m['history_before'] for m in coder_messages] == [0, 0], coder_messages
        assert [m['history_before'] for m in keeper_messages] == [0, 1], keeper_messages
        assert len({m['pid'] for m in keeper_messages}) == 1, keeper_messages
        assert 'Output p.kept = "history=1"' in output, output

        # The sentinel reached the first coder and nothing the second was given.
        assert 'SENTINEL-alpha' in coder_messages[0]['text']
        second = json.dumps([coder[1], coder_messages[1]])
        assert 'SENTINEL' not in second, second

        # Downstream ran only once the first worker was gone, and each attempt
        # says so.
        observed = [e for e in events if e['event'] == 'observed']
        assert observed == [{'event': 'observed', 'first_coder_alive': False,
                             'pid': coder[1]['pid']}], observed
        finished = [a for a in attempts if a['outcome'] != 'started']
        assert [a['outcome'] for a in finished] == ['completed', 'completed'], attempts
        assert {a['cleanup_status'] for a in finished} == {'stopped'}, attempts
        assert [a['native_session_id'] for a in finished] == [c['session_id'] for c in coder]
        assert all(a['logical_agent'] == 'p.coder' and a['scope'] == 'invocation'
                   and a['verification'] == 'chosen' for a in attempts), attempts
        replies = [e['reply'] for e in events if e['event'] == 'reply']
        assert all(not r.get('result', {}).get('isError') for r in replies), replies
        # What the worker reported is what it was handed the invocation in.
        assert [m['session_id'] for m in coder_messages] == [c['session_id'] for c in coder]
        if backend == 'pi':
            # In memory: `--no-session` even where workers keep theirs.
            for launch in coder:
                assert f"--no-session --session-id {launch['session_id']}" in launch['argv'], launch
        if backend == 'opencode':
            assert all(c['session_id'].startswith('ses_') for c in coder), coder
            requests = [(e['pid'], e['method'], e['path']) for e in events
                        if e['event'] == 'request' and e['agent'] == 'p.coder']
            for launch in coder:
                mine = [(m, path) for pid, m, path in requests if pid == launch['pid']]
                session = launch['session_id']
                # Unknown before it was created by id, empty after; never one
                # opencode named.
                assert mine.index(('GET', f'/session/{session}')) < \
                    mine.index(('POST', '/api/session')) < \
                    mine.index(('GET', f'/session/{session}/message')), mine
                assert ('POST', '/session') not in mine, mine
        print(f'PASS: {SPELLED[backend]}: two coder invocations ran in two fresh conversations, '
              'keeper reused one; sentinel absent, file visible, worker 1 stopped first')


def a_reused_conversation_fails_closed(backend='claude', fault='FAKE_WRONG_SESSION',
                                       says='an-old-conversation', recorded='an-old-conversation'):
    """`says` is in the error; `recorded` is the conversation the attempt
    records the worker reporting: None for none, OTHER for one not its own."""
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        result, events, attempts = run(Path(directory), env={fault: '1'}, **on(backend))
        output = result.stdout + result.stderr
        assert result.returncode != 0, output
        assert 'did not start a fresh conversation' in output, output
        assert says in output, output
        coder_messages = [e for e in events if e['event'] == 'message' and e['agent'] == 'p.coder']
        assert coder_messages == [], coder_messages
        launches = [e for e in events if e['event'] == 'launch' and e['agent'] == 'p.coder']
        assert len(launches) == 1, launches
        finished = [a for a in attempts if a['outcome'] != 'started']
        assert [(a['outcome'], a['cleanup_status']) for a in finished] == [('failed', 'stopped')], attempts
        # What it actually opened is on record, refused or not.
        if recorded is OTHER:
            assert finished[0]['native_session_id'] not in (
                None, launches[0]['attempt']['native_session']), attempts
        else:
            assert finished[0]['native_session_id'] == recorded, attempts
        if backend == 'opencode':
            # Refused at creation, with no fallback to a session opencode names.
            requests = [e for e in events if e['event'] == 'request' and e['agent'] == 'p.coder']
            assert not any(e['method'] == 'POST' and e['path'] == '/session'
                           for e in requests), requests
        print(f'PASS: {SPELLED[backend]} ({fault}): a worker that could not show a new '
              'conversation failed the run and was never handed the invocation')


def a_stub_worker_per_invocation():
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        result, events, attempts = run(Path(directory), coder='Stub(scope = invocation)',
                                       program_text=STUB_PROGRAM)
        output = result.stdout + result.stderr
        assert result.returncode == 0, output
        assert 'Output p.result = "stub"' in output, output
        assert 'Output p.kept = "stub"' in output, output
        assert events == [], events
        assert all(a['logical_agent'] == 'p.coder' and a['backend'] == 'Stub'
                   and a['scope'] == 'invocation' and a['verification'] == 'chosen'
                   for a in attempts), attempts

        # Two invocations, two attempts, two conversations: each the id its
        # attempt named, as the stub worker itself reported it.
        order = list(dict.fromkeys(a['attempt_id'] for a in attempts))
        assert len(order) == 2, attempts
        finished = [a for a in attempts if a['outcome'] != 'started']
        assert [a['attempt_id'] for a in finished] == order, attempts
        assert [(a['outcome'], a['cleanup_status']) for a in finished] == \
            [('completed', 'stopped')] * 2, attempts
        ids = [a['native_session_id'] for a in finished]
        assert all(ids) and ids[0] != ids[1], ids
        assert len({a['invocation_id'] for a in finished}) == 2, attempts

        # The first worker was seen gone before the second was started.
        first_stopped = attempts.index(finished[0])
        second_started = next(i for i, a in enumerate(attempts) if a['attempt_id'] == order[1])
        assert first_stopped < second_started, attempts
        print('PASS: Stub(scope = invocation) ran two attempts in two conversations '
              f'({ids[0]}, {ids[1]}); worker 1 stopped before worker 2 started')


def an_unsupported_backend_is_refused():
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        root = Path(directory)
        # omarc is the layer that refuses: nothing is written for the runtime
        # to refuse again, whether a program or a mutation plan.
        old = root / 'Old.omar'
        old.write_text(PROGRAM.format(coder='Codex'))
        for backend in ['Codex', 'Web', 'Nope']:
            program = root / 'Scoped.omar'
            program.write_text(PROGRAM.format(coder=f'{backend}(scope = invocation)'))
            for args in [[program], ['--diff', old, program]]:
                out = root / 'out.json'
                r = subprocess.run([str(OMARC), *map(str, args), str(out)],
                                   capture_output=True, text=True)
                assert r.returncode == 1 and not out.exists(), (backend, args, r)
                assert (f"backend '{backend}' cannot start a fresh conversation per "
                        f'invocation; supported: {supported()}') in r.stderr, r.stderr
        result, events, _ = run(root, coder='Web(scope = invocation)')
        output = result.stdout + result.stderr
        assert result.returncode != 0, output
        assert 'cannot start a fresh conversation per invocation' in output, output
        assert events == [], events
        print('PASS: Codex, Web and unknown backends refused by omarc, no bytecode written')


if __name__ == '__main__':
    fresh_worker_per_invocation()
    a_reused_conversation_fails_closed()
    fresh_worker_per_invocation('pi')
    a_reused_conversation_fails_closed('pi')
    # pi before 0.80.3 opens a conversation of its own under --no-session.
    a_reused_conversation_fails_closed('pi', 'FAKE_IGNORE_SESSION_ID',
                                       says='not the new conversation', recorded=OTHER)
    fresh_worker_per_invocation('opencode')
    # /api/session answers with an older session than the one asked for; with
    # the one asked for, already holding messages; or not at all.
    # Refused before the worker reports anything, so nothing is recorded.
    a_reused_conversation_fails_closed('opencode', 'FAKE_IGNORE_ID', says='ses_old', recorded=None)
    a_reused_conversation_fails_closed('opencode', 'FAKE_PRESEEDED', says='already holds',
                                       recorded=None)
    a_reused_conversation_fails_closed('opencode', 'FAKE_NO_API_SESSION', says='/api/session',
                                       recorded=None)
    a_stub_worker_per_invocation()
    an_unsupported_backend_is_refused()
