import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import designer_journey as journey
from evaluation_protocol import write


def fixture():
    profile = {"id": "codex-quality", "backend": "codex", "isolation": "ports_only"}
    step = {"reaction": "flow.reaction.0", "logical_id": "draft", "required_tools": [], "execution": {"effect": "repeatable", "max_invocations": 1, "deadline_ms": 90000},
        "interface": {"owner": "model", "implementation_digest": "initial", "outputs": [{"port": "flow.result"}]},
        "output_checks": [{"kind": "grounded_brief", "port": "flow.result", "allowed_source_ids": ["policy", "measurement", "review"], "minimum_claims": 1}]}
    initial = {"id": "plan", "revision": 1, "source_digest": "initial", "program": "initial", "inputs": {"flow.request": "supplied notes"}, "profiles": {"flow.worker": profile},
        "draft": {"steps": [step], "intent": {"assumptions": [], "requirements": []}, "research": {"usage": {"catalog_calls": 2, "source_calls": 1,
            "captures": [{"location": "configured:execution-profiles"}, {"location": "configured:capability-evidence"}, {"location": "chat:c/message:1"}]}},
            "questions": [{"id": "audience", "required": True, "answer": None, "target_reactions": ["flow.reaction.0"]}]}}
    saved = copy.deepcopy(initial); saved["revision"] = 2
    final = copy.deepcopy(initial); final.update(revision=3, source_digest="final", program="external_customers")
    final["draft"]["steps"][0]["interface"]["implementation_digest"] = "changed"
    final["draft"]["questions"] = []
    final["draft"]["intent"]["requirements"] = [{"origin": {"kind": "user_confirmed", "confirmation": {"kind": "review_answer", "plan": journey.reference(saved), "question_id": "audience", "answer": journey.ANSWER}}}]
    return profile, initial, saved, final


class DesignerJourneyTests(unittest.TestCase):
    def test_preparation_and_refusal_make_no_runtime_or_login_calls(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(journey.subprocess, "run", side_effect=AssertionError("runtime probe")), patch.object(journey.subprocess, "Popen", side_effect=AssertionError("model start")):
            root = Path(temp); binary = root / "binary"; binary.write_text("owned placeholder")
            protocol = journey.prepare(root / "packet", binary, binary)
            path = root / "packet/protocol.json"
            self.assertEqual(journey.load(path), protocol)
            with self.assertRaisesRegex(ValueError, "Execution requires"): journey.execute(path)
            self.assertFalse((path.parent / "attempt").exists())
            binary.write_text("changed")
            with self.assertRaisesRegex(ValueError, "binary changed"): journey.load(path)

    def test_scope_rejects_expanded_tools_steps_profiles_or_unchecked_outputs(self):
        profile, initial, _, _ = fixture()
        journey.validate_plan(initial, profile)
        cases = []
        for key, value in [("required_tools", ["web"]), ("execution", {"effect": "repeatable", "max_invocations": 2, "deadline_ms": 90000}), ("output_checks", [])]:
            plan = copy.deepcopy(initial); plan["draft"]["steps"][0][key] = value; cases.append(plan)
        plan = copy.deepcopy(initial); plan["profiles"]["flow.worker"]["isolation"] = "native_tools"; cases.append(plan)
        plan = copy.deepcopy(initial); plan["draft"]["steps"] *= 6; cases.append(plan)
        plan = copy.deepcopy(initial); plan["draft"]["integrations"] = {"connections": ["remote"]}; cases.append(plan)
        plan = copy.deepcopy(initial); plan["draft"]["steps"][0]["interface"]["owner"] = "code"; cases.append(plan)
        plan = copy.deepcopy(initial); plan["draft"]["research"]["usage"]["source_calls"] = 0; cases.append(plan)
        for plan in cases:
            with self.subTest(plan=plan), self.assertRaises(ValueError): journey.validate_plan(plan, profile)

    def test_redesign_needs_exact_answer_provenance_and_affected_source_change(self):
        profile, initial, saved, final = fixture()
        journey.validate_plan(final, profile, initial, saved)
        for change in ["source", "step", "origin", "question", "assumption"]:
            plan = copy.deepcopy(final)
            if change == "source": plan["source_digest"] = initial["source_digest"]
            elif change == "step": plan["draft"]["steps"][0]["interface"]["implementation_digest"] = "initial"
            elif change == "origin": plan["draft"]["intent"]["requirements"][0]["origin"]["confirmation"]["plan"]["revision"] = 1
            elif change == "question": plan["draft"]["questions"] = initial["draft"]["questions"]
            else: plan["draft"]["intent"]["assumptions"] = [{"accepted": False}]
            with self.subTest(change=change), self.assertRaises(ValueError): journey.validate_plan(plan, profile, initial, saved)

    def test_failed_or_unchecked_outputs_never_become_accepted(self):
        value = {"decision": "proceed", "audience": "external_customers", "recommendation": "Proceed with the release.", "claims": [{"text": "Approved", "source_ids": ["review"]}], "limitations": []}
        invocations = [{"status": "contract_satisfied", "outputs": {"flow.result": json.dumps(value)}}]
        quality, _ = journey.result_checks({"status": "completed"}, invocations)
        self.assertTrue(quality["deterministic_pass"]); self.assertIsNone(quality["semantic_acceptance"])
        for state in ["failed", "stopped", "running"]:
            self.assertFalse(journey.result_checks({"status": state}, invocations)[0]["deterministic_pass"])
        invocations[0]["status"] = "failed"
        self.assertFalse(journey.result_checks({"status": "completed"}, invocations)[0]["deterministic_pass"])
        invocations[0]["status"] = "contract_satisfied"
        value["recommendation"] = "Morgan Example recommends proceeding"
        invocations[0]["outputs"]["flow.result"] = json.dumps(value)
        self.assertFalse(journey.result_checks({"status": "completed"}, invocations)[0]["deterministic_pass"])

    def test_api_credentials_refused_before_login_and_failure_is_durable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); binary = root / "binary"; binary.write_text("placeholder")
            journey.prepare(root / "packet", binary, binary)
            with patch.dict(journey.os.environ, {"OPENAI_API_KEY": "synthetic-do-not-print"}), patch.object(journey.subprocess, "run", side_effect=AssertionError("login probe")):
                result = journey.execute(root / "packet/protocol.json", authorized=True)
            self.assertEqual(result["status"], "failed"); self.assertFalse(result["attempted"])
            self.assertIsNone(result["quality"])
            self.assertEqual(result, journey.read(root / "packet/attempt/status.json"))
            with self.assertRaises(FileExistsError): journey.execute(root / "packet/protocol.json", authorized=True)


if __name__ == "__main__": unittest.main()
