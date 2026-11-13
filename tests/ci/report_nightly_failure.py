#!/usr/bin/env python3
"""report-nightly-failure.sh against a stand-in gh that serves canned GitHub
JSON through the script's own --jq filters: it comments on the one tracking
issue, reopens it only when closed, and never opens an issue."""
import json
import os
import subprocess
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / ".github" / "scripts" / "report-nightly-failure.sh"

FAKE_GH = r'''#!/usr/bin/env python3
import json, os, subprocess, sys
args = sys.argv[1:]
with open(os.environ["GH_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
canned = json.load(open(os.environ["GH_CANNED"]))
key = "api" if args[0] == "api" else " ".join(args[:2])
if key not in canned:
    sys.exit(0)
data = json.dumps(canned[key])
if "--jq" in args:
    data = subprocess.run(["jq", "-r", args[args.index("--jq") + 1]], input=data,
                          capture_output=True, text=True, check=True).stdout
sys.stdout.write(data)
'''

JOBS = {"jobs": [
    {"name": "Plan backends", "conclusion": "success"},
    {"name": "Live Topology (self-hosted)", "conclusion": "failure"},
    {"name": "Live Topology (cursor)", "conclusion": "success"},
    {"name": "Live Ultracode", "conclusion": "failure"},
    {"name": "Report nightly failure", "conclusion": None},
]}
ISSUES = [
    {"number": 3, "title": "Nightly Live Topology failures (old)"},
    {"number": 7, "title": "Nightly Live Topology failures"},
    {"number": 9, "title": "Nightly Live Topology failures"},
]


def report(tmp, canned):
    log = tmp / "gh.log"
    log.unlink(missing_ok=True)
    (tmp / "canned.json").write_text(json.dumps(canned))
    env = dict(os.environ, PATH=f"{tmp}:{os.environ['PATH']}", GH_LOG=str(log),
               GH_CANNED=str(tmp / "canned.json"), GH_REPO="omar-os/omar",
               GITHUB_SERVER_URL="https://github.com", GITHUB_RUN_ID="42", GITHUB_RUN_ATTEMPT="1")
    result = subprocess.run([str(SCRIPT)], env=env, capture_output=True, text=True)
    calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    return result, calls


def main():
    with tempfile.TemporaryDirectory(prefix="omar-nightly-report-") as folder:
        tmp = Path(folder)
        gh = tmp / "gh"
        gh.write_text(FAKE_GH)
        gh.chmod(0o755)

        # Closed: reopened, then one comment on the oldest exact-title issue.
        result, calls = report(tmp, {
            "issue list": ISSUES, "api": JOBS,
            "issue view": {"state": "CLOSED", "assignees": [{"login": "alice"}, {"login": "bob"}]},
        })
        assert result.returncode == 0, result
        verbs = [c[:2] for c in calls]
        assert ["issue", "reopen"] in verbs and verbs.index(["issue", "reopen"]) < verbs.index(["issue", "comment"]), calls
        reopen = next(c for c in calls if c[:2] == ["issue", "reopen"])
        comment = next(c for c in calls if c[:2] == ["issue", "comment"])
        assert reopen[2] == "7" and comment[2] == "7", calls
        body = comment[comment.index("--body") + 1]
        assert "https://github.com/omar-os/omar/actions/runs/42" in body, body
        assert "- Live Topology (self-hosted)" in body and "- Live Ultracode" in body, body
        assert "cursor" not in body and "Plan backends" not in body, body
        assert body.rstrip().endswith("@alice @bob"), body

        # Open: commented, not reopened.
        result, calls = report(tmp, {
            "issue list": ISSUES, "api": JOBS,
            "issue view": {"state": "OPEN", "assignees": []},
        })
        assert result.returncode == 0, result
        assert ["issue", "reopen"] not in [c[:2] for c in calls], calls
        assert any(c[:2] == ["issue", "comment"] for c in calls), calls

        # No tracking issue: fails loudly, opens and comments on nothing.
        result, calls = report(tmp, {"issue list": ISSUES[:1], "api": JOBS})
        assert result.returncode != 0 and "no issue titled" in result.stderr, result
        assert all(c[:2] in (["issue", "list"],) for c in calls), calls

    print("PASS: nightly failure report comments on the one tracking issue, reopens only when closed, never opens one")


if __name__ == "__main__":
    main()
