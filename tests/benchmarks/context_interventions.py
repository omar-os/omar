"""Native context interventions. No terminal input and no task restatement."""
import json
from pathlib import Path
import shlex
import socket
import subprocess
import time
import urllib.request

class Rpc:
    def __init__(self,path):
        import websocket
        sock=socket.socket(socket.AF_UNIX); sock.settimeout(5); sock.connect(str(path))
        self.ws=websocket.create_connection('ws://localhost/',socket=sock,timeout=5)
        self.seq=0; self.notifications=[]
        self.call('initialize',{'clientInfo':{'name':'omar_context_benchmark','version':'1'}})
        self.ws.send(json.dumps({'method':'initialized','params':{}}))
    def call(self,method,params):
        self.seq+=1; self.ws.send(json.dumps({'id':self.seq,'method':method,'params':params}))
        while True:
            value=json.loads(self.ws.recv())
            if value.get('id')==self.seq:
                if 'error' in value: raise RuntimeError(value['error'])
                return value['result']
            self.notifications.append(value)
    def close(self): self.ws.close()

def http(port,path,body=None,timeout=10):
    request=urllib.request.Request(f'http://127.0.0.1:{port}'+path,
        data=None if body is None else json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request,timeout=timeout) as response:
        raw=response.read()
        return json.loads(raw) if raw else None

def stamp(tmux):
    value=tmux('show-environment','-t','bench-0-pm','OMAR_DELIVERY').strip()
    return value.removeprefix('OMAR_DELIVERY=')

def native_compact(tmux,backend,model):
    channel=stamp(tmux); started=time.monotonic()
    if backend=='codex' and channel.startswith('codex:'):
        rpc=Rpc(channel.split(':',1)[1])
        try:
            threads=rpc.call('thread/loaded/list',{})['data']; assert len(threads)==1
            thread=threads[0]
            def compact_ids():
                data=rpc.call('thread/read',{'threadId':thread,'includeTurns':True})
                return {item['id'] for turn in data['thread'].get('turns',[]) if turn.get('status')=='completed' for item in turn.get('items',[]) if item['type']=='contextCompaction'}
            before=compact_ids()
            rpc.call('thread/compact/start',{'threadId':thread})
            end=time.monotonic()+75
            while time.monotonic()<end:
                after=compact_ids()
                if after-before or any(n.get('method')=='thread/compacted' and n['params']['threadId']==thread for n in rpc.notifications):
                    return {'kind':'native_compaction','verified':True,'thread':thread,'new_compaction_ids':sorted(after-before),'seconds':round(time.monotonic()-started,3)}
                time.sleep(.25)
            raise TimeoutError('Codex did not confirm compaction')
        finally: rpc.close()
    if backend=='opencode' and channel.startswith('opencode:'):
        _,port,session=channel.split(':',2); port=int(port)
        before={m['info']['id'] for m in http(port,f'/session/{session}/message')}
        provider,model_id=model.split('/',1)
        accepted=http(port,f'/session/{session}/summarize',{'providerID':provider,'modelID':model_id},timeout=75)
        end=time.monotonic()+20
        while time.monotonic()<end:
            messages=http(port,f'/session/{session}/message')
            summaries=[m['info']['id'] for m in messages if m['info']['id'] not in before and m['info'].get('summary') and m['info'].get('time',{}).get('completed')]
            if accepted and summaries:
                return {'kind':'native_compaction','verified':True,'session':session,'summary_ids':summaries,'seconds':round(time.monotonic()-started,3)}
            time.sleep(.25)
        raise TimeoutError('OpenCode did not confirm a completed summary')
    raise NotImplementedError('native compaction is not verified for '+backend)


def fresh_conversation(tmux,backend,home):
    """Replace the PM backend process, keep its named pane and task ownership."""
    if backend=='claude':
        def identity():
            pid=int(tmux('display-message','-p','-t','bench-0-pm','#{pane_pid}').strip())
            rows=subprocess.check_output(['ps','-axo','pid=,ppid='],text=True).splitlines()
            pairs=[tuple(map(int,row.split())) for row in rows if len(row.split())==2]
            descendants={pid}
            for _ in range(8):
                descendants.update(p for p,parent in pairs if parent in descendants)
            for registry in (home/'.claude/sessions').glob('*.json'):
                value=json.loads(registry.read_text())
                if value.get('pid') in descendants: return value.get('sessionId')
            return None
        old_id=identity(); assert old_id, 'Claude session identity unavailable'
        command=tmux('display-message','-p','-t','bench-0-pm','#{pane_start_command}').strip()
        tmux('respawn-pane','-k','-t','bench-0-pm',command)
        deadline=time.monotonic()+50
        while time.monotonic()<deadline:
            new_id=identity()
            if new_id and new_id!=old_id:
                return {'kind':'fresh_conversation','verified':True,'old_id':old_id,'new_id':new_id}
            time.sleep(.25)
        raise TimeoutError('Claude did not publish a fresh native session identity')
    old_stamp=stamp(tmux)
    command=tmux('display-message','-p','-t','bench-0-pm','#{pane_start_command}').strip()
    old_id=None
    if old_stamp.startswith('managed:'):
        args=shlex.split(command); config_path=Path(args[args.index('--config-file')+1])
        config=json.loads(config_path.read_text())
        inbox=config_path.with_suffix('.inbox.json')
        old_id=json.loads(inbox.read_text()).get('session') if inbox.exists() else None
        replacement=home/'reset-runner.json'; new_socket=home/'reset.sock'
        config['initial_session']=None; config['socket']=str(new_socket)
        replacement.write_text(json.dumps(config)); replacement.chmod(0o600)
        command=command.replace(str(config_path),str(replacement)).replace(old_stamp.split(':',1)[1],str(new_socket))
        tmux('set-environment','-t','bench-0-pm','OMAR_DELIVERY','managed:'+str(new_socket))
        tmux('respawn-pane','-k','-t','bench-0-pm',command)
        time.sleep(1)
        deadline=time.monotonic()+50
        new_inbox=replacement.with_suffix('.inbox.json')
        while time.monotonic()<deadline:
            if new_inbox.exists():
                new_id=json.loads(new_inbox.read_text()).get('session')
                if new_id and new_id!=old_id:
                    return {'kind':'fresh_conversation','verified':True,'old_id':old_id,'new_id':new_id}
            # Antigravity emits its session ID only when a real event starts a
            # turn. Do not manufacture a prompt to obtain one; verify later.
            if backend=='agy' and new_socket.exists():
                return {'kind':'fresh_conversation','verified':False,'old_id':old_id,'pending_native_id':str(new_inbox)}
            time.sleep(.2)
        raise TimeoutError('managed runner did not create a new conversation')
    if old_stamp.startswith('codex:'):
        path=old_stamp.split(':',1)[1]; rpc=Rpc(path)
        try: old_ids=rpc.call('thread/loaded/list',{})['data']; assert len(old_ids)==1; old_id=old_ids[0]
        finally: rpc.close()
        tmux('respawn-pane','-k','-t','bench-0-pm',command)
        time.sleep(1)
        deadline=time.monotonic()+50
        while time.monotonic()<deadline:
            try:
                rpc=Rpc(path)
                try: ids=rpc.call('thread/loaded/list',{})['data']
                finally: rpc.close()
                if len(ids)==1 and ids[0]!=old_id:
                    return {'kind':'fresh_conversation','verified':True,'old_id':old_id,'new_id':ids[0]}
            except (OSError,RuntimeError,ValueError): pass
            time.sleep(.25)
        raise TimeoutError('Codex did not create a new native conversation')
    if old_stamp.startswith('opencode:'):
        _,port,old_id=old_stamp.split(':',2); port=int(port)
        tmux('respawn-pane','-k','-t','bench-0-pm',command)
        time.sleep(1)
        deadline=time.monotonic()+50; created=None
        while time.monotonic()<deadline:
            try:
                if created is None: created=http(port,'/session',{})['id']
                if http(port,'/tui/select-session',{'sessionID':created}):
                    tmux('set-environment','-t','bench-0-pm','OMAR_DELIVERY',f'opencode:{port}:{created}')
                    return {'kind':'fresh_conversation','verified':created!=old_id,'old_id':old_id,'new_id':created}
            except OSError: pass
            time.sleep(.25)
        raise TimeoutError('OpenCode did not establish a fresh native conversation')
    raise NotImplementedError('fresh native conversation identity is not verified for '+backend)
