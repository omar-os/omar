#!/usr/bin/env python3
"""Real local delivery, busy admission, duplicate receipt and process restart."""
import hashlib
import hmac
import http.client
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", ROOT / "target/debug/omar")).resolve()
COMPILER = Path(os.environ.get("OMARC_BIN", ROOT / "lang/.lake/build/bin/omarc")).resolve()
PROGRAM = """team Gate[reviewer : Web] {
    input candidate : string
    output approved : bool
    task reviewer(candidate) -> approved "Review $(candidate)."
}
main WebhookReview { gate = Gate() }
"""


def main():
    with tempfile.TemporaryDirectory(prefix="omar-hook-") as directory:
        secret = secrets.token_hex(32)
        keys = Path(directory) / "webhook-secrets"
        keys.mkdir(mode=0o700)
        key = keys / "github-test"
        key.write_text(secret)
        key.chmod(0o600)
        env = dict(os.environ, OMAR_HOME=directory, OMARC_BIN=str(COMPILER))
        for key in ("OMAR_SESSION_ID", "OMAR_EA_ID", "OMAR_STATE_DIR", "TMUX", "TMUX_PANE"):
            env.pop(key, None)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]

        def call(method, route, data=None, headers=None):
            raw = json.dumps(data).encode() if data is not None else b""
            client = http.client.HTTPConnection("127.0.0.1", port, timeout=45)
            try:
                client.request(method, route, raw, headers or {})
                response = client.getresponse()
                return response.status, json.loads(response.read())
            finally:
                client.close()

        def ok(method, path, data=None):
            status, body = call(method, path, data)
            assert 200 <= status < 300, (path, status, body)
            return body

        def wait(predicate):
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                try:
                    value = predicate()
                    if value:
                        return value
                except (ConnectionError, OSError):
                    pass
                time.sleep(0.15)
            raise AssertionError(f"timed out waiting for {predicate}")

        def command(*args):
            return subprocess.run([str(BIN), *args], env=env, text=True, capture_output=True,
                                  timeout=45, check=True)
        def launch():
            command("up", "-s", "hook-test", "--port", str(port), "--no-ea")
            wait(lambda: call("GET", "/health")[0] == 200)

        launch()
        try:
            chat = ok("GET", "/v1/chat")["id"]
            base = "/chats/" + chat + "/v1/"
            checked = ok("POST", base+"programs/check", {"program":PROGRAM, "filename":"WebhookReview.omar"})
            assert checked["ok"], checked
            workspace = ok("POST", base+"workflow", {"revision":0, "name":"Webhook review",
                "draft":{"filename":"WebhookReview.omar","proposal_sequence":0,
                         "design":{"program":PROGRAM,"files":{},"inputs":{},"preview":checked["preview"]}}})
            settings = {"revision":workspace["revision"], "event":"push", "secret_name":"github-test",
                        "mapping":{"gate.candidate":"/ref"}}
            hook = ok("POST", base+"webhook-trigger", settings)["trigger"]["id"]
            route = base + "webhook-deliveries/" + hook
            payload = {"ref":"release-42"}

            def send(delivery, body=payload, signing_key=secret, event="push"):
                tag = hmac.new(signing_key.encode(), json.dumps(body).encode(), hashlib.sha256).hexdigest()
                return call("POST", route, body, {"X-Hub-Signature-256":"sha256="+tag,
                            "X-GitHub-Event":event, "X-GitHub-Delivery":delivery})

            def panel(run):
                status, value = call("GET", base+"runs/"+run+"/panel")
                return value.get("pending") if status == 200 else None

            def answer(run):
                task = wait(lambda:panel(run))[0]
                assert list(task["trigger_values"].values()) == ["release-42"], task
                ok("POST", base+"runs/"+run+"/panel", {"task_id":task["task_id"],"agent":task["agent"],
                                                     "values":{"gate.approved":True}})
                wait(lambda:ok("GET",base+"runs/"+run)["status"]=="completed")
                assert ok("GET",base+"runs/"+run)["outputs"]["gate.approved"] is True

            assert send("invalid", signing_key="wrong")[0] == 401
            assert send("mistyped", {"ref":123})[0] == 422
            assert send("missing", {})[0] == 422
            assert send("wrong-event", event="issues")[0] == 422
            first_status, first = send("A")
            assert first_status == 202, first
            wait(lambda:panel(first["run_id"]))
            status, retry = send("A")
            assert status == 200 and retry["run_id"] == first["run_id"]
            next_status, second = send("B")
            assert next_status == 202 and second["run_id"] != first["run_id"]
            assert ok("GET",base+"webhook-trigger")["deliveries"][0]["status"]=="queued"
            ok("POST",base+"webhook-trigger",{"enabled":False})
            assert send("paused")[0] == 409
            answer(first["run_id"])
            answer(second["run_id"])
            entries = ok("GET",base+"webhook-trigger")["deliveries"]
            assert len(entries)==2 and all(e["status"]=="started" for e in entries), entries
            command("down","-s","hook-test")
            launch()
            assert send("A")[1]["run_id"]==first["run_id"]
            assert send("B")[1]["run_id"]==second["run_id"]
            assert len(ok("GET",base+"webhook-trigger")["deliveries"])==2
            # A queued delivery carries its own pinned request through a restart.
            ok("POST",base+"webhook-trigger",settings)
            _, third = send("C")
            wait(lambda:panel(third["run_id"]))
            _, fourth = send("D")
            assert ok("GET",base+"webhook-trigger")["deliveries"][0]["status"]=="queued"
            command("down", "-s", "hook-test", "--force")
            launch()
            assert send("D")[1]["run_id"] == fourth["run_id"]
            status, record = call("GET", base+"runs/"+third["run_id"])
            if status==200 and record["status"]=="paused":
                command("stop", "-s", "hook-test", third["run_id"])
            answer(fourth["run_id"])
            assert len(ok("GET",base+"webhook-trigger")["deliveries"])==4
            print("A queued delivery resumed under its original ID. Invalid signature and invalid mappings refused. A, retry A, and B produced two typed runs. Receipts survived restart.")
        finally:
            command("down","-s","hook-test","--force")


if __name__=="__main__":
    main()
