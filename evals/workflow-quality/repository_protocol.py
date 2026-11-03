"""Owned diagnostic suite records; these are not independent label reviews."""
import hashlib
import json


def suite_for(task, reference, image):
    return {"schema_version":1,"id":"diagnostic-"+task["id"],"description":"Owned repository regressions; provisional labels",
            "reviewed_by":"automated fixture review, not independent acceptance","reviewed_at":1,
            "adapter":"python_json_api_v1","image":image,"entrypoint":task["entrypoint"],"files":task["files"],
            "allowed_patch_paths":task["allowed_patch_paths"],"timeout_ms":10000,"protected_tests":reference["protected_tests"]}


def check_for(suite):
    # Preparation preview only. Runtime obtains the authoritative digest from
    # the host catalog (JSON floating-point encoders need not be identical).
    digest=hashlib.sha256(json.dumps(suite,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode()).hexdigest()
    return {"kind":"repository_patch","port":"flow.result","verifier_id":suite["id"],
            "verifier_digest":digest,"repository_input":"flow.request"}


def bind_catalog(proposal, catalog):
    summaries={entry["id"]:entry for entry in catalog["repository_verifiers"]}
    for step in proposal["plan"]["steps"]:
        for check in step["output_checks"]:
            if check["kind"]=="repository_patch":
                check["verifier_digest"]=summaries[check["verifier_id"]]["digest"]
