#!/usr/bin/env python3
"""Internal evaluation on the real daemon. Defaults to preparing files, never model calls."""
import argparse, hashlib, json, os, select, signal, subprocess, time, urllib.request, uuid
from pathlib import Path
from program import build
from grade import grade

ROOT=Path(__file__).resolve().parent
REPO=ROOT.parent.parent
CONFIGURATIONS=["single_agent","same_model_workflow","routed_workflow"]

def dump(path,value):
    path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(value,indent=2)+"\n")

def file_digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None

def request(url,path,body=None,*,timeout=120):
    data=None if body is None else json.dumps(body).encode()
    req=urllib.request.Request(url+path,data=data,headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req,timeout=timeout) as response:return json.load(response)

def start_daemon(directory,args,registry):
    state=directory/"state";work=directory/"workspace";state.mkdir(parents=True);work.mkdir()
    dump(state/"execution-profiles.json",registry)
    prefix="omar-eval-"+uuid.uuid4().hex[:10]+"-"
    config=state/"config.toml"
    config.write_text(f'[dashboard]\nsession_prefix = "{prefix}"\n[agent]\ndefault_workdir = {json.dumps(str(work))}\n')
    env={**os.environ,"OMAR_STATE_DIR":str(state),"OMARC_BIN":str(args.compiler),"OMAR_TMUX_SERVER":"eval-"+uuid.uuid4().hex[:10]}
    log=(directory/"daemon.log").open("w")
    process=subprocess.Popen([str(args.omar),"--config",str(config),"serve","--no-ea","--address","127.0.0.1:0"],env=env,stdout=subprocess.PIPE,stderr=log,text=True,start_new_session=True)
    deadline=time.monotonic()+30;url=None
    while time.monotonic()<deadline and process.poll() is None:
        ready,_,_=select.select([process.stdout],[],[],1)
        if ready:
            line=process.stdout.readline();log.write(line);log.flush()
            if "OMAR serve: http" in line:url=line.split("OMAR serve: ",1)[1].strip();break
    if not url: process.terminate();process.wait(timeout=5);log.close();raise RuntimeError("Daemon did not start; inspect daemon.log")
    return process,log,url,state,env["OMAR_TMUX_SERVER"]

def summarize(records):
    report={"planned_runs":len(records),"attempted_runs":sum(r.get("attempted",False) for r in records),
            "results":records,"superiority_established":False,
            "qualification":"Development diagnostics do not qualify a route. Labels, semantic review, independent groups and resource coverage remain separate requirements.","by_configuration":{}}
    for name in CONFIGURATIONS:
        planned=[r for r in records if r["configuration"]==name]
        rows=[r for r in planned if r.get("attempted")]
        report["by_configuration"][name]={"planned":len(planned),"attempts":len(rows),
            "not_started":sum(not r.get("attempted") for r in planned),
            "failures":sum(r["status"] in ["failed","interrupted"] for r in rows),
            "incomplete_attempts":sum(r["status"]=="running" for r in rows),
            "diagnostic_passes":sum((r["quality"] or {}).get("deterministic_pass") is True for r in rows),
            "unknown_costs":sum(r["cost_usd"] is None for r in rows),
            "unknown_quality":sum((r["quality"] or {}).get("deterministic_pass") is None for r in rows),
            "evaluation_infrastructure_failures":sum((r["quality"] or {}).get("evaluation_status")=="infrastructure_failure" for r in rows),"human_reviewed":0}
    return report


def evaluate(args):
    from evaluation_protocol import load, contained, require_labels, restricted_registry, write
    # Protocol loading and label checks precede any login probe or model start.
    protocol=None
    if args.protocol:
        protocol=load(args.protocol)
        if args.execute:
            if not args.labels_review:raise ValueError("Frozen execution requires --labels-review; pending labels cannot be approved by a model")
            require_labels(protocol,json.loads(args.labels_review.read_text()))
        root=args.protocol.resolve().parent
        registry=json.loads(contained(root,protocol["registry"]).read_text())
        args.quality,args.economy=protocol["quality"],protocol["economy"]
        args.omar=Path(protocol["binaries"]["omar"]["path"]);args.compiler=Path(protocol["binaries"]["compiler"]["path"])
        args.max_wall_seconds=protocol["resources"]["max_campaign_seconds"]
        args.max_attempt_seconds=protocol["resources"]["max_attempt_seconds"]
        args.step_seconds=protocol["resources"]["max_step_seconds"]
        tasks={t["id"]:(contained(root,t["input"]),json.loads(contained(root,t["input"]).read_text()),contained(root,t["reference"])) for t in protocol["tasks"]}
        schedule=protocol["schedule"]
        repository_image=protocol["repository_image"]
    else:
        registry=restricted_registry(args.quality,args.economy)
        tasks={}
        for path in sorted(ROOT.glob("*/inputs/*.json")):
            task=json.loads(path.read_text())
            if task["split"]==args.split and (not args.task or task["id"] in args.task):
                tasks[task["id"]]=(path,task,path.parent.parent/"references"/path.name)
        if not tasks:raise ValueError("No tasks matched")
        schedule=[{"attempt_id":f"attempt-{i+1:04}","task_id":task_id,"configuration":configuration,"repeat":1}
                  for i,(task_id,configuration) in enumerate((t,c) for t in tasks for c in CONFIGURATIONS)][:args.max_runs]
        tasks={t:tasks[t] for t in {r["task_id"] for r in schedule}}
        repository_image=None
        if args.repository_image:
            from repository_sandbox import resolve_image
            repository_image=resolve_image(args.repository_image)
    repository_tasks=any(t[1]["family"]=="repository_repair" for t in tasks.values())
    if args.execute:
        if repository_tasks and not repository_image:raise ValueError("Repository execution needs a pinned local image; no pulls or host-code fallback")
        if any(os.environ.get(name) for name in ["OPENAI_API_KEY","AZURE_OPENAI_API_KEY"]):raise ValueError("API credentials are present; subscription-only diagnostic refuses to execute")
        status=subprocess.run(["codex","login","status"],capture_output=True,text=True,timeout=10)
        if status.returncode or "ChatGPT" not in status.stdout+status.stderr:raise ValueError("ChatGPT subscription authentication not confirmed")
        if repository_tasks:
            from repository_sandbox import resolve_image
            if resolve_image(repository_image)!=repository_image:raise ValueError("Frozen repository image changed")
    out=args.output.resolve();out.mkdir(parents=True,exist_ok=False);out.chmod(0o700)
    settings={"schema_version":2,"created_at":time.time(),"mode":"execute" if args.execute else "prepare",
              "protocol_digest":protocol["digest"] if protocol else None,"protocol":protocol,
              "label_review_digest":file_digest(args.labels_review) if args.labels_review else None,
              "max_runs":len(schedule),"max_wall_seconds":args.max_wall_seconds,"max_attempt_seconds":args.max_attempt_seconds,
              "step_seconds":args.step_seconds,"schedule":schedule,"quality":args.quality,"economy":args.economy,
              "task_ids":list(tasks),"registry":registry,"source_revision":subprocess.run(["git","rev-parse","HEAD"],cwd=REPO,capture_output=True,text=True).stdout.strip(),
              "label_status":"reviewed" if args.execute and protocol else "provisional",
              "resource_policy":"Same input/tools/per-step and whole-attempt time limits. Cleanup/post-run grading may extend wall time. Total native compute/cost parity is unknown.",
              "design_cost":"Prepared templates; interactive design cost is not measured.",
              "repository_evaluation":{"image":repository_image,"acceptance":"Pinned protected regressions gate handoff and one repair/recheck; final grade is also retained."} if repository_tasks else None,
              "artifact_digests":{"omar_binary":file_digest(args.omar),"compiler_binary":file_digest(args.compiler),
                 "harness":{p.name:file_digest(p) for p in sorted(ROOT.glob("*.py"))},
                 "tasks":{task_id:{"input":file_digest(path),"reference":file_digest(ref)} for task_id,(path,_,ref) in tasks.items()}}}
    settings["freeze_digest"]=hashlib.sha256(json.dumps(settings,sort_keys=True).encode()).hexdigest();write(out/"manifest.json",settings)
    # Copy reviewed task bytes into the campaign; later review never depends on
    # an unrelated checkout or an old absolute temporary-directory path.
    for task_id,(path,task,ref) in tasks.items():
        snapshot=out/"tasks"/task_id;snapshot.mkdir(parents=True,exist_ok=True)
        (snapshot/"input.json").write_bytes(path.read_bytes());(snapshot/"reference.json").write_bytes(ref.read_bytes())
    records=[]
    for i,row in enumerate(schedule):
        task=tasks[row["task_id"]][1]
        directory=out/f"{i+1:03}-{task['id']}-{row['configuration']}"
        records.append({**row,"group":task["group"],"family":task["family"],"status":"planned","attempted":False,
                        "directory":str(directory),"artifact_directory":directory.name,"elapsed_seconds":None,
                        "cost_usd":None,"input_tokens":None,"output_tokens":None,"quality":None,"error":None})
    def persist():
        write(out/"results.json",records);write(out/"report.json",summarize(records))
    persist()
    campaign_deadline=time.monotonic()+args.max_wall_seconds
    interrupted=False
    for entry in records:
        if interrupted or (args.execute and time.monotonic()>=campaign_deadline):
            entry["status"]="not_started";entry["error"]="Operator interruption or campaign time budget exhausted";persist();continue
        _,task,reference_path=tasks[entry["task_id"]]
        reference=json.loads(reference_path.read_text())
        directory=out/entry["artifact_directory"];directory.mkdir()
        suite=None
        if task["family"]=="repository_repair" and repository_image:
            from repository_protocol import suite_for
            suite=suite_for(task,reference,repository_image);write(directory/"private-verifier.json",suite)
        proposal=build(task,entry["configuration"],args.quality,args.economy,repository_verifier=suite)
        write(directory/"proposal.json",proposal);(directory/"program.omar").write_text(proposal["program"])
        if not args.execute:
            entry["status"]="prepared";write(directory/"result.json",entry);persist();continue
        started=time.monotonic();deadline=min(campaign_deadline,started+args.max_attempt_seconds)
        process=log=socket=run_id=url=None
        entry["status"]="running";entry["attempted"]=True;entry["started_at"]=time.time()
        write(directory/"result.json",entry);persist()  # Before any runtime/model request.
        def bounded_request(path,body=None):
            remaining=deadline-time.monotonic()
            if remaining<=0:raise TimeoutError("Whole-attempt or campaign time budget exhausted")
            return request(url,path,body,timeout=min(30,remaining))
        try:
            process,log,url,state,socket=start_daemon(directory,args,registry)
            if suite is not None:
                from repository_protocol import bind_catalog
                write(state/"workflow-verifiers"/(suite["id"]+".json"),suite)
                bind_catalog(proposal,bounded_request("/v1/workflow/evidence"));write(directory/"proposal.json",proposal)
            context=json.loads((state/"mcp/ea-0/context.json").read_text())
            bounded_request("/v1/agent/proposals",{**proposal,"token":context["serve"]["token"]})
            chat=bounded_request("/v1/chat");design=next(m["design"] for m in reversed(chat["messages"]) if m.get("design"))
            run=bounded_request("/v1/runs",{"program":proposal["program"],"inputs":proposal["inputs"],"plan":design["plan"],"timeout_seconds":args.step_seconds,"fast":True})
            run_id=run["run_id"];entry["run_id"]=run_id;persist()
            while run["status"] in ["starting","running","stopping"]:
                time.sleep(.1);run=bounded_request(f"/v1/runs/{run_id}")
            receipt=bounded_request("/v1/workflow/receipts",{"run_id":run_id});write(directory/"receipt.json",receipt)
            entry["receipt_digest"]=file_digest(directory/"receipt.json")
            entry.update({k:receipt[k] for k in ["status","cost_usd","input_tokens","output_tokens","error"]})
            outputs={}
            for item in (state/"ea/0/serve"/run_id/"invocations").glob("*.json"):
                invocation=json.loads(item.read_text())
                if invocation["status"]=="contract_satisfied":outputs.update(invocation.get("outputs") or {})
            write(directory/"outputs.json",outputs)
            entry["outputs_digest"]=file_digest(directory/"outputs.json")
            result=outputs.get("flow.result")
            if receipt["status"]=="completed" and result is not None:
                entry["quality"]=grade(task,reference,result,repository_image=repository_image,repository_artifacts=directory/"repository-evaluation")
            else:
                entry["quality"]={"deterministic_pass":None,"evaluation_status":"no_accepted_output","errors":["no completed delivered artifact"],"semantic_acceptance":None}
        except (Exception,KeyboardInterrupt) as error:
            interrupted=isinstance(error,KeyboardInterrupt)
            entry["status"]="interrupted" if interrupted else "failed";entry["error"]=str(error) or "Interrupted by operator"
            entry["quality"]={"deterministic_pass":None,"evaluation_status":"execution_failure","errors":["execution_failure"],"semantic_acceptance":None}
            if run_id and url:
                try:request(url,f"/v1/runs/{run_id}/stop",{},timeout=5)
                except Exception:pass
        finally:
            if process is not None:
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
            if socket:
                try:subprocess.run(["tmux","-L",socket,"kill-server"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=10)
                except (OSError,subprocess.TimeoutExpired) as error:
                    entry["cleanup_error"]=str(error);entry["status"]="failed"
            if log:log.close()
            entry["elapsed_seconds"]=round(time.monotonic()-started,3)
            write(directory/"result.json",entry);persist()
        print(json.dumps({k:entry[k] for k in ["attempt_id","task_id","configuration","status","elapsed_seconds","error"]}),flush=True)
    return summarize(records)


if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--execute",action="store_true");p.add_argument("--output",type=Path,required=True)
    p.add_argument("--protocol",type=Path);p.add_argument("--labels-review",type=Path)
    p.add_argument("--task",action="append");p.add_argument("--split",choices=["development","reserved"],default="development")
    p.add_argument("--max-runs",type=int,default=3);p.add_argument("--max-wall-seconds",type=int,default=600)
    p.add_argument("--max-attempt-seconds",type=int,default=180);p.add_argument("--step-seconds",type=int,default=90)
    p.add_argument("--repository-image");p.add_argument("--quality",default="codex-quality");p.add_argument("--economy",default="codex-economy")
    p.add_argument("--omar",type=Path,default=REPO/"target/debug/omar");p.add_argument("--compiler",type=Path,default=REPO/"lang/.lake/build/bin/omarc")
    a=p.parse_args()
    if any(n<1 for n in [a.max_runs,a.max_wall_seconds,a.max_attempt_seconds,a.step_seconds]):p.error("Budgets must be positive")
    if a.protocol:
        import sys
        frozen_options={"--task","--split","--max-runs","--max-wall-seconds","--max-attempt-seconds","--step-seconds","--repository-image","--quality","--economy","--omar","--compiler"}
        if any(arg.split("=",1)[0] in frozen_options for arg in sys.argv[1:]):p.error("A frozen protocol owns task, profiles, binaries and budgets; prepare a new protocol instead of overriding them")
    evaluate(a)
