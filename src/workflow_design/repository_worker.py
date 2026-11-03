"""Trusted embedded runtime adapter. It never imports candidate code on the host."""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from repository_grade import fingerprint, grade_repository


def main():
    request = json.loads((ROOT / "request.json").read_text())
    suite = request["suite"]
    task = {"files": suite["files"], "allowed_patch_paths": suite["allowed_patch_paths"],
            "repository_digest": fingerprint(suite["files"])}
    reference = {"protected_tests": suite["protected_tests"], "review_status": "operator_reviewed"}
    grade = grade_repository(task, reference, request["candidate"], image=suite["image"],
                            artifacts=ROOT / "artifacts", timeout=request["timeout_ms"] / 1000,
                            cancel_path=ROOT / "cancel", entrypoint=suite["entrypoint"])
    passed = grade["deterministic_pass"]
    # Private grader diagnostics can include candidate stdout. Only coarse
    # categories cross back into model-visible repair messages and receipts.
    status = "passed" if passed is True else "failed" if passed is False else "infrastructure_failure"
    failure = "invalid patch or response" if grade["evaluation_status"] == "candidate_failure" else "protected regressions did not pass"
    (ROOT / "result.json").write_text(json.dumps({"status": status, "failure_kind": failure}))


if __name__ == "__main__":
    main()
