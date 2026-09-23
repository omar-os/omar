#!/usr/bin/env python3
"""A topology with a Cursor or Antigravity worker runs to completion.

Those backends launch under OMAR's protocol runner, so `omar run` must not
wait for a TUI banner that never paints. Deterministic native-protocol peers
stand in for the CLIs, found through a private HOME whose profile puts them
on PATH the way an operator's would. No model, no credentials, no terminal
input: the proof is the invocation reaching the peer over the runner.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from protocol_runner import PEER  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
OMARC = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()

PROGRAM = '''team Ping[worker : {backend}] {{
    input request : string
    output result : string

    prompt worker(request) -> result? within(15s)
    "Set `result` to pong. Request: $(request)"
}}

main Runner{backend} {{
    ping = Ping()
}}
'''


def main():
    for backend, executable in [('Cursor', 'cursor'), ('Antigravity', 'agy')]:
        with tempfile.TemporaryDirectory(prefix='omar-topo-') as directory:
            root = Path(directory)
            home = root / 'home'
            shims = home / 'shims'
            shims.mkdir(parents=True)
            peer = root / 'peer.py'
            peer.write_text(PEER)
            shim = shims / executable
            shim.write_text(f"#!/bin/sh\nexec python3 '{peer}' '{root}' {executable} \"$@\"\n")
            shim.chmod(0o700)
            # `sh -lc` reads this in the pane, so the peer is found the way an
            # operator's own CLI is: from PATH, not from a config override.
            (home / '.profile').write_text(f'export PATH="{shims}:$PATH"\n')
            program = root / f'Runner{backend}.omar'
            program.write_text(PROGRAM.format(backend=backend))
            server = 'omar-topo-' + root.name.rsplit('-', 1)[-1]
            env = dict(os.environ, HOME=str(home), OMAR_TMUX_SERVER=server, OMARC_BIN=str(OMARC))
            started = time.monotonic()
            try:
                run = subprocess.run(
                    [str(BIN), 'run', str(program), '--input', 'ping.request=smoke', '--replace'],
                    env=env, capture_output=True, text=True, timeout=180)
            finally:
                subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)
            elapsed = time.monotonic() - started
            assert run.returncode == 0, f'{backend}: omar run failed\n{run.stdout}\n{run.stderr}'
            assert 'completed' in run.stdout, f'{backend}: {run.stdout}'
            requests = root / 'requests.jsonl'
            assert requests.exists(), f'{backend}: the runner never started the peer'
            seen = [json.loads(line) for line in requests.read_text().splitlines()]
            prompts = [r for r in seen if r.get('method') == 'session/prompt' or r.get('event') == 'user']
            assert prompts, f'{backend}: no invocation reached the peer: {seen}'
            assert 'OMAR INVOCATION' in json.dumps(prompts[0]), f'{backend}: {prompts[0]}'
            # The gate this guards against waited 60s for a banner and failed.
            assert elapsed < 60, f'{backend}: run took {elapsed:.0f}s; something waited on a banner'
            print(f'PASS: {backend} worker launched under the runner, invocation delivered, run completed in {elapsed:.0f}s')


if __name__ == '__main__':
    main()
