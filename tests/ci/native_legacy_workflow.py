#!/usr/bin/env python3
"""Opt-in native Cursor/Antigravity worker -> scripted OpenCode-contract parent workflow.
Uses live backend credentials and real OMAR MCP/spawn/scheduler/ack/retirement.
The parent is a deterministic driver, not a model-management benchmark.
"""
import http.server
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import threading
import time
import uuid
from legacy_supervision import MCP, Backend, receipts

def main():
    if os.environ.get('OMAR_LIVE_BACKENDS') != '1':
        raise SystemExit('Set OMAR_LIVE_BACKENDS=1 for authenticated model calls')
    native = sys.argv[1] if len(sys.argv) > 1 else 'cursor'
    assert native in ['cursor', 'agy']
    server = 'omar-live-workflow-' + uuid.uuid4().hex[:10]
    backend = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Backend)
    threading.Thread(target=backend.serve_forever, daemon=True).start()
    env = dict(os.environ, OMAR_TMUX_SERVER=server)
    for key in ['OMAR_AGENT_NAME', 'OMAR_EA_ID', 'OMAR_MCP_CONTEXT_FILE', 'TMUX']:
        env.pop(key, None)
    parent = None
    with tempfile.TemporaryDirectory(prefix='omar-live-workflow-', dir='/tmp') as directory:
        root = Path(directory)
        if native == 'agy':
            # Keep plugin registration and native conversations out of the real
            # home. Copy only cached authentication into this private test home.
            test_home = root / 'home'
            for relative in ['.gemini/antigravity-cli/antigravity-oauth-token',
                             '.gemini/oauth_creds.json', '.gemini/google_accounts.json']:
                source = Path.home() / relative
                if source.is_file():
                    target = test_home / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)
                    target.chmod(0o600)
            env['HOME'] = str(test_home)
        context = root / 'parent.json'
        context.write_text(json.dumps(dict(omar_dir=str(root), ea_id=0, session_prefix='reg-',
            default_command='cat', default_workdir=str(root), health_idle_warning=15, tmux_server=server)))
        def tmux(*args):
            return subprocess.run(['tmux', '-L', server, *args], env=env, check=True, text=True, capture_output=True).stdout
        try:
            tmux('new-session', '-d', '-s', 'reg-ea-0', 'cat')
            tmux('set-environment', '-t', 'reg-ea-0', 'OMAR_BACKEND', 'opencode')
            tmux('set-environment', '-t', 'reg-ea-0', 'OMAR_DELIVERY', f'opencode:{backend.server_port}:ses_parent')
            parent = MCP(context, env)
            project = parent.call('add_project', name='native tracked completion')['project_id']
            task = parent.call('spawn_agent', name='worker', project_id=project,
                backend=native,
                task='Bounded integration test. Compute 19*23 mentally. Do not use shell or file tools and do not spawn agents. Call the OMAR finish_task tool for your assigned task_id with status completed and result {"answer":437,"validation":"mental multiplication"}. Discover OMAR tools if needed. A final chat reply is insufficient.')
            task_id = task['task_id']
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                ledger = json.loads((root / 'task-lifecycle.json').read_text())['tasks'][task_id]
                if ledger['status'] == 'completed': break
                time.sleep(.2)
            else:
                raise AssertionError('native worker did not complete via MCP: ' + tmux('capture-pane', '-p', '-t', 'reg-0-worker'))
            assert ledger['result']['answer'] == 437, ledger
            endpoint, body = receipts.get(timeout=15)
            assert endpoint == '/session/ses_parent/prompt_async'
            assert task_id in body['parts'][0]['text']
            page = parent.call('get_task', task_id=task_id)
            assert json.loads(page['content'])['result']['answer'] == 437
            parent.call('acknowledge_task', task_id=task_id, result_revision=page['result_revision'])
            parent.call('kill_agent', name='worker')
            parent.call('complete_project', project_id=project)
            assert parent.call('coordination_state')['tasks'] == []
            print(f'PASS: live {native} worker completed through OMAR MCP; host woke scripted OpenCode parent; result read, acknowledged, retired and project settled', flush=True)
        finally:
            if parent: parent.stop()
            subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)
            backend.shutdown(); backend.server_close()

if __name__ == '__main__': main()
