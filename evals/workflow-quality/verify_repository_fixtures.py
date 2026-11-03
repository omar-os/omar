#!/usr/bin/env python3
"""Verify authored broken/oracle repositories; no model runs or quality qualification."""
import argparse
import hashlib
import json
from pathlib import Path
from repository_grade import grade_repository
from repository_sandbox import resolve_image, run

ROOT=Path(__file__).resolve().parent


def verify(output, image, include_reserved=False):
    output=output.resolve();output.mkdir(parents=True,exist_ok=False)
    image=resolve_image(image)
    selected=[p for p in sorted((ROOT/"repository-repair/inputs").glob("*.json"))
              if include_reserved or json.loads(p.read_text())["split"]!="reserved"]
    manifest={"image":image,"model_calls":0,"purpose":"Authored fixture/grader sanity, not independent label review or model evaluation",
              "harness_digests":{name:hashlib.sha256((ROOT/name).read_bytes()).hexdigest() for name in ["repository_fixtures.py","repository_grade.py","repository_sandbox.py","verify_repository_fixtures.py"]},
              "case_digests":{p.stem:{"input":hashlib.sha256(p.read_bytes()).hexdigest(),"reference":hashlib.sha256((ROOT/"repository-repair/references"/p.name).read_bytes()).hexdigest()} for p in selected}}
    (output/"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
    rows=[]
    for path in selected:
        task=json.loads(path.read_text())
        if task["split"]=="reserved" and not include_reserved:continue
        reference=json.loads((ROOT/"repository-repair/references"/path.name).read_text())
        directory=output/task["id"];directory.mkdir()
        baseline=directory/"baseline";baseline.mkdir();baseline.chmod(0o755)
        for name,content in task["files"].items():
            file=baseline/name;file.parent.mkdir(parents=True,exist_ok=True);file.write_text(content);file.chmod(0o444)
        execution=run(baseline,[test["request"] for test in reference["protected_tests"]],image)
        (directory/"baseline-execution.json").write_text(json.dumps(execution,indent=2)+"\n")
        failed=[]
        if execution["exit_code"] or execution["stop_reason"]:
            raise RuntimeError(f"Baseline failed to execute for {task['id']}; inspect artifacts")
        observed=[json.loads(line) for line in execution["stdout"].splitlines()]
        if len(observed)!=len(reference["protected_tests"]):raise RuntimeError("Unexpected baseline response count")
        for actual,test in zip(observed,reference["protected_tests"]):
            if json.dumps(actual.get("value"),sort_keys=True)!=json.dumps(test["expected"],sort_keys=True):failed.append(test["id"])
        if not failed:raise RuntimeError("Authored defect did not reproduce: "+task["id"])
        repaired=grade_repository(task,reference,reference["oracle_patch"],image=image,artifacts=directory/"oracle")
        row={"task_id":task["id"],"group":task["group"],"split":task["split"],"baseline_failures":failed,"oracle_pass":repaired["deterministic_pass"],"patch":repaired["patch"],"label_review_status":"provisional"}
        rows.append(row)
        (output/"report.json").write_text(json.dumps({"image":image,"purpose":"Authored fixture/grader sanity only; no model results or independent labels","results":rows,"model_calls":0},indent=2)+"\n")
        print(json.dumps({"task_id":task["id"],"baseline_failures":len(failed),"oracle_pass":repaired["deterministic_pass"]}),flush=True)
        if repaired["deterministic_pass"] is not True:raise RuntimeError("Oracle failed: "+task["id"])


if __name__=="__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image",required=True,help="Already-installed image; never pulled")
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--include-reserved",action="store_true",help="Validate fixture infrastructure only; do not inspect reserved model results while tuning")
    args=parser.parse_args();verify(args.output,args.image,args.include_reserved)
