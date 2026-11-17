#!/usr/bin/env python3
"""`omar muse-app`: the Muse App answers a team's `MuseApp` agent.

Against a real session, the real compiler and a live run, this plays the Muse
App's side over HTTP. Without the token nothing is served. With it, /work
lists the `MuseApp` agent's task and not the `Web` agent's, which is for a
person. An answer of the wrong type is refused and the task stays open; a
right one completes it, and once the person answers through Mission Control's
panel the run finishes with the Muse App's value on its output.
"""
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
OMARC = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()

PROGRAM = """team Errands[concierge : MuseApp, person : Web] {
    input request : string
    output booking : string
    output note : string

    task concierge(request) -> booking
    "Book this: $(request). Set `booking` to the confirmation."

    task person(request) -> note
    "Add a note for the guests about: $(request)"
}

main Desk {
    e = Errands()
}
"""


def http(method, url, body=None, token=None):
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def wait(probe, label, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = probe()
        if value:
            return value
        time.sleep(0.3)
    raise AssertionError(label)


def main():
    assert shutil.which("tmux"), "tmux is required"
    with tempfile.TemporaryDirectory(prefix="omar-museapp-", dir="/tmp", ignore_cleanup_errors=True) as directory:
        root = Path(directory)
        home, source = root / "home", root / "source"
        home.mkdir()
        source.mkdir()
        env = dict(os.environ, HOME=str(home), OMARC_BIN=str(OMARC))
        for key in ("TMUX", "TMUX_PANE", "OMAR_TMUX_SERVER", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        (source / "Desk.omar").write_text(PROGRAM)

        def omar(*args, check=True):
            result = subprocess.run([str(BIN), *args], cwd=source, env=env, text=True,
                                    capture_output=True, timeout=180)
            if check:
                assert result.returncode == 0, f"{args}: {result.stdout}\n{result.stderr}"
            return result

        connector = None
        try:
            session = json.loads(omar("up", "--no-ea", "-s", "muse", "--json").stdout)
            full = session["url"]
            run = json.loads(omar("-s", "muse", "run", "Desk.omar",
                                  "--input", 'e.request="a table for 4, Friday 7pm"').stdout)
            run_id = run["run_id"]

            with socket.socket() as probe:
                probe.bind(("127.0.0.1", 0))
                port = probe.getsockname()[1]
            connector = subprocess.Popen([str(BIN), "-s", "muse", "muse-app", "--listen", f"127.0.0.1:{port}"],
                                         cwd=source, env=env, text=True, stdout=subprocess.PIPE,
                                         stderr=subprocess.STDOUT)
            setup = ""
            while "recurring task" not in setup:
                line = connector.stdout.readline()
                assert line, f"connector exited: {setup}"
                setup += line
            token = re.search(r"secret credential: (\S+)", setup).group(1)
            base = f"http://127.0.0.1:{port}"

            assert http("GET", f"{base}/work")[0] == 401
            assert http("GET", f"{base}/work", token="omar-wrong")[0] == 401
            assert http("GET", f"{base}/v1/runs", token=token)[0] == 404, "the session API must not leak"

            work = wait(lambda: http("GET", f"{base}/work", token=token)[1]["work"], "no work for the Muse App")
            assert len(work) == 1, work
            task = work[0]
            assert task["run_id"] == run_id and task["backend"] == "MuseApp", task
            assert task["agent"].endswith("concierge"), task
            assert "a table for 4, Friday 7pm" in task["prompt"], task
            assert task["allowed_effects"] == {"e.booking": "string"}, task

            answer = {"run_id": run_id, "task_id": task["task_id"], "agent": task["agent"]}
            wrong = http("POST", f"{base}/answer", {**answer, "values": {"e.booking": 7}}, token)
            assert wrong[0] >= 400, wrong
            assert http("GET", f"{base}/work", token=token)[1]["work"], "a refused answer closed the task"

            done = http("POST", f"{base}/answer", {**answer, "values": {"e.booking": "Confirmed: Nopa, Fri 7pm, 4"}}, token)
            assert done[0] == 202, done

            # Reactions of one team run in order, so the person's task comes
            # next. It is on the panel, and out of the Muse App's reach.
            panel = wait(lambda: [p for p in http("GET", f"{full}/v1/runs/{run_id}/panel")[1]["pending"]
                                  if p["backend"] == "Web"], "the person's task never reached the panel")
            person = panel[0]
            assert http("GET", f"{base}/work", token=token)[1]["work"] == []
            refused = http("POST", f"{base}/answer", {"run_id": run_id, "task_id": person["task_id"],
                                                      "agent": person["agent"], "values": {"e.note": "hi"}}, token)
            assert refused[0] == 404, refused

            noted = http("POST", f"{full}/v1/runs/{run_id}/panel",
                         {"task_id": person["task_id"], "agent": person["agent"], "values": {"e.note": "Bring ID"}})
            assert noted[0] == 202, noted
            record = wait(lambda: (lambda r: r if r.get("status") == "completed" else None)(
                http("GET", f"{full}/v1/runs/{run_id}")[1]), "the run never completed")
            assert "Confirmed: Nopa, Fri 7pm, 4" in json.dumps(record), record
            print("PASS: the Muse App connector serves only with its token, lists only the MuseApp agent's "
                  "task, refuses a wrong-typed answer and another agent's task, and its answer completes the run")
        finally:
            if connector:
                connector.kill()
            omar("rm", "-s", "muse", check=False)


if __name__ == "__main__":
    main()
