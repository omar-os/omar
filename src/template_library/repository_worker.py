"""Trusted embedded runtime adapter. It never imports candidate code on the host."""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from repository_grade import fingerprint, grade_repository
from repository_sandbox import run


def reproduce(suite, timeout):
    """Run the unchanged snapshot; infrastructure failures are never reproduction."""
    directory = ROOT / "baseline"
    directory.mkdir()
    for name, text in suite["files"].items():
        path = directory.joinpath(*name.split("/"))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o444)
    tests = suite["protected_tests"]
    execution = run(directory, [test["request"] for test in tests], suite["image"],
                    timeout=timeout, cancel_path=ROOT / "cancel", entrypoint=suite["entrypoint"])
    (ROOT / "baseline-execution.json").write_text(json.dumps(execution))
    if execution["stop_reason"] or execution["exit_code"] or execution["oom_killed"]:
        raise RuntimeError("Baseline infrastructure or process failed; reproduction unconfirmed")
    lines = execution["stdout"].splitlines()
    if len(lines) != len(tests):
        raise RuntimeError("Baseline output count invalid; reproduction unconfirmed")
    observed = [json.loads(line) for line in lines]
    # The driver catches exceptions from the API, which count as a reproducible
    # defect; failure to start/import the driver itself does not.
    return any(value.get("error_type") or value.get("input_unchanged") is not True
               or json.dumps(value.get("value"), sort_keys=True) != json.dumps(test["expected"], sort_keys=True)
               for value, test in zip(observed, tests))


def main():
    request = json.loads((ROOT / "request.json").read_text())
    suite = request["suite"]
    reproduced = None
    if request.get("require_reproduction"):
        reproduced = reproduce(suite, request["timeout_ms"] / 1000)
        if not reproduced:
            (ROOT / "result.json").write_text(json.dumps({"status":"failed", "failure_kind":"original snapshot passes protected cases; could not reproduce", "baseline_failure_observed":False}))
            return
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
    (ROOT / "result.json").write_text(json.dumps({"status": status, "failure_kind": failure, "baseline_failure_observed":reproduced}))


if __name__ == "__main__":
    main()
