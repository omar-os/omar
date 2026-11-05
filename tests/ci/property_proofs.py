#!/usr/bin/env python3
"""Safety properties: the assistant's proofs, checked by Lean through serve.

A real `omar serve` (no assistant; this script holds its token) is asked, as
the assistant asks with `omar_check_properties`, to check the trading desk's
properties: both are proved, and a proof that is only `sorry` is not. A wrong
token is refused. Proposed with the properties, the design is published with
Lean's verdicts, which is what Mission Control shows.

The real compiler, Lean and the semantics library the session pins; no model.
"""
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
OMARC = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()
DESK = (REPO / "tests/verification/Desk.omar").read_text()
REQUEST = json.loads((REPO / "tests/verification/desk-properties.json").read_text())


def main():
    assert OMARC.exists(), f"{OMARC}: build the compiler with `cd lang && lake build`"
    with tempfile.TemporaryDirectory(prefix="omar-proofs-") as directory:
        home = Path(directory)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        # Lean comes through elan, which keeps its toolchains under the real home.
        env = dict(os.environ, HOME=str(home), OMARC_BIN=str(OMARC),
                   ELAN_HOME=os.environ.get("ELAN_HOME", str(Path.home() / ".elan")))
        for key in ("TMUX", "TMUX_PANE", "OMAR_SESSION_ID", "OMAR_STATE_DIR", "OMAR_HOME", "OMAR_EA_ID"):
            env.pop(key, None)
        log = open(home / "serve.log", "w")
        daemon = subprocess.Popen([str(BIN), "serve", "--no-ea", "-s", "proofs", "--port", str(port)],
                                  env=env, stdout=log, stderr=log)

        def call(method, path, body=None, timeout=330):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
            try:
                connection.request(method, path, body=json.dumps(body) if body is not None else None)
                response = connection.getresponse()
                return response.status, json.loads(response.read() or b"null")
            finally:
                connection.close()

        try:
            deadline = time.monotonic() + 60
            contexts = []
            while not contexts and time.monotonic() < deadline:
                contexts = list(home.glob(".omar/sessions/*/mcp/ea-0/context.json"))
                time.sleep(0.2)
            assert contexts, (home / "serve.log").read_text()
            token = json.loads(contexts[0].read_text())["serve"]["token"]
            while time.monotonic() < deadline:
                try:
                    if call("GET", "/health", timeout=2)[0] == 200:
                        break
                except OSError:
                    time.sleep(0.2)

            assumed = {"name": "assumed", "description": "Nothing ever executes.",
                       "statement": '∀ g, ¬ e.invoked g "x.reaction.0"', "proof": "sorry"}
            check = {"token": token, "program": DESK, "lemmas": REQUEST["lemmas"],
                     "properties": REQUEST["properties"] + [assumed]}
            status, answer = call("POST", "/v1/agent/properties", check)
            assert status == 200, answer
            verdicts = {verdict["name"]: verdict for verdict in answer["verdicts"]}
            for name in ("orders_are_approved", "rejected_not_executed"):
                assert verdicts[name]["proved"], verdicts[name]["problems"]
            assert verdicts["orders_are_approved"]["mentions"] == ["x.order", "r.approved"]
            assert not verdicts["assumed"]["proved"]
            assert "sorry" in verdicts["assumed"]["problems"][0]["message"]

            status, _ = call("POST", "/v1/agent/properties", dict(check, token="wrong"))
            assert status == 403, status

            proposal = {"token": token, "program": DESK, "summary": "A trading desk.",
                        "lemmas": REQUEST["lemmas"], "properties": REQUEST["properties"]}
            status, answer = call("POST", "/v1/agent/proposals", proposal)
            assert status == 202, answer
            _, chat = call("GET", "/v1/chat")
            design = [message["design"] for message in chat["messages"] if message.get("design")][-1]
            assert [verdict["proved"] for verdict in design["properties"]] == [True, True], design["properties"]
            assert "def topology" in design["properties"][0]["source"]
            print("properties proved through serve; an assumption was not")
        finally:
            daemon.terminate()
            daemon.wait(timeout=30)
            log.close()
            subprocess.run([str(BIN), "rm", "-s", "proofs"], env=env, capture_output=True)


if __name__ == "__main__":
    main()
