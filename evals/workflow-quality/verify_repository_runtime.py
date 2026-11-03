#!/usr/bin/env python3
"""Real-daemon executable gate checks; scripted Web replies, zero model calls."""
import argparse
import json
import os
import signal
import subprocess
import time
import urllib.error
from pathlib import Path
from program import build
from repository_sandbox import command, resolve_image
from repository_protocol import suite_for, bind_catalog
from run import REPO, ROOT, dump, file_digest, request, start_daemon


def verify(args):
    output=args.output.resolve();output.mkdir(parents=True,exist_ok=False)
    image=resolve_image(args.image)
    task=json.loads((ROOT/"repository-repair/inputs/repo-schedule-01.json").read_text())
    reference=json.loads((ROOT/"repository-repair/references/repo-schedule-01.json").read_text())
    registry=json.loads((REPO/"config/execution-profiles.example.json").read_text())
    dump(output/"manifest.json",{"model_calls":0,"mode":"scripted Web-only runtime checks, not agent quality or human acceptance",
        "image":image,"omar_digest":file_digest(args.omar),"compiler_digest":file_digest(args.compiler),
        "harness_digests":{p.name:file_digest(p) for p in ROOT.glob("*.py")}})
    reports=[]
    scenarios=["oracle","hardcoded-then-oracle","exhausted","prohibited-path","missing-image","cancel"]
    if args.scenario:scenarios=[args.scenario]
    for scenario in scenarios:
        directory=output/scenario;directory.mkdir()
        process=log=socket=None
        try:
            suite=suite_for(task,reference,"sha256:"+"0"*64 if scenario=="missing-image" else image)
            proposal=build(task,"same_model_workflow","unused","unused",backend="Web",repository_verifier=suite)
            proposal["plan"]["preferences"]={"allowed_backends":[],"overrides":{}}
            dump(directory/"proposal.json",proposal)
            process,log,url,state,socket=start_daemon(directory,args,registry)
            dump(state/"workflow-verifiers"/(suite["id"]+".json"),suite)
            bind_catalog(proposal,request(url,"/v1/workflow/evidence"))
            dump(directory/"proposal.json",proposal)
            token=json.loads((state/"mcp/ea-0/context.json").read_text())["serve"]["token"]
            request(url,"/v1/agent/proposals",{**proposal,"token":token})
            chat=request(url,"/v1/chat");design=next(m["design"] for m in reversed(chat["messages"]) if m.get("design"))
            record=request(url,"/v1/runs",{"program":proposal["program"],"inputs":proposal["inputs"],"plan":design["plan"],"fast":True,"timeout_seconds":20})
            run_id=record["run_id"];answered=set();deadline=time.monotonic()+90;stopped=False;active_container=None
            candidate=reference["oracle_patch"]
            if scenario in ["hardcoded-then-oracle","exhausted"]:
                candidate={"changes":[{"path":"candidate_app/api.py","content":"def handle(request):\n    return [[1,6]]\n"}],"explanation":"Intentionally hardcoded visible example"}
            if scenario=="prohibited-path":candidate={"changes":[{"path":"tests/test_public.py","content":"pass\n"}],"explanation":"Intentionally attempts to modify a protected path"}
            if scenario=="cancel":candidate={"changes":[{"path":"candidate_app/api.py","content":"def handle(request):\n    while True: pass\n"}],"explanation":"Intentionally nonterminating candidate"}
            run_dir=state/"ea/0/serve"/run_id
            while record["status"] in ["starting","running","stopping"]:
                if time.monotonic()>=deadline:
                    request(url,f"/v1/runs/{run_id}/stop",{});raise TimeoutError("Runtime check replay exceeded 90 seconds")
                try:panel={} if stopped else request(url,f"/v1/runs/{run_id}/panel")
                except urllib.error.HTTPError as error:
                    if error.code==502:
                        # The invocation socket closes before the run record's
                        # final write. Confirm that transition, never treat an
                        # arbitrary panel failure as a successful scenario.
                        for _ in range(40):
                            record=request(url,f"/v1/runs/{run_id}")
                            if record["status"] not in ["starting","running","stopping"]:break
                            time.sleep(.05)
                        else:raise
                    elif error.code!=404:raise
                    panel={}
                for pending in panel.get("pending",[]):
                    if pending["invocation_id"] in answered:continue
                    index=int(pending["reaction"].rsplit(".",1)[1])
                    repair=candidate if scenario=="exhausted" else reference["oracle_patch"]
                    port,value={0:("flow.evidence","Scripted fixture context"),1:("flow.raw",json.dumps(candidate)),2:("flow.result",json.dumps(candidate)),3:("flow.repaired",json.dumps(repair)),4:("flow.result",json.dumps(repair))}[index]
                    request(url,f"/v1/runs/{run_id}/panel",{"invocation_id":pending["invocation_id"],"agent":pending["agent"],"values":{port:value}})
                    answered.add(pending["invocation_id"])
                if scenario=="cancel" and not stopped:
                    # Observe the owned runtime artifact before asking Docker;
                    # no other evaluation container may be active in this test.
                    if list(run_dir.glob("repository-checks/*/artifacts/candidate")):
                        names=command(["ps","--filter","label=omar.evaluation=repository-repair","--format","{{.Names}}"]).splitlines()
                        if names:
                            if len(names)!=1:raise RuntimeError("Cancellation test needs an otherwise idle evaluation runner")
                            active_container=names[0];request(url,f"/v1/runs/{run_id}/stop",{});stopped=True
                time.sleep(.05);record=request(url,f"/v1/runs/{run_id}")
            receipt=request(url,"/v1/workflow/receipts",{"run_id":run_id});dump(directory/"receipt.json",receipt)
            invocations=[json.loads(path.read_text()) for path in (run_dir/"invocations").glob("*.json")]
            accepted=[r["outputs"]["flow.result"] for r in invocations if r["status"]=="contract_satisfied" and "flow.result" in (r.get("outputs") or {})]
            expected_accept=scenario in ["oracle","hardcoded-then-oracle","prohibited-path"]
            if len(accepted)!=int(expected_accept):raise RuntimeError(f"Unexpected accepted output: {record}, {receipt}")
            statuses=[c["status"] for c in receipt["repository_checks"]]
            expected={"oracle":["passed"],"hardcoded-then-oracle":["failed","passed"],"exhausted":["failed","failed"],
                      "prohibited-path":["failed","passed"],"missing-image":["infrastructure_failure"],"cancel":["cancelled"]}[scenario]
            if sorted(statuses)!=sorted(expected):raise RuntimeError(f"Wrong regression receipts: {statuses}, {receipt}")
            if len(answered)!=(5 if scenario in ["hardcoded-then-oracle","exhausted","prohibited-path"] else 3):raise RuntimeError("Unexpected number of invocations")
            decisions=[v["decision"] for v in receipt["verification"]]
            if scenario in ["missing-image","cancel"] and "repair_requested" in decisions:raise RuntimeError("Infrastructure failure/cancellation triggered repair")
            if scenario=="exhausted" and "blocked" not in decisions:raise RuntimeError("Exhausted repair was not blocked")
            if active_container and active_container in command(["ps","--all","--format","{{.Names}}"]).splitlines():raise RuntimeError("Cancelled container leaked")
            if accepted:dump(directory/"accepted-output.json",accepted[0])
            reports.append({"scenario":scenario,"run_id":run_id,"runtime_status":record["status"],"invocations":len(answered),
                            "accepted_outputs":len(accepted),"repository_checks":statuses,"model_calls":0})
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
    p.add_argument("--scenario");p.add_argument("--omar",type=Path,default=REPO/"target/debug/omar")
    p.add_argument("--compiler",type=Path,default=REPO/"lang/.lake/build/bin/omarc")
    verify(p.parse_args())
