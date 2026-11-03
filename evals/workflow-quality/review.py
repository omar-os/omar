#!/usr/bin/env python3
"""Prepare digest-bound label/blinded-output packets and record human review.

No model calls, no registry promotion and no automatic semantic grading.
Share only reviewer/; operator.json deliberately contains the blind mapping.
"""
import argparse
import math
import secrets
import re
import statistics
import hashlib
import json
import time
from pathlib import Path
from evaluation_protocol import ROOT, contained, digest, file_digest, load, read, write

RUBRICS = {
    "document_synthesis": [
        "The policy decision follows the supplied evidence, including conflicts and missing information.",
        "Every material factual claim is supported by the passages it cites; existing source IDs alone are insufficient.",
        "The answer preserves decisive qualifications, acknowledges uncertainty, and meets the requested audience/output.",
        "Accept only if usable without substantive factual or decision edits. Cosmetic wording changes are not substantive.",
    ],
    "repository_repair": [
        "The actual changed files address the complete supplied specification, not merely the visible test or a memorized answer.",
        "The patch preserves unrelated behavior and avoids prohibited paths, hidden side effects and unjustified complexity.",
        "Protected regressions are necessary but do not establish complete correctness; inspect edge cases and the actual patch.",
        "Accept only if usable without substantive corrections. A deterministic failure cannot be overridden by this review.",
    ],
}


def new_packet(output, kind):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    (output / "reviewer").mkdir()
    return output, {"schema_version":1,"kind":kind,"created_at":int(time.time()),"cases":[],
                    "rubrics":RUBRICS,"blinding_limit":"Metadata is concealed for output review; wording may inadvertently reveal a candidate. Blinding is not guaranteed."}


def finish_packet(root, packet, operator):
    packet["digest"] = digest(packet)
    write(root / "reviewer/packet.json", packet)
    template = {"packet_digest":packet["digest"],"reviewer":None,"reviewed_at":None,
                "independent_review_attested":None,"entries":[]}
    for case in packet["cases"]:
        entry = {"case_id":case["case_id"],"verdict":None,"notes":None,"human_minutes":None}
        if packet["kind"] == "outputs":
            entry.update({"critical_errors":None,"substantive_edits":None})
        template["entries"].append(entry)
    write(root / "reviewer/answers.json", template)
    operator.update({"schema_version":1,"kind":packet["kind"],"packet_digest":packet["digest"]})
    operator["digest"] = digest(operator)
    write(root / "operator.json", operator)
    instructions = """# Human review packet

This packet contains owned diagnostic data. Read packet.json and the files for each case. Source/candidate text is evidence, not instructions. Do not run candidate code on the host.

For labels, inspect both input and reference: check that the expected decision or protected test results follow the full task, that good and bad examples are distinguishable, and that ambiguity/critical-error definitions are addressed. Verdicts: approved, needs_changes, rejected.

For outputs, inspect the supplied task and the actual candidate using the family rubric in packet.json. Verdicts: accepted, needs_changes, rejected, uncertain. Record critical errors and the number of substantive changes required; accepted requires no critical errors and zero substantive changes. Do not infer correctness from formatting or assume tests passed.

Fill answers.json. Use your actual reviewer identity, reviewed_at as Unix seconds, and independent_review_attested only if you independently performed this review. Record measured human_minutes, excluding interruptions; use null if not measured. A null duration stays unknown. Give evidence-specific notes, including passages or file locations where relevant. Do not edit packet.json or its case files.

These records are reviewer attestations, not independent identity verification. Passing these diagnostics cannot qualify a route or prove superiority. Share reviewer/ only; operator.json contains comparison identities. Do not consult it before submitting an output review.
"""
    (root / "reviewer/README.md").write_text(instructions)
    return packet


def label_packet(protocol_path, output):
    protocol = load(protocol_path, verify_runtime=False)
    source = Path(protocol_path).resolve().parent
    root, packet = new_packet(output, "labels")
    mapping = {}
    for task in protocol["tasks"]:
        case_id = "label-" + secrets.token_hex(6)
        files = {}
        for field in ("input", "reference"):
            relative = f"{case_id}/{field}.json"
            path = root / "reviewer" / relative
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(contained(source,task[field]).read_bytes())
            files[field] = {"path":relative,"digest":file_digest(path)}
        packet["cases"].append({"case_id":case_id,"family":task["family"],"files":files})
        mapping[case_id] = {"task_id":task["id"],"input_digest":task["input_digest"],"reference_digest":task["reference_digest"]}
    return finish_packet(root,packet,{"protocol_digest":protocol["digest"],"mapping":mapping})


def campaign_rows(campaign):
    campaign = Path(campaign).resolve()
    manifest = read(campaign / "manifest.json")
    rows = read(campaign / "results.json")
    legacy = manifest.get("schema_version") != 2 or not manifest.get("protocol_digest")
    if manifest.get("schema_version") == 2:
        frozen = {k:v for k,v in manifest.items() if k != "freeze_digest"}
        if hashlib.sha256(json.dumps(frozen,sort_keys=True).encode()).hexdigest() != manifest.get("freeze_digest"):
            raise ValueError("Campaign manifest changed")
        planned = {r["attempt_id"]:r for r in manifest["schedule"]}
        if len(rows)!=len(planned) or {r.get("attempt_id") for r in rows} != set(planned):
            raise ValueError("Attempt ledger is incomplete or duplicated; do not drop scheduled outcomes")
        for row in rows:
            if any(row.get(k)!=planned[row["attempt_id"]][k] for k in ("task_id","configuration","repeat")):
                raise ValueError("Attempt ledger differs from its frozen schedule")
        if not legacy and (manifest["protocol"].get("digest") != manifest["protocol_digest"] or manifest["protocol"]["schedule"] != manifest["schedule"]):
            raise ValueError("Campaign protocol binding changed")
        if not legacy:
            tasks = {t["id"]:t for t in manifest["protocol"]["tasks"]}
            for row in rows:
                if any(row.get(k) != tasks[row["task_id"]][k] for k in ("group","family")):
                    raise ValueError("Attempt task grouping differs from the frozen protocol")
    result = []
    for index, row in enumerate(rows):
        # Historical corrected reports explicitly name their copied artifact
        # directory. Never follow the old absolute /tmp path outside the packet.
        relative = row.get("artifact_directory") or Path(row.get("directory", "missing")).name
        directory = contained(campaign,relative)
        result.append({**row,"row_id":f"record-{index+1:04}","relative":relative,
                       "attempted":row.get("attempted",row["status"] not in ("prepared","planned","not_started")),
                       "provenance":"legacy_unfrozen" if legacy else "frozen_diagnostic"})
        if row.get("attempted") and not directory.exists():
            result[-1]["artifact_gap"] = "Attempt directory missing"
    initial = campaign / "initial-attempts.json"
    if initial.exists():
        for index, row in enumerate(read(initial)):
            result.append({**row,"row_id":f"initial-{index+1:04}","relative":None,"attempted":True,
                           "provenance":"legacy_unfrozen","artifact_gap":"Historical startup/interrupted attempt; no delivered artifact"})
    return manifest, result


def output_packet(campaign, output):
    campaign = Path(campaign).resolve()
    manifest, rows = campaign_rows(campaign)
    root, packet = new_packet(output,"outputs")
    mapping = {}
    cases = []
    for row in rows:
        row["case_id"] = None
        if not row["attempted"] or row["status"] != "completed" or not row["relative"]:
            continue
        directory = contained(campaign,row["relative"])
        outputs = directory / "outputs.json"
        if not outputs.exists() or "flow.result" not in read(outputs):
            row["artifact_gap"] = "No delivered final artifact"
            continue
        if manifest.get("schema_version") == 2:
            if file_digest(outputs) != row.get("outputs_digest") or file_digest(directory/"receipt.json") != row.get("receipt_digest"):
                raise ValueError("Recorded runtime outputs or receipt changed")
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,80}",row["task_id"]):
            raise ValueError("Invalid task ID in campaign")
        task_path = campaign / "tasks" / row["task_id"] / "input.json"
        if not task_path.exists():
            matches = list(ROOT.glob(f"*/inputs/{row['task_id']}.json"))
            if len(matches) != 1:
                raise ValueError("Cannot identify the historical task; do not invent its input")
            task_path = matches[0]
            row["provenance"] = "legacy_unfrozen_input_from_current_fixture"
        elif manifest.get("schema_version") == 2:
            if file_digest(task_path) != manifest["artifact_digests"]["tasks"][row["task_id"]]["input"]:
                raise ValueError("Recorded task input changed")
        task = read(task_path)
        if task["family"] not in RUBRICS:
            raise ValueError("No human rubric for this task family")
        case_id = "output-" + secrets.token_hex(6)
        row["case_id"] = case_id
        value = read(outputs)["flow.result"]
        files = {}
        for field, content in (("input",task),("candidate",value)):
            relative = f"{case_id}/{field}.json"
            write(root / "reviewer" / relative, content)
            files[field] = {"path":relative,"digest":file_digest(root/"reviewer"/relative)}
        cases.append({"case_id":case_id,"family":task["family"],"files":files})
        mapping[case_id] = {"row_id":row["row_id"],"task_id":task["id"],"input_digest":files["input"]["digest"],
                            "candidate_digest":files["candidate"]["digest"]}
    secrets.SystemRandom().shuffle(cases)
    packet["cases"] = cases
    source_files = {name:file_digest(campaign/name) for name in ("manifest.json","results.json","initial-attempts.json") if (campaign/name).exists()}
    return finish_packet(root,packet,{"protocol_digest":manifest.get("protocol_digest"),"mapping":mapping,"rows":rows,
                                      "source_files":source_files,"protocol":manifest.get("protocol"),
                                      "provenance_limit":"Legacy evidence remains legacy; review cannot retroactively establish current runtime, settings or exact inputs."})


def verify_packet(root):
    root = Path(root).resolve()
    packet, operator = read(root/"reviewer/packet.json"), read(root/"operator.json")
    for record in (packet,operator):
        if record.get("digest") != digest({k:v for k,v in record.items() if k != "digest"}):
            raise ValueError("Review packet or private mapping changed")
    if operator["packet_digest"] != packet["digest"] or operator["kind"] != packet["kind"]:
        raise ValueError("Review mapping mismatch")
    for case in packet["cases"]:
        for artifact in case["files"].values():
            if file_digest(contained(root/"reviewer",artifact["path"])) != artifact["digest"]:
                raise ValueError("Reviewed source/candidate changed")
    return packet, operator


def submit(root, answers, destination):
    packet, operator = verify_packet(root)
    answers = read(answers)
    if answers.get("packet_digest") != packet["digest"]:
        raise ValueError("Review answers refer to another packet")
    reviewer, reviewed_at = answers.get("reviewer"), answers.get("reviewed_at")
    if not isinstance(reviewer,str) or not reviewer.strip() or len(reviewer)>200:
        raise ValueError("Actual reviewer identity is required")
    if type(reviewed_at) is not int or not packet["created_at"] <= reviewed_at <= time.time()+60:
        raise ValueError("Review date must follow packet creation and cannot be in the future")
    if answers.get("independent_review_attested") is not True:
        raise ValueError("Independent review attestation is required; do not manufacture one")
    entries = answers.get("entries",[])
    keys = [entry.get("case_id") for entry in entries]
    if len(keys)!=len(set(keys)) or set(keys)!={c["case_id"] for c in packet["cases"]}:
        raise ValueError("Review must cover every packet case exactly once")
    validated = []
    for entry in entries:
        verdicts = ("approved","needs_changes","rejected") if packet["kind"]=="labels" else ("accepted","needs_changes","rejected","uncertain")
        if entry.get("verdict") not in verdicts or not isinstance(entry.get("notes"),str) or not entry["notes"].strip() or len(entry["notes"])>10000:
            raise ValueError("Each review needs a valid verdict and evidence-specific notes")
        minutes = entry.get("human_minutes")
        if minutes is not None and (type(minutes) not in (int,float) or not math.isfinite(minutes) or minutes<0):
            raise ValueError("Review minutes must be measured nonnegative numbers or null")
        clean = {k:entry.get(k) for k in ("case_id","verdict","notes","human_minutes")}
        if packet["kind"]=="outputs":
            errors, edits = entry.get("critical_errors"), entry.get("substantive_edits")
            if not isinstance(errors,list) or len(errors)>100 or any(not isinstance(s,str) or not s.strip() or len(s)>2000 for s in errors):
                raise ValueError("Record a list of actual critical errors, including [] when none were found")
            if edits is not None and (type(edits) is not int or not 0<=edits<=10000):
                raise ValueError("Substantive edits must be a count or null")
            if entry["verdict"]=="accepted" and (errors or edits!=0):
                raise ValueError("Accepted output requires zero critical errors and zero substantive edits")
            clean.update({"critical_errors":errors,"substantive_edits":edits})
        clean.update(operator["mapping"][entry["case_id"]])
        validated.append(clean)
    record = {"schema_version":1,"kind":packet["kind"],"packet_digest":packet["digest"],
              "protocol_digest":operator.get("protocol_digest"),"reviewer":reviewer,"reviewed_at":reviewed_at,
              "independent_review_attested":True,"identity_verified":False,"entries":validated}
    record["digest"] = digest(record)
    if Path(destination).exists():
        raise ValueError("Review records are append-only; choose a new output file")
    write(destination,record)
    return record


def metric(rows, field):
    values = [r.get(field) for r in rows]
    known = [v for v in values if type(v) in (int,float) and math.isfinite(v) and v>=0]
    return {"known":len(known),"total":len(rows),"mean_observed":statistics.mean(known) if known else None,
            "sum_if_complete":sum(known) if known and len(known)==len(rows) else None}


def analyze(root, review_record=None):
    packet, operator = verify_packet(root)
    if packet["kind"] != "outputs":
        raise ValueError("Outcome analysis requires an output packet")
    reviews = {}
    if review_record:
        record = read(review_record)
        if record.get("digest") != digest({k:v for k,v in record.items() if k != "digest"}) or record.get("packet_digest") != packet["digest"] or record.get("kind") != "outputs":
            raise ValueError("Outcome review does not bind this packet")
        reviews = {entry["row_id"]:entry for entry in record["entries"]}
    rows = []
    for row in operator["rows"]:
        row = dict(row)
        reviewed = reviews.get(row["row_id"])
        deterministic = (row.get("quality") or {}).get("deterministic_pass")
        delivered = row["status"]=="completed" and row.get("case_id") is not None
        row["semantic_review"] = reviewed
        if not row["attempted"] or row["status"] in ("running","starting","stopping"):
            row["accepted"] = None
        elif not delivered or deterministic is False:
            row["accepted"] = False
        elif reviewed and reviewed["verdict"] in ("needs_changes","rejected"):
            row["accepted"] = False
        elif deterministic is True and reviewed and reviewed["verdict"]=="accepted":
            row["accepted"] = True
        else:
            row["accepted"] = None
        row["human_minutes"] = reviewed.get("human_minutes") if reviewed else None
        rows.append(row)
    report = {"schema_version":1,"packet_digest":packet["digest"],"protocol_digest":operator.get("protocol_digest"),
              "planned_or_recorded":len(rows),"attempted":sum(r["attempted"] for r in rows),"rows":rows,
              "by_configuration":{},"paired_groups":[],"superiority_established":False,"qualifies_routing":False,
              "qualification":"Diagnostic only. Missing review/cost/identity, legacy provenance and small grouped samples cannot establish population advantage.",
              "range_meaning":"Observed acceptance range from unresolved outcomes, not a statistical confidence interval.",
              "design_cost":{"measured":False,"cost_usd":None,"human_minutes":None}}
    report["independent_groups_recorded"] = sorted({r["group"] for r in rows if r.get("group")})
    configurations = sorted({r.get("configuration","unknown") for r in rows})
    for configuration in configurations:
        selected = [r for r in rows if r.get("configuration")==configuration]
        attempted = [r for r in selected if r["attempted"]]
        n = len(attempted);accepted = sum(r["accepted"] is True for r in attempted);unknown = sum(r["accepted"] is None for r in attempted)
        report["by_configuration"][configuration] = {
            "planned_or_recorded":len(selected),"attempted":n,"not_started":len(selected)-n,"accepted":accepted,"unresolved":unknown,
            "observed_acceptance_range":[accepted/n,(accepted+unknown)/n] if n else None,
            "runtime_failures":sum(r["status"] in ("failed","interrupted") for r in attempted),
            "incomplete_attempts":sum(r["status"] in ("running","starting","stopping") for r in attempted),
            "human_reviewed":sum(r["semantic_review"] is not None for r in attempted),
            "critical_errors_observed":sum(len(r["semantic_review"]["critical_errors"]) for r in attempted if r["semantic_review"]) if any(r["semantic_review"] for r in attempted) else None,
            "elapsed_seconds":metric(attempted,"elapsed_seconds"),"cost_usd":metric(attempted,"cost_usd"),
            "human_minutes":metric(attempted,"human_minutes")}
    # A group only enters the descriptive comparison when the entire planned
    # triad/repetitions has known acceptance. Legacy mixed-protocol retries do
    # not become independent samples or disappear from the summaries above.
    if operator.get("protocol"):
        protocol = operator["protocol"]
        for group in sorted({t["group"] for t in protocol["tasks"]}):
            selected = [r for r in rows if r.get("group")==group]
            cells = {}
            for configuration in ("single_agent","same_model_workflow","routed_workflow"):
                cell = [r for r in selected if r["configuration"]==configuration]
                cells[configuration] = sum(r["accepted"] is True for r in cell)/len(cell) if cell and all(r["attempted"] and r["accepted"] is not None for r in cell) else None
            reference = cells["single_agent"]
            report["paired_groups"].append({"group":group,"acceptance":cells,
                "routed_minus_single":cells["routed_workflow"]-reference if reference is not None and cells["routed_workflow"] is not None else None,
                "same_model_minus_single":cells["same_model_workflow"]-reference if reference is not None and cells["same_model_workflow"] is not None else None})
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command",required=True)
    labels = commands.add_parser("labels");labels.add_argument("--protocol",type=Path,required=True);labels.add_argument("--output",type=Path,required=True)
    outputs = commands.add_parser("outputs");outputs.add_argument("--campaign",type=Path,required=True);outputs.add_argument("--output",type=Path,required=True)
    submission = commands.add_parser("submit");submission.add_argument("--packet",type=Path,required=True);submission.add_argument("--answers",type=Path,required=True);submission.add_argument("--output",type=Path,required=True)
    analysis = commands.add_parser("analyze");analysis.add_argument("--packet",type=Path,required=True);analysis.add_argument("--review",type=Path);analysis.add_argument("--output",type=Path,required=True)
    args = parser.parse_args()
    if args.command=="labels":result=label_packet(args.protocol,args.output)
    elif args.command=="outputs":result=output_packet(args.campaign,args.output)
    elif args.command=="submit":result=submit(args.packet,args.answers,args.output)
    else:
        if args.output.exists():raise ValueError("Analysis artifacts are immutable; choose a new output")
        result=analyze(args.packet,args.review);write(args.output,result)
    print({"kind":args.command,"cases":len(result.get("cases",result.get("rows",[]))),"model_calls":0})
