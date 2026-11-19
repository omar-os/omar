"""Materialize an exact allowed-file patch and grade the actual repository in Docker."""
import difflib
import hashlib
import json
from pathlib import Path, PurePosixPath
from repository_sandbox import InfrastructureError, run


def fingerprint(files):
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()


def safe_path(name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or ".." in path.parts or "." in path.parts
            or str(path) != name or "\\" in name or "\x00" in name or name.startswith(".git/")):
        raise ValueError("Invalid repository path")
    return path


def apply_patch(task, output, directory):
    if isinstance(output, str):
        output = json.loads(output)
    if not isinstance(output, dict) or set(output) != {"changes", "explanation"}:
        raise ValueError("Expected changes and explanation")
    if not isinstance(output["explanation"], str) or not output["explanation"].strip():
        raise ValueError("Patch explanation is required")
    files = task["files"]
    if fingerprint(files) != task["repository_digest"]:
        raise ValueError("Repository snapshot changed")
    if not isinstance(output["changes"], list) or not 1 <= len(output["changes"]) <= 16:
        raise ValueError("Expected 1–16 changed files")
    if not set(task["allowed_patch_paths"]) <= files.keys():
        raise ValueError("Patch allowlist must refer to supplied files")
    updated = dict(files)
    seen = set()
    for change in output["changes"]:
        if not isinstance(change, dict) or set(change) != {"path", "content"}:
            raise ValueError("Each change needs path and content")
        name = change["path"]
        safe_path(name)
        if name not in task["allowed_patch_paths"] or name in seen:
            raise ValueError("Duplicate or prohibited patch path")
        content = change["content"]
        if not isinstance(content, str) or len(content.encode()) > 65536 or "\x00" in content:
            raise ValueError("Patch content must be bounded UTF-8 text")
        seen.add(name)
        updated[name] = content
    if updated == files:
        raise ValueError("Patch makes no change")
    if len(updated) > 64 or sum(len(v.encode()) for v in updated.values()) > 262144:
        raise ValueError("Repository exceeds the fixture budget")
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=False)
    directory.chmod(0o755)
    for name, content in updated.items():
        path = directory.joinpath(*safe_path(name).parts)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        path.chmod(0o444)
    diff = "".join(line if line.endswith("\n") else line + "\n\\ No newline at end of file\n"
                   for name in sorted(seen)
                   for line in difflib.unified_diff(files[name].splitlines(keepends=True),
                                                   updated[name].splitlines(keepends=True),
                                                   fromfile="a/"+name, tofile="b/"+name))
    return {"before_digest":fingerprint(files),"after_digest":fingerprint(updated),"changed_files":sorted(seen),"diff":diff}


def grade_repository(task, reference, output, *, image, artifacts, timeout=15, cancel_path=None, entrypoint="candidate_app:handle"):
    artifacts = Path(artifacts)
    owned_artifacts = False
    result = {"deterministic_pass":False,"errors":[],"semantic_acceptance":None,"human_review_status":"pending",
              "label_review_status":reference["review_status"],"human_minutes":None,"substantive_edits":None,
              "evaluation_status":"candidate_failure","patch":None,"execution":None}
    try:
        artifacts.mkdir(parents=True, exist_ok=False)
        owned_artifacts = True
        patch = apply_patch(task, output, artifacts / "candidate")
        (artifacts / "candidate.patch").write_text(patch.pop("diff"))
        result["patch"] = patch
        tests = reference["protected_tests"]
        execution = run(artifacts / "candidate", [test["request"] for test in tests], image,
                        timeout=timeout, cancel_path=cancel_path, entrypoint=entrypoint)
        result["execution"] = execution
        if execution["stop_reason"] or execution["exit_code"] or execution["oom_killed"]:
            result["errors"].append(execution["stop_reason"] or "candidate_process_failed")
        else:
            lines = execution["stdout"].splitlines()
            if len(lines) != len(tests):
                result["errors"].append("unexpected_output_count")
            else:
                for line, test in zip(lines, tests):
                    observed = json.loads(line)
                    if not isinstance(observed, dict) or set(observed) != {"value", "input_unchanged"}:
                        result["errors"].append("invalid_result:"+test["id"])
                    elif json.dumps(observed["value"], sort_keys=True) != json.dumps(test["expected"], sort_keys=True):
                        result["errors"].append("protected_regression_failure:"+test["id"])
                    if isinstance(observed, dict) and observed.get("input_unchanged") is not True:
                        result["errors"].append("input_mutation:"+test["id"])
        result["deterministic_pass"] = not result["errors"]
        result["evaluation_status"] = "graded"
    except (InfrastructureError, OSError) as error:
        result["deterministic_pass"] = None
        result["evaluation_status"] = "infrastructure_failure"
        result["errors"].append(str(error))
    except (ValueError, TypeError, KeyError) as error:
        result["errors"].append("invalid_patch_or_output:"+str(error))
    if owned_artifacts:
        try:
            (artifacts / "grade.json").write_text(json.dumps(result, indent=2)+"\n")
        except OSError as error:
            result["deterministic_pass"] = None
            result["evaluation_status"] = "infrastructure_failure"
            result["errors"].append("Could not persist grade: " + str(error))
    return result
