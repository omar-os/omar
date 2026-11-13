#!/usr/bin/env python3
"""Model-free restoration through the public CLI and scoped workflow API."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get('OMAR_BIN', REPO / 'target/debug/omar')).resolve()
COMPILER = Path(os.environ.get('OMARC_BIN', REPO / 'lang/.lake/build/bin/omarc')).resolve()

with tempfile.TemporaryDirectory(prefix='omar-workspace-restore-') as directory:
    root = Path(directory)
    env = {k: v for k, v in os.environ.items() if not k.startswith('OMAR_') and k not in ('TMUX', 'TMUX_PANE')}
    env.update(OMAR_HOME=str(root / 'state'), OMARC_BIN=str(COMPILER))
    started = False

    def cli(*args):
        result = subprocess.run([str(BIN), *args], env=env, cwd=root, text=True, capture_output=True, timeout=60)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    def api(url, value=None, expected=200):
        data = None if value is None else json.dumps(value).encode()
        request = urllib.request.Request(url, data=data, headers={'Content-Type': 'application/json'})
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        assert response.status == expected, (url, response.status, response.read())
        return json.load(response)

    try:
        session = json.loads(cli('up', '--name', 'saved', '--no-ea', '--json'))
        started = True
        url = session['url']
        chat_id = api(url + '/v1/chats')['active_id']
        scoped = url + '/chats/' + chat_id
        draft = dict(filename='restored.omar', proposal_sequence=0, design=dict(
            program=(REPO / 'web/tests/fixtures/review-flow.omar').read_text(),
            files={'brief.md': 'Preserve this companion file'}, inputs={'flow.request': 'Preserve this input'},
            preview=json.loads((REPO / 'web/tests/fixtures/diagram-snapshot.v1.json').read_text())))
        saved = api(scoped + '/v1/workflow', dict(revision=0, name='Saved across restart', draft=draft))
        assert saved['revision'] == 1
        api(scoped + '/v1/workflow', dict(revision=0, draft=draft), expected=409)
        assert api(scoped + '/v1/workflow') == saved
        # Another chat must not receive the first chat's draft.
        other = api(url + '/v1/chats', {})
        assert api(url + '/chats/' + other['id'] + '/v1/workflow')['draft'] is None
        api(url + '/v1/chats/' + chat_id + '/activate', {})
        assert json.loads(cli('open', '-s', 'saved', '--print-url'))['url'] == url
        cli('down', '-s', 'saved')
        reopened = json.loads(cli('open', '--print-url'))
        assert reopened == dict(session='saved', url=url)
        assert api(scoped + '/v1/workflow') == saved
        history = api(url + '/v1/chats')
        assert history['active_id'] == chat_id and len(history['conversations']) == 2
        assert all(chat['run'] is None for chat in history['conversations'])
        assert len(json.loads(cli('ls', '--json'))) == 1
        api(url + '/v1/chats/' + chat_id + '/archive', {})
        assert chat_id not in [chat['id'] for chat in api(url + '/v1/chats')['conversations']]
        cli('down', '-s', 'saved')
        assert json.loads(cli('open', '--print-url'))['url'] == url
        assert chat_id not in [chat['id'] for chat in api(url + '/v1/chats')['conversations']]
        assert chat_id in [chat['id'] for chat in api(url + '/v1/chats/archived')['conversations']]
        api(url + '/v1/chats/' + chat_id + '/restore', {})
        assert api(scoped + '/v1/workflow') == saved
        api(url + '/v1/chats/' + chat_id + '/archive', {})
        api(url + '/v1/chats/' + chat_id + '/delete', {}, expected=400)
        api(url + '/v1/chats/' + chat_id + '/delete', {'confirm': True})
        cli('down', '-s', 'saved')
        assert json.loads(cli('open', '--print-url'))['url'] == url
        assert not api(url + '/v1/chats/archived')['conversations']
        assert chat_id not in [chat['id'] for chat in api(url + '/v1/chats')['conversations']]
        api(url + '/v1/chats/' + chat_id + '/restore', {}, expected=404)
        api(scoped + '/v1/workflow', expected=404)
        assert api(url + '/chats/' + other['id'] + '/v1/workflow')['draft'] is None
        print('PASS: saved work and Archive survive restart; confirmed deletion stays deleted; other workflows preserved; stale writes refused; no runs started')
    finally:
        if started:
            cli('down', '-s', 'saved')
