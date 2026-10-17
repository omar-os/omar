"""Exercise the registered Jev backend through OMAR, with a fake curl client."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
PROGRAM = '''type Decision from "./schemas/decision.json"
team Review[reviewer : Jev] {
    input request : string
    output out : Decision
    output evidence : string
    task reviewer(request) -> out, evidence "Choose continue, stop, or defer."
}
team Followup[worker : Stub] {
    input decision : Decision
    input evidence : string
    output note : string
    task worker(decision, evidence) -> note "Read the checked decision and evidence."
}
main JevBackendTest {
    review = Review()
    followup = Followup()
    review.out -> followup.decision
    review.evidence -> followup.evidence
}
'''


class JevBackendTests(unittest.TestCase):
    def test_configuration_failures_deadlines_and_evidence(self):
        # kind=None means a valid answer. Otherwise check safe typed failure evidence.
        cases = [
            ("success", "fail", None),
            ("invalid", "defer", "invalid_response"),
            ("non_json", "defer", "invalid_response"),
            ("bad_probabilities", "defer", "invalid_response"),
            ("authentication", "defer", "authentication"),
            ("rate_limit", "defer", "rate_limit"),
            ("service", "defer", "service"),
            ("http", "defer", "http"),
            ("transport", "defer", "transport"),
            ("timeout", "defer", "timeout"),
            ("blocked_input", "defer", "timeout"),
            ("oversize", "defer", "invalid_response"),
            ("missing_key", "defer", "configuration"),
            ("missing_key", "default", "configuration"),
            ("invalid", "default", "invalid_response"),
        ]
        for mode, policy, kind in cases:
            with self.subTest(mode=mode, policy=policy), tempfile.TemporaryDirectory() as temp:
                temp = Path(temp)
                (temp / "schemas").mkdir()
                (temp / "schemas/decision.json").write_text(json.dumps({"type":"string", "enum":["continue","stop","defer"]}))
                program = PROGRAM
                if mode in ("timeout", "blocked_input"):
                    program = program.replace('-> out, evidence "', '-> out, evidence within(250ms) "')
                (temp / "workflow.omar").write_text(program)
                response = {"model":"test-jev", "answers":{"decision":{"type":"choice",
                    "choice":"not_in_schema" if mode == "invalid" else "continue", "confidence":.97,
                    "probabilities":{"continue":.98,"stop":.01,"defer":.01}}}}
                if mode == "bad_probabilities":
                    response["answers"]["decision"]["probabilities"]["continue"] = .2
                status = {"authentication":401, "rate_limit":429, "service":503, "http":400}.get(mode, 200)
                provider_body = "sensitive-provider-body" if status != 200 else json.dumps(response)
                if mode == "non_json": provider_body = "not-json"
                if mode == "oversize": provider_body = "x" * (1024 * 1024 + 128)
                curl = temp / "curl"
                before = "time.sleep(2)\n" if mode == "blocked_input" else ""
                after = "time.sleep(2)\n" if mode == "timeout" else ""
                curl.write_text("#!/usr/bin/env python3\nimport json,sys,time,os\nfrom pathlib import Path\n"
                    + "Path(" + repr(str(temp / "pid")) + ").write_text(str(os.getpid()))\n" + before
                    + "config=sys.stdin.read()\n"
                    "assert not any('fixture-not-a-real-key' in arg for arg in sys.argv)\n"
                    "assert sys.argv[1]=='-q'\n"
                    "line=next(line for line in config.splitlines() if line.startswith('data = '))\n"
                    "request=json.loads(json.loads(line[7:]))\n"
                    "assert request['state']['review.request'].startswith('test request')\n"
                    "assert request['model']=='jev-fixture-selected'\n"
                    "assert set(request['questions']['decision']['criteria'])=={'continue','stop','defer'}\n"
                    + after + ("sys.exit(7)\n" if mode == "transport" else
                        "sys.stdout.write(" + repr(provider_body + "\n" + str(status)) + ")\n"))
                curl.chmod(0o700)
                env = dict(os.environ)
                env["PATH"] = f"{temp}:{REPO}/target/debug:{REPO}/lang/.lake/build/bin:{Path.home()}/.cargo/bin:" + env.get("PATH", "")
                env["OMARC_BIN"] = str(REPO / "lang/.lake/build/bin/omarc")
                env["TYPESAFE_API_KEY"] = "fixture-not-a-real-key"
                env["OMAR_JEV_MODEL"] = "jev-fixture-selected"
                env.pop("OMAR_JEV_ON_ERROR", None)
                if policy != "default": env["OMAR_JEV_ON_ERROR"] = policy
                if mode == "missing_key": env.pop("TYPESAFE_API_KEY", None)
                env["OMAR_HOME"] = tempfile.mkdtemp(prefix="omar-jev-adapter-test-")
                message = "test request" + ("x" * 65536 if mode == "blocked_input" else "")
                result = subprocess.run([str(REPO / "target/debug/omar"), "run", str(temp / "workflow.omar"),
                    "--input", "review.request=" + message, "--timeout-seconds", "30", "--fast", "--fg"],
                    env=env, text=True, capture_output=True, timeout=60)
                session = re.search(r"Started session ([\w-]+)", result.stderr)
                if session:
                    subprocess.run([str(REPO / "target/debug/omar"), "down", "-s", session.group(1)],
                                   env=env, capture_output=True, timeout=30)
                combined = result.stdout + result.stderr
                self.assertNotIn("fixture-not-a-real-key", combined)
                self.assertNotIn("sensitive-provider-body", combined)
                if mode == "missing_key": self.assertFalse((temp / "pid").exists())
                if mode in ("timeout", "blocked_input"):
                    pid = int((temp / "pid").read_text())
                    with self.assertRaises(ProcessLookupError): os.kill(pid, 0)
                if policy == "default":
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Jev " + kind, combined)
                    self.assertNotIn("Output followup.note", result.stdout)
                    continue
                self.assertEqual(result.returncode, 0, combined[-4000:])
                expected = "continue" if kind is None else "defer"
                self.assertIn(f'Output review.out = "{expected}"', result.stdout)
                self.assertIn('Output followup.note = "stub"', result.stdout)
                line = next(line for line in result.stdout.splitlines() if line.startswith("Output review.evidence = "))
                evidence = json.loads(json.loads(line.split(" = ",1)[1]))
                self.assertEqual(evidence["context"], {"review.request":message})
                if kind is None:
                    self.assertEqual(evidence["model"], "test-jev")
                    self.assertEqual(evidence["answers"]["decision"]["probabilities"], response["answers"]["decision"]["probabilities"])
                else:
                    self.assertEqual(evidence["error"]["kind"], kind)
                    self.assertEqual(evidence["model"], "jev-fixture-selected")
                    if status != 200: self.assertEqual(evidence["error"]["http_status"], status)
    def test_invalid_policy_or_incompatible_schema_stops_before_http(self):
        for mode in ("bad_policy", "empty_model", "no_evidence", "no_defer"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp:
                temp = Path(temp)
                (temp / "schemas").mkdir()
                options = ["continue", "stop"] if mode == "no_defer" else ["continue", "stop", "defer"]
                (temp / "schemas/decision.json").write_text(json.dumps({"type":"string", "enum":options}))
                source = '''type Decision from "./schemas/decision.json"
team Review[reviewer : Jev] {
    input request : string
    output out : Decision
    task reviewer(request) -> out "Choose an option."
}
main JevConfigurationTest { review = Review() }
'''
                if mode != "no_evidence":
                    source = source.replace("output out : Decision", "output out : Decision\n    output evidence : string")
                    source = source.replace('-> out "', '-> out, evidence "')
                (temp / "workflow.omar").write_text(source)
                curl = temp / "curl"
                curl.write_text("#!/usr/bin/env python3\nfrom pathlib import Path\nPath(" + repr(str(temp / "called")) + ").touch()\n")
                curl.chmod(0o700)
                env = dict(os.environ, TYPESAFE_API_KEY="fixture-not-a-real-key", OMAR_JEV_MODEL="jev-fixture-selected")
                env["PATH"] = f"{temp}:{REPO}/target/debug:{REPO}/lang/.lake/build/bin:{Path.home()}/.cargo/bin:" + env.get("PATH", "")
                env["OMARC_BIN"] = str(REPO / "lang/.lake/build/bin/omarc")
                env["OMAR_HOME"] = tempfile.mkdtemp(prefix="omar-jev-configuration-test-")
                env["OMAR_JEV_ON_ERROR"] = "bad" if mode == "bad_policy" else "defer"
                if mode == "empty_model": env["OMAR_JEV_MODEL"] = " "
                result = subprocess.run([str(REPO / "target/debug/omar"), "run", str(temp / "workflow.omar"),
                    "--input", "review.request=test", "--timeout-seconds", "30", "--fast", "--fg"],
                    env=env, text=True, capture_output=True, timeout=60)
                session = re.search(r"Started session ([\w-]+)", result.stderr)
                if session:
                    subprocess.run([str(REPO / "target/debug/omar"), "down", "-s", session.group(1)],
                                   env=env, capture_output=True, timeout=30)
                self.assertNotEqual(result.returncode, 0)
                expected = {"bad_policy":"must be fail or defer", "empty_model":"must not be empty",
                            "no_evidence":"requires a defer enum value", "no_defer":"requires a defer enum value"}[mode]
                self.assertIn(expected, result.stderr)
                self.assertFalse((temp / "called").exists())

    def test_shipped_example_parses_evidence_in_rust(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            response = {"model":"test-jev", "answers":{"decision":{"type":"choice","choice":"continue",
                "confidence":.97, "probabilities":{"continue":.98,"stop":.01,"defer":.01}}}}
            curl = temp / "curl"
            curl.write_text("#!/usr/bin/env python3\nimport sys\nsys.stdin.read()\nsys.stdout.write(" + repr(json.dumps(response)+"\n200") + ")\n")
            curl.chmod(0o700)
            env = dict(os.environ, TYPESAFE_API_KEY="fixture-not-a-real-key")
            env.pop("OMAR_JEV_ON_ERROR", None)
            env["PATH"] = f"{temp}:{REPO}/target/debug:{REPO}/lang/.lake/build/bin:{Path.home()}/.cargo/bin:" + env.get("PATH", "")
            env["OMARC_BIN"] = str(REPO / "lang/.lake/build/bin/omarc")
            env["OMAR_HOME"] = tempfile.mkdtemp(prefix="omar-jev-example-test-")
            result = subprocess.run([str(REPO / "target/debug/omar"), "run", str(REPO / "tests/topology/src/JevReview.omar"),
                "--input", "review.request=test request", "--timeout-seconds", "30", "--fast", "--fg"],
                env=env, text=True, capture_output=True, timeout=60)
            session = re.search(r"Started session ([\w-]+)", result.stderr)
            if session:
                subprocess.run([str(REPO / "target/debug/omar"), "down", "-s", session.group(1)],
                               env=env, capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("Output decider.proceed = true", result.stdout)
            self.assertNotIn("Output decider.stop", result.stdout)
            self.assertNotIn("Output decider.needs_review", result.stdout)



if __name__ == "__main__":
    unittest.main()
