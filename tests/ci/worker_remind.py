#!/usr/bin/env python3
"""Exercise native turn boundaries, reminders, and the shared retry budget."""
import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path

from worker_restart import REPO, execute, live_environment, preflight


def verify_opencode_event_order():
    with tempfile.TemporaryDirectory(prefix='omar-turn-events-', dir='/tmp') as directory:
        root = Path(directory)
        log = root / 'events'
        peer = root / 'omar'
        peer.write_text('#!/usr/bin/env python3\nimport sys,time\n'
                        'from pathlib import Path\nevent=sys.argv[-1]\n'
                        'if event.startswith("Busy"): time.sleep(.3)\n'
                        f'with Path({str(log)!r}).open("a") as out: out.write(event+"\\n")\n'
                        'print("{}")\n')
        peer.chmod(0o700)
        plugin = root / 'coordination.mjs'
        plugin.write_text(f'const exe={json.dumps(str(peer))}; const contextFile="unused"; const topology=true;\n'
                          + (REPO / 'src/backend/opencode/plugin.mjs').read_text())
        script = '''const { OmarCoordination } = await import(process.argv[1]);
const plugin = await OmarCoordination();
await Promise.all([
  plugin.event({event:{type:"session.status",properties:{sessionID:"ses_a",status:{type:"busy"}}}}),
  plugin.event({event:{type:"session.idle",properties:{sessionID:"ses_a"}}}),
]);'''
        subprocess.run(['node', '--input-type=module', '-e', script, plugin.as_uri()], check=True, timeout=10)
        assert log.read_text().splitlines() == ['Busy:ses_a', 'Stop:ses_a']
    print('PASS: OpenCode turn hooks keep native event order', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--questions', action='store_true', help='Run blocking user-input recovery scenarios')
    parser.add_argument('--async-questions', action='store_true', help='Run Codex 0.167 async questions through its real TUI and serve')
    parser.add_argument('--backend', choices=['Claude', 'Cursor', 'Agy', 'Codex', 'Pi', 'OpenCode', 'Stub'])
    args = parser.parse_args()
    if args.async_questions:
        assert not args.live, 'the deterministic async scenario uses a local model catalog'
        execute('Codex', deaths=0, scenario='question_async')
        execute('Codex', per=True, deaths=0, scenario='question_async_ignore')
        execute('Codex', deaths=0, scenario='question_async_ignore')
        return
    if not args.live:
        verify_opencode_event_order()
    backends = [args.backend] if args.backend else ['Claude', 'Cursor', 'Agy', 'Codex', 'Pi', 'OpenCode', 'Stub']
    failures = []
    for backend in backends:
        if args.live and backend == 'Stub':
            continue
        if args.live and backend == 'Claude':
            with tempfile.TemporaryDirectory(prefix='omar-remind-preflight-', dir='/tmp') as directory:
                root = Path(directory)
                try:
                    preflight(root, live_environment(root), backend)
                except AssertionError as error:
                    print(f'SKIP: Claude live: {error}', flush=True)
                    continue
        try:
            if args.questions:
                if backend == 'Stub': continue
                execute(backend, deaths=0, live=args.live, scenario='question')
                if not args.live:
                    execute(backend, per=backend == 'Codex', deaths=0, scenario='question_ignore')
                    if backend in ['Claude', 'Cursor', 'Agy', 'OpenCode']: execute(backend, deaths=0, scenario='question_permission')
                    if backend in ['Cursor', 'Agy']: execute(backend, deaths=0, scenario='permission')
                    if backend == 'Cursor':
                        execute(backend, deaths=0, scenario='question_elicitation')
                        execute(backend, deaths=0, scenario='question_fallback')
                continue
            execute(backend, deaths=0, live=args.live, scenario='complete')
            if not args.live:
                execute(backend, deaths=0, scenario='ignore')
                execute(backend, deaths=0, scenario='working')
                if backend in ['Claude', 'Codex', 'Pi', 'OpenCode', 'Stub']:
                    execute(backend, per=True, deaths=0, scenario='complete')
                if backend != 'Stub':
                    execute(backend, deaths=0, scenario='question')
                    execute(backend, per=backend == 'Codex', deaths=0, scenario='question_ignore')
                    if backend in ['Claude', 'Cursor', 'Agy', 'OpenCode']: execute(backend, deaths=0, scenario='question_permission')
                    if backend in ['Cursor', 'Agy']: execute(backend, deaths=0, scenario='permission')
                    if backend == 'Cursor':
                        execute(backend, deaths=0, scenario='question_elicitation')
                        execute(backend, deaths=0, scenario='question_fallback')
        except Exception as error:
            if not args.live:
                raise
            detail = str(error)
            for name, value in os.environ.items():
                if value and any(word in name.upper() for word in ['KEY', 'TOKEN', 'SECRET', 'PASSWORD']):
                    detail = detail.replace(value, '<redacted>')
            print(f'FAIL: {backend} live: {detail}', flush=True)
            failures.append(backend)
    if failures:
        raise SystemExit('Failed: ' + ', '.join(failures))


if __name__ == '__main__':
    main()
