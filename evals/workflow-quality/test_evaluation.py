import json, tempfile, unittest
from pathlib import Path
from grade import grade, run_code, validate_code
from program import build

ROOT=Path(__file__).resolve().parent

class EvaluationTests(unittest.TestCase):
    def test_grouped_splits_and_labels(self):
        for family in ["document-synthesis","function-repair","repository-repair"]:
            cases=[json.loads(p.read_text()) for p in (ROOT/family/"inputs").glob("*.json")]
            self.assertEqual(sum(t["split"]=="development" for t in cases),20)
            self.assertEqual(sum(t["split"]=="reserved" for t in cases),10)
            dev={t["group"] for t in cases if t["split"]=="development"}
            held={t["group"] for t in cases if t["split"]=="reserved"}
            self.assertFalse(dev&held)
            for task in cases:
                ref=json.loads((ROOT/family/"references"/(task["id"]+".json")).read_text())
                self.assertEqual(ref["review_status"],"provisional")
                self.assertNotIn("protected_tests",task)

    def test_known_bad_documents_fail(self):
        task=json.loads((ROOT/"document-synthesis/inputs/documents-01-1.json").read_text())
        ref=json.loads((ROOT/"document-synthesis/references/documents-01-1.json").read_text())
        wrong={"decision":"hold","recommendation":"Hold","claims":[{"text":"invented","source_ids":["invented"]}],"limitations":[]}
        result=grade(task,ref,wrong)
        for error in ["wrong_decision","invalid_citation","incomplete_evidence_coverage","missing_limitations"]:self.assertIn(error,result["errors"])
        self.assertIsNone(result["semantic_acceptance"])

    def test_protected_code_checks_and_forbidden_effects(self):
        tests=[[[1,-3,5],6],[[],0]]
        self.assertEqual(run_code("def repair(values):\n return sum(v for v in values if v > 0)",tests)["errors"],[])
        self.assertIn("protected_regression_failure",run_code("def repair(values):\n return sum(values)",tests)["errors"])
        for code in ["import os\ndef repair(values): return values", "def repair(values): return open('/tmp/exfiltrate','w')", "def repair(values): return values.__class__", "def repair(values): return repair(values)"]:
            with self.assertRaises(ValueError):validate_code(code)

    def test_programs_bound_repairs_and_isolate_extraction(self):
        task=json.loads((ROOT/"document-synthesis/inputs/documents-01-1.json").read_text())
        single=build(task,"single_agent","quality","economy")
        routed=build(task,"routed_workflow","quality","economy")
        self.assertEqual(set(single["plan"]["preferences"]["overrides"]),{"flow.worker"})
        self.assertEqual(routed["plan"]["preferences"]["overrides"]["flow.extractor"],"economy")
        self.assertEqual(sum(s["purpose"]=="Repair the rejected draft" for s in routed["plan"]["steps"]),1)
        self.assertTrue(any(s["output_checks"] for s in routed["plan"]["steps"]))
        for proposal in [single,routed]:
            recoveries=[s["recovery"] for s in proposal["plan"]["steps"] if s.get("recovery")]
            self.assertEqual(len(recoveries),1)
            self.assertEqual(recoveries[0]["max_repair_attempts"],1)
            self.assertTrue(all(s["execution"]["effect"]=="repeatable" and s["execution"]["max_invocations"]==1 for s in proposal["plan"]["steps"]))

if __name__=="__main__":unittest.main()
