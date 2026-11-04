#!/usr/bin/env python3
"""Small document smoke comparison. No model calls, ranking or routing qualification.

Supply a JSON list of {configuration: 'single_agent'|'workflow', case_id,
output: {...}, human_accepted: true|false|null}. Missing cases count as failures.
Give models only instruction/sources, not expected_decision. Use the same inputs
and a declared resource budget for both configurations. A reviewer must assess
whether claims are actually supported; citation IDs alone cannot establish that.
"""
import argparse
import json
from pathlib import Path

CASES = json.loads(Path(__file__).with_name("examples.json").read_text())

def check(case, output):
    if not isinstance(output, dict):
        return False
    claims = output.get("claims", [])
    ids = {s["id"] for s in case["sources"]}
    return (output.get("decision") == case["expected_decision"]
        and isinstance(output.get("recommendation"), str) and bool(output["recommendation"].strip())
        and isinstance(claims, list) and bool(claims)
        and all(isinstance(c, dict) and isinstance(c.get("text"), str) and c["text"].strip()
            and isinstance(c.get("source_ids"), list) and bool(c["source_ids"])
            and all(isinstance(i, str) and i in ids for i in c["source_ids"]) for c in claims)
        and isinstance(output.get("limitations"), list)
        and all(isinstance(v, str) for v in output["limitations"]))

def compare(records):
    indexed = {}
    for r in records:
        key = (r["configuration"], r["case_id"])
        if key in indexed or key[0] not in ("single_agent", "workflow") or key[1] not in {c["id"] for c in CASES}:
            raise ValueError("Duplicate or unknown configuration/case")
        if r.get("human_accepted") is not None and type(r["human_accepted"]) is not bool:
            raise ValueError("Human acceptance must be true, false or null")
        indexed[key] = r
    rows = []
    for config in ("single_agent", "workflow"):
        for case in CASES:
            r = indexed.get((config, case["id"]), {})
            passed = bool(check(case, r.get("output")))
            rows.append(dict(configuration=config, case_id=case["id"], deterministic_pass=passed,
                human_accepted=r.get("human_accepted"), accepted=passed and r.get("human_accepted") is True,
                missing=not bool(r)))
    return {"purpose":"Three illustrative smoke cases; labels are authored and unqualified. Not a statistical benchmark.",
        "rows":rows, "summary":{c:{"total":len(CASES), "deterministic_passes":sum(r["deterministic_pass"] for r in rows if r["configuration"]==c),
        "reviewed_acceptances":sum(r["accepted"] for r in rows if r["configuration"]==c)} for c in ("single_agent","workflow")}}

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    print(json.dumps(compare(json.loads(args.results.read_text())), indent=2))
