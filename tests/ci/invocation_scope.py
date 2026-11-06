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
- a backend that cannot promise a fresh conversation, or that omarc does not
  know, is refused by omarc itself, which writes no bytecode or plan.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
OMARC = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()

FAKE = r'''
import json, os, re, socket, subprocess, sys, threading
from pathlib import Path

log = Path(LOG)
argv = sys.argv[1:]
def flag(name):
    return argv[argv.index(name) + 1] if name in argv else None
def write(entry):
    entry["pid"] = os.getpid()
    with log.open("a") as out:
        out.write(json.dumps(entry) + "\n")

mcp = json.loads(Path(flag("--mcp-config")).read_text())["mcpServers"]["omar"]
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

def call(tool, arguments):
    server = subprocess.Popen([mcp["command"], *mcp["args"]], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, text=True)
    for request in [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": tool, "arguments": arguments}},
    ]:
        server.stdin.write(json.dumps(request) + "\n")
        server.stdin.flush()
        reply = json.loads(server.stdout.readline())
    server.stdin.close()
    server.wait()
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

def serve(conn):
    for frame in map(json.loads, conn.makefile().read().splitlines()):
        if frame.get("type") == "user":
            answer(frame["message"]["content"])

while True:
    threading.Thread(target=serve, args=(peer.accept()[0],), daemon=True).start()
'''

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


def run(root, coder='Claude(scope = invocation)', env=None):
    home = root / 'home'
    shims = home / 'shims'
    shims.mkdir(parents=True, exist_ok=True)
    log = root / 'claude.jsonl'
    fake = root / 'fake_claude.py'
    fake.write_text(f'LOG = {str(log)!r}\n' + FAKE)
    shim = shims / 'claude'
    shim.write_text(f"#!/bin/sh\nexec python3 '{fake}' \"$@\"\n")
    shim.chmod(0o700)
    (home / '.profile').write_text(f'export PATH="{shims}:$PATH"\n')
    program = root / 'Scoped.omar'
    program.write_text(PROGRAM.format(coder=coder))
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


def fresh_worker_per_invocation():
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        result, events, attempts = run(Path(directory))
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
        print('PASS: two coder invocations ran in two fresh conversations, '
              'keeper reused one; sentinel absent, file visible, worker 1 stopped first')


def a_reused_conversation_fails_closed():
    with tempfile.TemporaryDirectory(prefix='omar-scope-') as directory:
        result, events, attempts = run(Path(directory), env={'FAKE_WRONG_SESSION': '1'})
        output = result.stdout + result.stderr
        assert result.returncode != 0, output
        assert 'did not start a fresh conversation' in output, output
        assert 'an-old-conversation' in output, output
        coder_messages = [e for e in events if e['event'] == 'message' and e['agent'] == 'p.coder']
        assert coder_messages == [], coder_messages
        launches = [e for e in events if e['event'] == 'launch' and e['agent'] == 'p.coder']
        assert len(launches) == 1, launches
        finished = [a for a in attempts if a['outcome'] != 'started']
        assert [(a['outcome'], a['cleanup_status']) for a in finished] == [('failed', 'stopped')], attempts
        # What it actually opened is on record, refused or not.
        assert finished[0]['native_session_id'] == 'an-old-conversation', attempts
        print('PASS: a worker that opened another conversation failed the run and was never handed the invocation')


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
                        'invocation; supported: claude') in r.stderr, r.stderr
        result, events, _ = run(root, coder='Web(scope = invocation)')
        output = result.stdout + result.stderr
        assert result.returncode != 0, output
        assert 'cannot start a fresh conversation per invocation' in output, output
        assert events == [], events
        print('PASS: Codex, Web and unknown backends refused by omarc, no bytecode written')


if __name__ == '__main__':
    fresh_worker_per_invocation()
    a_reused_conversation_fails_closed()
    an_unsupported_backend_is_refused()
