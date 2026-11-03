#!/usr/bin/env python3
"""Freeze a development diagnostic before execution; never calls a model."""
import argparse
import hashlib
import json
import os
import random
import re
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
CONFIGURATIONS = ("single_agent", "same_model_workflow", "routed_workflow")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def file_digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def read(path):
    return json.loads(Path(path).read_text())


def write(path, value):
    """Atomic host artifacts; interrupted writes cannot erase the last ledger."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False, allow_nan=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def contained(root, relative):
    path = Path(relative)
    if path.is_absolute() or not path.parts or any(p in (".", "..") for p in path.parts):
        raise ValueError("Artifact paths must be relative to their packet")
    resolved = (Path(root) / path).resolve()
    if not resolved.is_relative_to(Path(root).resolve()):
        raise ValueError("Artifact escapes its packet")
    return resolved


def task_paths(ids):
    found = {}
    for path in ROOT.glob("*/inputs/*.json"):
        task = read(path)
        if task["id"] in ids:
            if task["id"] in found:
                raise ValueError("Duplicate task ID")
            found[task["id"]] = path
    if set(found) != set(ids) or len(ids) != len(set(ids)):
        raise ValueError("Missing or duplicate selected task")
    return [found[task_id] for task_id in ids]


def restricted_registry(quality, economy, source=None):
    registry = read(source or REPO / "config/execution-profiles.example.json")
    selected = [p for p in registry["profiles"] if p["id"] in (quality, economy)]
    if len(selected) != 2 or quality == economy or any(p["backend"] != "codex" for p in selected):
        raise ValueError("This diagnostic requires two distinct configured Codex candidates")
    for profile in selected:
        profile["isolation"] = "ports_only"
        profile["tools"] = ["workflow_ports"]
        # No account observation or tool attestation transfers into this new
        # temporary scope. Candidate definitions are not live qualification.
        profile["access"] = "unknown"
        profile.pop("access_observation", None)
        profile["tool_capabilities"] = []
        profile.pop("repair", None)
    return {"schema_version": 1, "profiles": selected, "evidence": []}


def prepare(output, ids, *, quality="codex-quality", economy="codex-economy", image=None,
            omar=REPO / "target/debug/omar", compiler=REPO / "lang/.lake/build/bin/omarc",
            seed=20261102, repetitions=1, attempt_seconds=180, campaign_seconds=600):
    if not 1 <= repetitions <= 3 or not 30 <= attempt_seconds <= 600 or not 60 <= campaign_seconds <= 3600:
        raise ValueError("Diagnostic bounds: 1–3 repetitions, 30–600 seconds/attempt, 60–3600 seconds/campaign")
    paths = task_paths(ids)
    if not 1 <= len(paths) <= 10:
        raise ValueError("Select 1–10 development tasks explicitly")
    tasks = [(path, read(path)) for path in paths]
    if any(t["split"] != "development" or t["family"] not in ("document_synthesis", "repository_repair") for _, t in tasks):
        raise ValueError("This protocol supports development document/repository diagnostics only; holdout qualification needs a separate reviewed study")
    if any(t["family"] == "repository_repair" for _, t in tasks):
        if not isinstance(image, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise ValueError("Repository protocol needs an immutable already-installed image ID; preparation does not pull or probe it")
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    registry = restricted_registry(quality, economy)
    write(output / "assets/registry.json", registry)
    snapshots = []
    for path, task in tasks:
        reference = read(path.parent.parent / "references" / path.name)
        input_file, reference_file = f"assets/{task['id']}/input.json", f"assets/{task['id']}/reference.json"
        write(output / input_file, task)
        write(output / reference_file, reference)
        snapshots.append({"id":task["id"],"family":task["family"],"group":task["group"],"split":task["split"],
                          "input":input_file,"reference":reference_file,"input_digest":file_digest(output/input_file),
                          "reference_digest":file_digest(output/reference_file)})
    randomizer = random.Random(seed)
    schedule = []
    for repeat in range(1, repetitions + 1):
        order = list(snapshots)
        randomizer.shuffle(order)
        for task in order:
            configurations = list(CONFIGURATIONS)
            randomizer.shuffle(configurations)
            for configuration in configurations:
                schedule.append({"attempt_id":f"attempt-{len(schedule)+1:04}","task_id":task["id"],
                                 "configuration":configuration,"repeat":repeat})
    policy_source = (REPO / "src/workflow_design/profiles.rs").read_text()
    protocol = {
        "schema_version":1,"purpose":"development_diagnostic","created_at":int(time.time()),
        "quality":quality,"economy":economy,"registry":"assets/registry.json",
        "registry_digest":file_digest(output/"assets/registry.json"),"tasks":snapshots,"schedule":schedule,"seed":seed,
        "runtime_contract":re.search(r'RUNTIME_CONTRACT_REVISION: &str = "([^"]+)"',policy_source)[1],
        "routing_policy":re.search(r'ROUTING_POLICY_REVISION: &str = "([^"]+)"',policy_source)[1],
        "binaries":{name:{"path":str(Path(path).resolve()),"digest":file_digest(path)} for name,path in [("omar",omar),("compiler",compiler)]},
        "harness":{p.name:file_digest(p) for p in sorted(ROOT.glob("*.py"))},"repository_image":image,
        "resources":{"max_runs":len(schedule),"max_attempt_seconds":attempt_seconds,"max_campaign_seconds":campaign_seconds,
                     "max_step_seconds":90,"max_invocations_per_attempt":5,"access":"included_subscription_only",
                     "paid_api_allowed":False,"native_token_cap":None,"native_cost_cap":None,
                     "boundary":"Same inputs, scoped workflow ports, per-step and whole-attempt time caps; total compute parity is unverified."},
        "review":{"labels_required_before_execution":True,"output_review":"blinded human semantic and final-artifact acceptance",
                  "acceptance":"Completed delivered artifact, deterministic checks pass, human accepts without substantive edits or critical errors.",
                  "independence":"Reviewer attestation; this tooling cannot prove human identity or independence."},
        "decision_rule":{"mode":"diagnostic_only","superiority_established":False,"qualifies_routing":False,
                         "primary":"Accepted complete outcomes per attempted task, including execution failures in the denominator.",
                         "paired_unit":"Original independent task group; variants and repetitions do not create new groups.",
                         "missingness":"Show an observed lower/upper acceptance range; it is not a confidence interval.",
                         "future_thresholds_proposed":{"quality_gain_percentage_points":5,"maximum_human_minutes_increase":1,
                                                       "maximum_latency_ratio":1.2,"maximum_cost_ratio":1.2},
                         "future_confirmation_requires":"Independent reviewed groups, justified sample size and uncertainty method, frozen decision rule, resource coverage and new usage authorization. Proposed thresholds are not approved qualification."},
        "design_cost":{"measured":False,"reason":"Prepared workflow templates; interactive research/design is a separate required journey."},
    }
    protocol["digest"] = digest(protocol)
    write(output / "protocol.json", protocol)
    return protocol


def load(path, *, verify_runtime=True):
    path = Path(path).resolve()
    protocol = read(path)
    unsigned = {k:v for k,v in protocol.items() if k != "digest"}
    if protocol.get("schema_version") != 1 or protocol.get("purpose") != "development_diagnostic" or protocol.get("digest") != digest(unsigned):
        raise ValueError("Invalid or changed frozen diagnostic protocol")
    root = path.parent
    if file_digest(contained(root,protocol["registry"])) != protocol["registry_digest"]:
        raise ValueError("Frozen profile registry changed")
    tasks = {}
    for item in protocol["tasks"]:
        for field in ("input", "reference"):
            if file_digest(contained(root,item[field])) != item[field+"_digest"]:
                raise ValueError("Frozen task input/reference changed")
        task = read(contained(root,item["input"]))
        if any(task[k] != item[k] for k in ("id","family","group","split")) or item["id"] in tasks:
            raise ValueError("Frozen task metadata mismatch")
        tasks[item["id"]] = item
    attempts = set()
    for row in protocol["schedule"]:
        if row["task_id"] not in tasks or row["configuration"] not in CONFIGURATIONS or row["attempt_id"] in attempts:
            raise ValueError("Invalid diagnostic schedule")
        attempts.add(row["attempt_id"])
    if len(attempts) != protocol["resources"]["max_runs"]:
        raise ValueError("Diagnostic run budget differs from its schedule")
    if verify_runtime:
        for name, expected in protocol["harness"].items():
            if file_digest(contained(ROOT,name)) != expected:
                raise ValueError(f"Frozen harness changed: {name}; prepare/review a new protocol")
        for value in protocol["binaries"].values():
            if file_digest(value["path"]) != value["digest"]:
                raise ValueError("Frozen executable/compiler changed; prepare/review a new protocol")
    return protocol


def require_labels(protocol, record):
    unsigned = {k:v for k,v in record.items() if k != "digest"}
    if record.get("digest") != digest(unsigned) or record.get("kind") != "labels" or record.get("protocol_digest") != protocol["digest"]:
        raise ValueError("Label review does not bind this protocol")
    if record.get("independent_review_attested") is not True or not isinstance(record.get("reviewer"),str) or not record["reviewer"].strip():
        raise ValueError("A dated independent reviewer attestation is required")
    if type(record.get("reviewed_at")) is not int or not protocol["created_at"] <= record["reviewed_at"] <= time.time()+60:
        raise ValueError("Invalid label review date")
    reviews = {entry["task_id"]:entry for entry in record["entries"]}
    if len(reviews) != len(record["entries"]) or set(reviews) != {t["id"] for t in protocol["tasks"]}:
        raise ValueError("Label review must cover every selected task exactly once")
    for task in protocol["tasks"]:
        review = reviews[task["id"]]
        if review["verdict"] != "approved" or not isinstance(review.get("notes"),str) or not review["notes"].strip() or any(review.get(k) != task[k] for k in ("input_digest","reference_digest")):
            raise ValueError("All frozen labels must be approved before execution")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--task",action="append",required=True)
    parser.add_argument("--repository-image")
    parser.add_argument("--attempt-seconds",type=int,default=180)
    parser.add_argument("--campaign-seconds",type=int,default=600)
    parser.add_argument("--repetitions",type=int,default=1)
    args = parser.parse_args()
    result = prepare(args.output,args.task,image=args.repository_image,attempt_seconds=args.attempt_seconds,
                     campaign_seconds=args.campaign_seconds,repetitions=args.repetitions)
    print(json.dumps({"protocol":str(args.output/"protocol.json"),"digest":result["digest"],
                      "planned_runs":len(result["schedule"]),"model_calls":0,"execution_authorized":False}))
