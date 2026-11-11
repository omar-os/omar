#!/usr/bin/env python3
"""A physical action takes events from outside into one running program.

Against a real session, `omarc` and a code reaction: events sent with `omar
schedule`, over HTTP and through `omar github-webhook` all reach the same run,
each tagged at its arrival and handled in that order, with the reaction's state
carried across them. Wrong types, unknown ports and unsigned deliveries are
refused, the run waits on an empty queue until stopped, and a stopped run
accepts nothing.
"""
import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()

PROGRAM = """\
team Intake {
    physical action pr_event : string
    output latest : string
    state count : int = 0

    reaction(pr_event) -> latest {=
        self.count += 1;
        latest = Some(format!("#{} {}", self.count, pr_event.unwrap_or_default()));
    =}
}

main PrIntake {
    intake = Intake()
}
"""


def http(method, url, body=None, headers=None, raw=None, timeout=10):
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    request = urllib.request.Request(url, data=data, method=method,
                                     headers={"content-type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-physical-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source = root / "home", root / "source"
        for path in (home, source):
            path.mkdir()
        # A code reaction is built with cargo, which a fresh HOME would hide.
        env = dict(os.environ, HOME=str(home),
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        (source / "PrIntake.omar").write_text(PROGRAM)
        secret = b"ci-webhook-secret"
        (root / "secret").write_bytes(secret)

        def omar(*args, check=True):
            result = subprocess.run([str(BIN), *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=300)
            if check:
                assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result

        webhook = None
        try:
            session = json.loads(omar("up", "--no-ea", "-s", "phys", "--json").stdout)
            full = session["url"]
            run_id = json.loads(omar("-s", "phys", "run", "PrIntake.omar").stdout)["run_id"]

            # Refused until the run's clock starts; then tagged in arrival order.
            end = time.monotonic() + 240
            while True:
                first = omar("-s", "phys", "schedule", run_id, "intake.pr_event", '{"pr":1}', check=False)
                if first.returncode == 0:
                    break
                assert "not accepting" in first.stderr or "not running" in first.stderr, first.stderr
                record = http("GET", f"{full}/v1/runs/{run_id}")[1]
                assert record["status"] in ("starting", "running"), record
                assert time.monotonic() < end, f"the run never accepted an event: {first.stderr}"
                time.sleep(0.5)
            tags = [json.loads(first.stdout)["tag"]]
            tags.append(json.loads(omar("-s", "phys", "schedule", run_id, "intake.pr_event", '{"pr":2}').stdout)["tag"])
            status, accepted = http("POST", f"{full}/v1/runs/{run_id}/actions/intake.pr_event", {"value": "via http"})
            assert status == 202, accepted
            tags.append(accepted["tag"])
            key = [(tag["timestamp"], tag["microstep"]) for tag in tags]
            assert key == sorted(key) and len(set(key)) == 3, tags

            assert http("POST", f"{full}/v1/runs/{run_id}/actions/intake.pr_event", {"value": 5})[0] == 400
            assert http("POST", f"{full}/v1/runs/{run_id}/actions/intake.latest", {"value": "x"})[0] == 404
            refused = omar("-s", "phys", "schedule", run_id, "intake.latest", "x", check=False)
            assert refused.returncode != 0 and "not a physical action" in refused.stderr, refused.stderr

            # The webhook listener prints where it bound before it serves.
            webhook = subprocess.Popen([str(BIN), "-s", "phys", "github-webhook", run_id, "intake.pr_event",
                                        "--listen", "127.0.0.1:0", "--secret-file", str(root / "secret")],
                                       cwd=source, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            line = webhook.stdout.readline()
            assert line.startswith("Listening for GitHub webhooks on "), line
            hook = "http://" + line.rsplit(" ", 1)[1].strip()
            payload = json.dumps({"action": "synchronize", "number": 359,
                                  "pull_request": {"number": 359, "head": {"sha": "0a3f7b3"}},
                                  "repository": {"full_name": "omar-os/omar"}, "sender": {"login": "dev"}}).encode()

            def deliver(event, body, key=secret):
                signature = "sha256=" + hmac.new(key, body, hashlib.sha256).hexdigest()
                return http("POST", hook, raw=body, headers={"X-GitHub-Event": event, "X-GitHub-Delivery": "ci-1",
                                                             "X-Hub-Signature-256": signature})

            assert deliver("ping", b'{"zen":"hi"}')[0] == 200
            assert deliver("pull_request", payload, key=b"forged")[0] == 401
            status, accepted = deliver("pull_request", payload)
            assert status == 202, accepted

            # An empty queue is not the end: the run is still there to hear more.
            time.sleep(2)
            assert http("GET", f"{full}/v1/runs/{run_id}")[1]["status"] == "running"
            omar("-s", "phys", "stop", run_id)
            end = time.monotonic() + 60
            while (record := http("GET", f"{full}/v1/runs/{run_id}")[1])["status"] != "stopped":
                assert time.monotonic() < end, record
                time.sleep(0.3)
            assert record["state"]["intake.count"] == 4, record
            number, summary = record["outputs"]["intake.latest"].split(" ", 1)
            assert number == "#4", record
            assert json.loads(summary) == {"event": "pull_request", "delivery": "ci-1", "action": "synchronize",
                                           "repo": "omar-os/omar", "pr": 359, "sha": "0a3f7b3", "sender": "dev"}
            late = omar("-s", "phys", "schedule", run_id, "intake.pr_event", "late", check=False)
            assert late.returncode != 0 and "not running" in late.stderr, late.stderr
            assert deliver("pull_request", payload)[0] == 503
            print("PASS: schedule, HTTP and a signed webhook feed one run in arrival order; "
                  "bad types, unknown ports and forged deliveries are refused; the run waits until stopped")
        finally:
            if webhook:
                webhook.kill()
            omar("rm", "-s", "phys", check=False)


if __name__ == "__main__":
    main()
