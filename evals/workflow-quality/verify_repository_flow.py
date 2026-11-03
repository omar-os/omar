#!/usr/bin/env python3
"""Replay known repository outputs through a real Web-only daemon; no models/human reviews."""
import argparse
import json
import os
import signal
import subprocess
import time
import urllib.error
from pathlib import Path
from program import build
from repository_grade import grade_repository
from repository_sandbox import resolve_image
from run import REPO, ROOT, dump, file_digest, request, start_daemon


def verify(args):
    output=args.output.resolve();output.mkdir(parents=True,exist_ok=False)
    image=resolve_image(args.image)
    task=json.loads((ROOT/"repository-repair/inputs/repo-schedule-01.json").read_text())
    reference=json.loads((ROOT/"repository-repair/references/repo-schedule-01.json").read_text())
    registry=json.loads((REPO/"config/execution-profiles.example.json").read_text())
    dump(output/"manifest.json",{"model_calls":0,"mode":"scripted Web-only replay, not native designer/agent behavior or human acceptance","image":image,"omar_digest":file_digest(args.omar),"compiler_digest":file_digest(args.compiler),"harness_digests":{p.name:file_digest(p) for p in ROOT.glob("*.py")}})
    reports=[]
    for scenario in ["oracle","hardcoded","malformed-then-oracle"]:
        directory=output/scenario;directory.mkdir()
        process=log=socket=None
        try:
            proposal=build(task,"same_model_workflow","unused-quality","unused-economy",backend="Web")
            proposal["plan"]["preferences"]={"allowed_backends":[],"overrides":{}}
            dump(directory/"proposal.json",proposal)
            process,log,url,state,socket=start_daemon(directory,args,registry)
            token=json.loads((state/"mcp/ea-0/context.json").read_text())["serve"]["token"]
            request(url,"/v1/agent/proposals",{**proposal,"token":token})
            chat=request(url,"/v1/chat")
            design=next(m["design"] for m in reversed(chat["messages"]) if m.get("design"))
            record=request(url,"/v1/runs",{"program":proposal["program"],"inputs":proposal["inputs"],"plan":design["plan"],"fast":True,"timeout_seconds":10})
            run_id=record["run_id"];answered=set();deadline=time.monotonic()+30
            candidate=reference["oracle_patch"]
            if scenario=="hardcoded":candidate={"changes":[{"path":"candidate_app/api.py","content":"def handle(request):\n    return [[1,6]]\n"}],"explanation":"Intentionally hardcoded public example"}
            if scenario=="malformed-then-oracle":candidate={"missing_patch":True}
            while record["status"] in ["starting","running","stopping"]:
                if time.monotonic()>=deadline:
                    request(url,f"/v1/runs/{run_id}/stop",{})
                    raise TimeoutError("Scripted replay exceeded 30 seconds")
                try:panel=request(url,f"/v1/runs/{run_id}/panel")
                except urllib.error.HTTPError as error:
                    if error.code!=404:raise
                    panel={}
                for pending in panel.get("pending",[]):
                    if pending["invocation_id"] in answered:continue
                    index=int(pending["reaction"].rsplit(".",1)[1])
                    port,value={0:("flow.evidence","Scripted fixture context"),1:("flow.raw",json.dumps(candidate)),2:("flow.result",json.dumps(candidate)),3:("flow.repaired",json.dumps(reference["oracle_patch"])),4:("flow.result",json.dumps(reference["oracle_patch"]))}[index]
                    request(url,f"/v1/runs/{run_id}/panel",{"invocation_id":pending["invocation_id"],"agent":pending["agent"],"values":{port:value}})
                    answered.add(pending["invocation_id"])
                time.sleep(.05);record=request(url,f"/v1/runs/{run_id}")
            if record["status"]!="completed":raise RuntimeError(f"Replay failed: {record}")
            receipt=request(url,"/v1/workflow/receipts",{"run_id":run_id});dump(directory/"receipt.json",receipt)
            accepted=[]
            for path in (state/"ea/0/serve"/run_id/"invocations").glob("*.json"):
                invocation=json.loads(path.read_text())
                if invocation["status"]=="contract_satisfied" and "flow.result" in (invocation.get("outputs") or {}):accepted.append(invocation["outputs"]["flow.result"])
            if len(accepted)!=1:raise RuntimeError("Expected exactly one accepted terminal patch")
            dump(directory/"accepted-output.json",accepted[0])
            graded=grade_repository(task,reference,accepted[0],image=image,artifacts=directory/"evaluation")
            if graded["deterministic_pass"] is not (scenario!="hardcoded"):raise RuntimeError(f"Unexpected protected result: {graded}")
            if len(answered)!=(5 if scenario=="malformed-then-oracle" else 3):raise RuntimeError("Unexpected repair invocation count")
            if scenario=="malformed-then-oracle" and not any(v["decision"]=="repair_requested" for v in receipt["verification"]):raise RuntimeError("Missing recorded shape-check repair")
            reports.append({"scenario":scenario,"run_id":run_id,"runtime_status":record["status"],"invocations":len(answered),"protected_pass":graded["deterministic_pass"],"model_calls":0})
            dump(output/"report.json",reports);print(json.dumps(reports[-1]),flush=True)
        finally:
            if process:
                process.terminate()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:os.killpg(process.pid,signal.SIGKILL);process.wait()
            if socket:subprocess.run(["tmux","-L",socket,"kill-server"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            if log:log.close()


if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--output",type=Path,required=True);p.add_argument("--image",required=True)
    p.add_argument("--omar",type=Path,default=REPO/"target/debug/omar");p.add_argument("--compiler",type=Path,default=REPO/"lang/.lake/build/bin/omarc")
    verify(p.parse_args())
