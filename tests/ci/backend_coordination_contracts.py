#!/usr/bin/env python3
"""Exercise every production backend's context wire format and OpenCode plugin.
No models or credentials: invokes the built OMAR and real Node plugin loader.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()


def main():
    with tempfile.TemporaryDirectory(prefix='omar-backend-contract-') as directory:
        root = Path(directory)
        context = root / 'context.json'
        context.write_text(json.dumps(dict(omar_dir=str(root), ea_id=0, agent_name='worker',
            session_prefix='test-', default_command='cat', default_workdir=str(root), health_idle_warning=15)))
        task = dict(id='durable-proof', ea_id=0, agent='worker', session='test-worker', parent='ea',
            project_id=1, assignment='restore fresh state', status='running', result=None,
            acknowledged=False, retired=False, next_check_ms=9999999999999)
        ledger = root / 'task-lifecycle.json'
        ledger.write_text(json.dumps({'tasks': {task['id']: task}}))

        def hook(backend, event, **extra):
            output = subprocess.run([str(BIN), 'agent-hook', '--context-file', str(context), '--format', backend],
                input=json.dumps(dict(hook_event_name=event, **extra)), capture_output=True, text=True, check=True)
            return json.loads(output.stdout)

        for backend, event, path in [
            ('claude', 'SessionStart', ('hookSpecificOutput', 'additionalContext')),
            ('codex', 'SessionStart', ('hookSpecificOutput', 'additionalContext')),
            ('cursor', 'sessionStart', ('additional_context',)),
            ('agy', 'PreInvocation', ('injectSteps', 0, 'ephemeralMessage')),
            ('opencode', 'PreInvocation', ('context',)),
        ]:
            response = hook(backend, event, source='compact')
            for key in path:
                response = response[key]
            assert 'durable-proof' in response, backend
            assert 'restore fresh state' in response, backend
            print(f'PASS: {backend} restores ownership through its supported context contract', flush=True)
        for backend in ['claude', 'codex']:
            assert hook(backend, 'Stop')['decision'] == 'block'
            assert hook(backend, 'Stop', stop_hook_active=True) == {}
        assert 'followup_message' in hook('cursor', 'stop', status='completed', loop_count=0)
        assert hook('cursor', 'stop', status='aborted', loop_count=0) == {}
        assert hook('cursor', 'stop', status='completed', loop_count=1) == {}
        # An old installed hook may still invoke beforeSubmitPrompt. Never
        # discard queued context on an event whose output cannot inject it.
        spool = root / 'spool.jsonl'
        spool.write_text(json.dumps({'at': 1, 'text': 'queued event'}) + '\n')
        env = dict(os.environ, OMAR_MCP_CONTEXT_FILE=str(context), OMAR_EVENT_SPOOL=str(spool))
        subprocess.run([str(BIN), 'hook-drain', '--format', 'cursor'],
            input=json.dumps({'hook_event_name': 'beforeSubmitPrompt'}), capture_output=True, text=True, env=env, check=True)
        assert 'queued event' in spool.read_text()
        response = subprocess.run([str(BIN), 'hook-drain', '--format', 'agy'],
            input='{}', capture_output=True, text=True, env=env, check=True)
        assert 'durable-proof' in response.stdout and 'queued event' in response.stdout
        response = subprocess.run([str(BIN), 'hook-drain', '--format', 'agy'],
            input='{}', capture_output=True, text=True, env=env, check=True)
        assert 'durable-proof' in response.stdout and 'queued event' not in response.stdout
        print('PASS: Cursor continuation is bounded; Antigravity reads live state even with an empty spool', flush=True)

        plugin = root / 'plugin.mjs'
        plugin.write_text(f'const exe={json.dumps(str(BIN))};\nconst contextFile={json.dumps(str(context))};\n' +
                          (REPO / 'src/backend_hooks/opencode.mjs').read_text())
        harness = root / 'check.mjs'
        harness.write_text('''
import { OmarCoordination } from './plugin.mjs';
import { readFileSync, writeFileSync } from 'node:fs';
const plugin = await OmarCoordination();
const first = { system: ['default coding policy'] };
await plugin['experimental.chat.system.transform']({}, first);
if (first.system[0] !== 'default coding policy' || !first.system[1].includes('durable-proof')) throw Error('policy lost');
const state = JSON.parse(readFileSync('./task-lifecycle.json'));
state.tasks['durable-proof'].assignment = 'fresh after compaction';
writeFileSync('./task-lifecycle.json', JSON.stringify(state));
const compact = { context: ['default summary policy'] };
await plugin['experimental.session.compacting']({}, compact);
if (compact.context[0] !== 'default summary policy' || !compact.context[1].includes('fresh after compaction')) throw Error('stale compaction');
await plugin.event({ event: { type: 'session.idle' } });
if (JSON.parse(readFileSync('./task-lifecycle.json')).tasks['durable-proof'].next_check_ms !== 0) throw Error('idle did not arm runtime');
''')
        subprocess.run(['node', str(harness)], cwd=root, check=True)
        print('PASS: actual OpenCode plugin refreshes each request/compaction, preserves base policy, and arms idle recovery', flush=True)


if __name__ == '__main__':
    main()
