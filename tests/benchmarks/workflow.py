#!/usr/bin/env python3
"""Paired live legacy-hierarchy benchmark. Opt in: OMAR_LIVE_BACKENDS=1.
Never sends terminal input or human reminders. See docs/workflow-benchmark.md.
"""
import argparse
import concurrent.futures
import hashlib
import http.server
import json
import os
from pathlib import Path
import random
import selectors
import shlex
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import uuid
from context_interventions import native_compact, fresh_conversation

ROOT = Path(__file__).resolve().parents[2]
MODELS = {'claude': 'claude-sonnet-4-6', 'codex': 'gpt-5.6-sol',
          'cursor': 'composer-2.5', 'agy': 'gemini-3.8-flash-medium', 'opencode': 'opencode/big-pickle'}
COMMANDS = {'claude': 'claude --dangerously-skip-permissions',
            'codex': 'codex --dangerously-bypass-approvals-and-sandbox',
            'cursor': 'cursor agent --yolo', 'agy': 'agy --dangerously-skip-permissions', 'opencode': 'opencode'}
CHILDREN = {'claude': ['codex', 'opencode'], 'codex': ['opencode', 'claude'],
            'opencode': ['claude', 'codex'], 'cursor': ['codex', 'agy'], 'agy': ['cursor', 'opencode']}
SOURCE_HOME = Path.home()
CLAUDE_OAUTH_TOKEN = None
CURSOR_ACCESS_TOKEN = None
CURSOR_REFRESH_TOKEN = None

def cached_cursor_access(kind="access"):
    if shutil.which('security'):
        value=subprocess.run(['security','find-generic-password','-s','cursor-'+kind+'-token','-a','cursor-user','-w'],capture_output=True,text=True,timeout=8)
        if value.returncode==0: return value.stdout.strip()
    source=SOURCE_HOME/'.cursor/auth.json'
    if source.exists(): return json.loads(source.read_text()).get(kind+'Token')
    return None

def cached_claude_oauth():
    # Reuse only this application's existing credential. Never serialize it
    # into benchmark results or print native auth responses.
    if os.environ.get('CLAUDE_CODE_OAUTH_TOKEN'):
        return os.environ['CLAUDE_CODE_OAUTH_TOKEN']
    if shutil.which('security'):
        value = subprocess.run(['security','find-generic-password','-s','Claude Code-credentials','-w'],
            capture_output=True,text=True,timeout=8)
        try: return json.loads(value.stdout).get('claudeAiOauth',{}).get('accessToken')
        except json.JSONDecodeError: pass
    source = SOURCE_HOME/'.claude/.credentials.json'
    if source.exists():
        return json.loads(source.read_text()).get('claudeAiOauth',{}).get('accessToken')
    return None


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True))


def credentials(home):
    """Copy only authentication/config needed by isolated CLIs; never log it."""
    for relative in ['.codex/auth.json', '.gemini/antigravity-cli/antigravity-oauth-token',
                     '.gemini/oauth_creds.json', '.gemini/google_accounts.json']:
        src = SOURCE_HOME / relative
        if src.is_file():
            dst = home / relative
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(src, dst); dst.chmod(0o600)
    cursor = SOURCE_HOME / '.cursor/cli-config.json'
    if cursor.exists():
        original = json.loads(cursor.read_text())
        dest = home / '.cursor/cli-config.json'; dest.parent.mkdir(parents=True, exist_ok=True)
        write_json(dest, {k: original[k] for k in ['authInfo', 'version'] if k in original})
        dest.chmod(0o600)
    # The installed `cursor` shim compares itself with $HOME/.local/bin/cursor;
    # changing HOME otherwise makes it exec itself forever. Pin its real agent.
    installed=shutil.which('cursor-agent')
    if installed:
        launcher=home/'bin/cursor'; launcher.parent.mkdir(parents=True,exist_ok=True)
        launcher.write_text('#!/bin/sh\nexec '+shlex.quote(str(Path(installed).resolve()))+' "$@"\n')
        launcher.chmod(0o700)
    if CURSOR_ACCESS_TOKEN:
        auth=home/'.cursor/auth.json'; auth.parent.mkdir(exist_ok=True)
        write_json(auth,{'accessToken':CURSOR_ACCESS_TOKEN,'refreshToken':CURSOR_REFRESH_TOKEN}); auth.chmod(0o600)
    (home / '.codex').mkdir(exist_ok=True)
    (home / '.codex/config.toml').write_text('model_reasoning_effort="low"\n')
    (home / '.claude').mkdir(exist_ok=True)
    setup = {'hasCompletedOnboarding': True, 'theme': 'dark', 'bypassPermissionsModeAccepted': True}
    key = os.environ.get('ANTHROPIC_API_KEY')
    if key: setup['customApiKeyResponses'] = {'approved': [key[-20:]], 'rejected': []}
    write_json(home / '.claude.json', setup)
    (home / '.claude.json').chmod(0o600)


def environment(home, server):
    env = dict(os.environ, PATH=str(home/'bin')+os.pathsep+os.environ['PATH'], HOME=str(home), CODEX_HOME=str(home / '.codex'), OMAR_TMUX_SERVER=server,
               TERM='xterm-256color', NO_OPEN_BROWSER='1', AGENT_CLI_CREDENTIAL_STORE='file', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1')
    for key in ['TMUX', 'OMAR_AGENT_NAME', 'OMAR_EA_ID', 'OMAR_MCP_CONTEXT_FILE', 'OMAR_EVENT_SPOOL',
                'OMAR_DIR', 'CLAUDE_CONFIG_DIR', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_CONTENT',
                'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME']:
        env.pop(key, None)
    if CLAUDE_OAUTH_TOKEN:
        env.pop('ANTHROPIC_API_KEY',None)
        env['CLAUDE_CODE_OAUTH_TOKEN'] = CLAUDE_OAUTH_TOKEN
    return env


class MCP:
    def __init__(self, binary, context, env, log):
        self.process = subprocess.Popen([str(binary), 'mcp-server', '--context-file', str(context)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, env=env)
        self.selector = selectors.DefaultSelector(); self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.pending, self.seq = b'', 0
    def call(self, tool_name, timeout=110, **arguments):
        self.seq += 1
        self.process.stdin.write((json.dumps({'jsonrpc':'2.0','id':self.seq,'method':'tools/call',
            'params':{'name':tool_name,'arguments':arguments}})+'\n').encode()); self.process.stdin.flush()
        end = time.monotonic()+timeout
        while time.monotonic()<end:
            while b'\n' in self.pending:
                line, self.pending = self.pending.split(b'\n', 1)
                reply = json.loads(line)
                if reply.get('id') != self.seq: continue
                result = reply.get('result', {})
                if 'error' in reply or result.get('isError'): raise RuntimeError(reply)
                return result['structuredContent']
            if self.selector.select(min(1, max(0, end-time.monotonic()))):
                chunk = os.read(self.process.stdout.fileno(), 65536)
                if not chunk: raise RuntimeError('MCP exited')
                self.pending += chunk
        raise TimeoutError(tool_name)
    def close(self):
        self.process.terminate()
        try: self.process.wait(timeout=5)
        except subprocess.TimeoutExpired: self.process.kill(); self.process.wait()
        self.selector.close(); self.process.stdin.close(); self.process.stdout.close()


def task(children):
    jobs = []
    for role, backend in zip(['left','right'], children):
        jobs.append(f"- {role}: backend={backend}, model={MODELS[backend]}. Task: run `python3 worker_fixture.py {role}` in this workspace with a tool timeout of at least 180 seconds. It waits on a fixture I/O gate, then writes {role}.output.json and prints its result. Do not modify the fixture, gate, inputs or outputs. Report the printed result using the normal OMAR completion protocol available to you. Do not delegate further.")
    return '\n'.join([
        'You are pm, managing a bounded delegation benchmark under the existing project 1.',
        'Use OMAR MCP to spawn exactly two children named left and right, parent=pm, project_id=1, using these backend/model settings:',
        *jobs,
        'Do not run either worker fixture yourself. Use only the provided workspace. Do not ask the user for input.',
        'The fixture gate is opened automatically by the harness after both workers start. Do not open it yourself.',
        'Manage both workers through completion, read and incorporate their actual results, and retire both child sessions through OMAR.',
        'Write final.json containing {"total": <sum of both answers>, "nonces": [<left nonce>, <right nonce>]} from their results.',
        'Then report completion to your parent through the normal OMAR completion protocol available on this branch. Do not complete the project; you are still its tracked PM.',
        'No human reminders will arrive. Persist any recovery notes you normally need. Do not modify OMAR or its runtime state files.'
    ])


def run_case(binary, revision, variant, parent_backend, scenario, seed, output, budget):
    case_id = f'{variant}-{parent_backend}-{scenario}-{seed}'
    case_dir = output / case_id; case_dir.mkdir(parents=True)
    server = 'omar-bench-' + uuid.uuid4().hex[:12]
    receipts = []
    class Receiver(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            receipts.append({'time':time.monotonic(), 'body':json.loads(self.rfile.read(int(self.headers['Content-Length'])))})
            self.send_response(204); self.end_headers()
    httpd = http.server.ThreadingHTTPServer(('127.0.0.1',0), Receiver)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    result = {'case_id':case_id,'variant':variant,'revision':revision,'parent':parent_backend,
              'children':CHILDREN[parent_backend],'scenario':scenario,'seed':seed,'models':MODELS,
              'human_reminders':0,'budget_seconds':budget,'outcome':'harness_error'}
    mcp = None; started = time.monotonic(); log = (case_dir/'mcp.log').open('w')
    # Homes (including copied auth) are always destroyed; only sanitized state
    # and fixture artifacts go into the result directory.
    with tempfile.TemporaryDirectory(prefix='omar-bench-',dir='/tmp') as directory:
        home = Path(directory); credentials(home)
        work = home/'work'; work.mkdir(); (work/'.git').mkdir()
        state = home/'.omar'; state.mkdir()
        trust = json.loads((home/'.claude.json').read_text())
        trust['projects'] = {str(work.resolve()): {'hasTrustDialogAccepted': True, 'hasCompletedProjectOnboarding': True}}
        write_json(home/'.claude.json',trust)
        env = environment(home,server)
        config = '[dashboard]\nsession_prefix="bench-"\n[agent]\ndefault_command="cat"\ndefault_workdir='+json.dumps(str(work))+'\n'
        (state/'config.toml').write_text(config)
        context = home/'context.json'
        write_json(context, dict(omar_dir=str(state),ea_id=0,session_prefix='bench-',default_command='cat',
            default_workdir=str(work),health_idle_warning=15,tmux_server=server))
        shutil.copyfile(Path(__file__).with_name('worker_fixture.py'),work/'worker_fixture.py')
        generator = random.Random(seed); expected = []
        for role in ['left','right']:
            values = [generator.randrange(1,1000) for _ in range(16)]
            nonce = hashlib.sha256(f'{seed}:{role}'.encode()).hexdigest()[:20]
            write_json(work/f'{role}.input.json',{'values':values,'nonce':nonce})
            expected.append({'answer':sum(values),'nonce':nonce})
        assignment = task(CHILDREN[parent_backend])
        result['task_sha256'] = hashlib.sha256(assignment.encode()).hexdigest()
        (case_dir/'assignment.txt').write_text(assignment)
        def tmux(*args,check=True):
            proc = subprocess.run(['tmux','-L',server,*args],env=env,capture_output=True,text=True,timeout=10)
            if check and proc.returncode: raise RuntimeError(proc.stderr.strip())
            return proc.stdout
        def snapshot():
            sessions = tmux('list-sessions','-F','#{session_name}',check=False).splitlines()
            ledger = json.loads((state/'task-lifecycle.json').read_text()) if (state/'task-lifecycle.json').exists() else {}
            return sessions,ledger
        try:
            tmux('new-session','-d','-s','bench-ea-0','-c',str(work),'cat')
            tmux('set-environment','-t','bench-ea-0','OMAR_BACKEND','opencode')
            tmux('set-environment','-t','bench-ea-0','OMAR_DELIVERY',f'opencode:{httpd.server_port}:parent')
            # Both branches get their normal dashboard scheduler.
            tmux('new-session','-d','-s','omar-dashboard','-x','110','-y','35','-c',str(work),shlex.quote(str(binary)))
            mcp = MCP(binary,context,env,log)
            project = mcp.call('add_project',name='paired hierarchy benchmark')['project_id']
            assert project == 1
            root = mcp.call('spawn_agent',name='pm',project_id=1,task=assignment,backend=parent_backend,model=MODELS[parent_backend])
            result['spawn_response'] = root
            events=[]; previous=None; both_ready=None; released=False; finished=None
            output_times={}; retired_times={}; seen_children=set()
            while time.monotonic()-started<budget:
                elapsed = round(time.monotonic()-started,3)
                sessions,ledger = snapshot()
                ready = all((work/f'{r}.ready').exists() for r in ['left','right'])
                if ready and both_ready is None:
                    both_ready = time.monotonic(); result['both_children_ready_s']=elapsed
                if both_ready is not None and not released and time.monotonic()-both_ready>=10:
                    if scenario == 'compact':
                        result['intervention']=native_compact(tmux,parent_backend,MODELS[parent_backend])
                    elif scenario == 'restart':
                        result['intervention']=fresh_conversation(tmux,parent_backend,home)
                    (work/'release').touch(); released=True; result['released_s']=elapsed
                outputs = {}
                for role in ['left','right']:
                    try: outputs[role]=json.loads((work/f'{role}.output.json').read_text())
                    except (OSError,json.JSONDecodeError): pass
                for role in ['left','right']:
                    child_session='bench-0-'+role
                    if child_session in sessions: seen_children.add(role)
                    if role in outputs: output_times.setdefault(role,elapsed)
                    if role in seen_children and child_session not in sessions: retired_times.setdefault(role,elapsed)
                final = None
                try: final=json.loads((work/'final.json').read_text())
                except (OSError,json.JSONDecodeError): pass
                current=json.dumps({'sessions':sorted(sessions),'ledger':ledger,'outputs':outputs,'final':final},sort_keys=True)
                if current!=previous:
                    events.append({'seconds':elapsed,'state':json.loads(current)}); previous=current
                    write_json(case_dir/'events.json',events)
                orphans=[s for s in sessions if s.startswith('bench-0-') and s!='bench-0-pm']
                valid=final == {'total':sum(e['answer'] for e in expected),'nonces':[e['nonce'] for e in expected]}
                attributed=len(outputs)==2 and all(outputs[r].get('actor')=='bench-0-'+r for r in ['left','right'])
                tasks=list(ledger.get('tasks',{}).values())
                manager_done=any(t['agent']=='pm' and t['status']=='completed' for t in tasks) if tasks else any('[CHILD COMPLETE] pm' in json.dumps(r['body']) for r in receipts)
                intervention=result.get('intervention',{})
                if intervention.get('pending_native_id'):
                    path=Path(intervention['pending_native_id'])
                    if path.exists():
                        new_id=json.loads(path.read_text()).get('session')
                        if new_id and new_id!=intervention['old_id']:
                            intervention.update(verified=True,new_id=new_id)
                            del intervention['pending_native_id']
                context_verified=scenario=='normal' or intervention.get('verified',False)
                if valid and attributed and not orphans and manager_done and context_verified:
                    finished=elapsed; result['outcome']='passed'; break
                time.sleep(.25)
            else: result['outcome']='timeout'
            result['result_to_retirement_seconds']={r:round(retired_times[r]-output_times[r],3)
                for r in output_times if r in retired_times}
            result['unretired_result_seconds']={r:round(time.monotonic()-started-output_times[r],3)
                for r in output_times if r not in retired_times}
            result.update(completed_s=finished,elapsed_s=round(time.monotonic()-started,3),artifact_valid=valid,
                worker_execution_attributed=attributed,orphan_sessions=orphans,manager_reported_completion=manager_done,
                result_receipts=len(receipts),terminal_unacknowledged=sum(t['status'] in ['completed','failed'] and not t['acknowledged'] for t in tasks if t['agent']!='pm'))
            if result['outcome']=='timeout' and both_ready is None:
                result['failure_phase']='startup_or_delegation'
            elif result['outcome']=='timeout': result['failure_phase']='result_collection_or_retirement'
            write_json(case_dir/'ledger.json',ledger)
        except Exception as error:
            result['error']=str(error)
        finally:
            for name in tmux('list-sessions','-F','#{session_name}',check=False).splitlines():
                (case_dir/(name+'.txt')).write_text(tmux('capture-pane','-p','-S','-500','-t',name,check=False))
            if mcp: mcp.close()
            tmux('kill-server',check=False)
            httpd.shutdown(); httpd.server_close(); log.close()
    write_json(case_dir/'result.json',result)
    print(json.dumps({k:result.get(k) for k in ['case_id','outcome','elapsed_s','failure_phase','error']}),flush=True)
    return result


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--baseline',type=Path,required=True)
    parser.add_argument('--candidate',type=Path,default=ROOT/'target/debug/omar')
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--parents',nargs='+',default=list(CHILDREN))
    parser.add_argument('--variants',nargs='+',default=['baseline','candidate'])
    parser.add_argument('--scenarios',nargs='+',choices=['normal','compact','restart'],default=['normal'])
    parser.add_argument('--seeds',nargs='+',type=int,default=[314159])
    parser.add_argument('--budget',type=int,default=240)
    parser.add_argument('--jobs',type=int,default=2)
    args=parser.parse_args()
    if os.environ.get('OMAR_LIVE_BACKENDS')!='1': parser.error('requires OMAR_LIVE_BACKENDS=1')
    global CLAUDE_OAUTH_TOKEN, CURSOR_ACCESS_TOKEN, CURSOR_REFRESH_TOKEN
    CLAUDE_OAUTH_TOKEN = cached_claude_oauth()
    CURSOR_ACCESS_TOKEN = cached_cursor_access()
    CURSOR_REFRESH_TOKEN = cached_cursor_access("refresh")
    args.output.mkdir(parents=True,exist_ok=True); args.output.chmod(0o700)
    revisions={}
    for variant,binary in [('baseline',args.baseline),('candidate',args.candidate)]:
        binary=binary.resolve()
        revisions[variant]=subprocess.check_output(['git','-C',str(binary.parent.parent.parent),'rev-parse','HEAD'],text=True).strip()
    jobs=[(variant,parent,scenario,seed) for seed in args.seeds for parent in args.parents for scenario in args.scenarios for variant in args.variants if scenario!='compact' or parent in ['codex','opencode']]
    # Counterbalance which branch starts first; never change a scenario after its pair starts.
    random.Random(2718).shuffle(jobs)
    versions={}
    for backend,arguments in {'claude':['claude','--version'],'codex':['codex','--version'],
        'cursor':['cursor','agent','--version'],'agy':['agy','--version'],'opencode':['opencode','--version']}.items():
        version=subprocess.run(arguments,capture_output=True,text=True,timeout=15)
        versions[backend]={'version':(version.stdout or version.stderr).strip().splitlines()[0] if version.returncode==0 else 'not reported',
            'launcher_sha256':hashlib.sha256(Path(shutil.which(arguments[0])).resolve().read_bytes()).hexdigest()}
    manifest={'revisions':revisions,'models':MODELS,'cases':jobs,'budget':args.budget,'versions':versions,
        'harness_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'human_reminders':0,
        'binary_sha256':{v:hashlib.sha256(p.read_bytes()).hexdigest() for v,p in [('baseline',args.baseline),('candidate',args.candidate)]}}
    write_json(args.output/'manifest.json',manifest)
    results=[]
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
        futures=[executor.submit(run_case,(args.baseline if v=='baseline' else args.candidate).resolve(),
            revisions[v],v,p,scenario,seed,args.output,args.budget) for v,p,scenario,seed in jobs]
        for future in concurrent.futures.as_completed(futures):
            results.append(future.result()); write_json(args.output/'results.json',results)


if __name__=='__main__': main()
