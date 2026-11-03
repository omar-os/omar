import copy
import json
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import evaluation_protocol as protocol
import review
import run


class ReviewTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.binary = self.root / "omar"
        self.compiler = self.root / "omarc"
        self.binary.write_bytes(b"test executable fixture, never executed")
        self.compiler.write_bytes(b"test compiler fixture, never executed")
        self.study = self.root / "study"
        self.spec = protocol.prepare(self.study,["documents-01-1"],omar=self.binary,compiler=self.compiler,
                                     attempt_seconds=30,campaign_seconds=60)

    def tearDown(self):
        self.directory.cleanup()

    def arguments(self, output="campaign", execute=False):
        return SimpleNamespace(protocol=self.study/"protocol.json",labels_review=None,execute=execute,output=self.root/output)

    def approved_labels(self):
        packet_path = self.root / "labels"
        review.label_packet(self.study/"protocol.json",packet_path)
        answers = protocol.read(packet_path/"reviewer/answers.json")
        answers.update(reviewer="Synthetic test reviewer",reviewed_at=int(time.time()),independent_review_attested=True)
        for entry in answers["entries"]:
            entry.update(verdict="approved",notes="Synthetic test assertion only; not a real human label review.",human_minutes=None)
        protocol.write(packet_path/"reviewer/answers.json",answers)
        destination = self.root / "label-review.json"
        review.submit(packet_path,packet_path/"reviewer/answers.json",destination)
        return destination

    def completed_campaign(self):
        args = self.arguments()
        run.evaluate(args)
        rows = protocol.read(args.output/"results.json")
        for index,row in enumerate(rows):
            row.update(status="completed",attempted=True,elapsed_seconds=10+index,
                       quality={"deterministic_pass":index!=2,"semantic_acceptance":None})
            directory = args.output/row["artifact_directory"]
            protocol.write(directory/"outputs.json",{"flow.result":{"recommendation":"A synthetic test output", "claims":[]}})
            protocol.write(directory/"receipt.json",{"fixture_only":True})
            row["outputs_digest"] = protocol.file_digest(directory/"outputs.json")
            row["receipt_digest"] = protocol.file_digest(directory/"receipt.json")
        protocol.write(args.output/"results.json",rows)
        return args.output, rows

    def test_protocol_freezes_every_configuration_and_detects_input_binary_and_harness_changes(self):
        self.assertEqual({r["configuration"] for r in self.spec["schedule"]},set(protocol.CONFIGURATIONS))
        self.assertEqual(protocol.load(self.study/"protocol.json")["digest"],self.spec["digest"])
        path = self.study/self.spec["tasks"][0]["input"]
        original = path.read_bytes();path.write_bytes(original+b" ")
        with self.assertRaisesRegex(ValueError,"task input/reference changed"):protocol.load(self.study/"protocol.json")
        path.write_bytes(original)
        self.binary.write_bytes(b"changed build")
        with self.assertRaisesRegex(ValueError,"executable/compiler changed"):protocol.load(self.study/"protocol.json")
        self.binary.write_bytes(b"test executable fixture, never executed")
        altered = copy.deepcopy(self.spec);altered["harness"]["program.py"]="wrong"
        altered["digest"]=protocol.digest({k:v for k,v in altered.items() if k!="digest"})
        protocol.write(self.study/"protocol.json",altered)
        with self.assertRaisesRegex(ValueError,"harness changed"):protocol.load(self.study/"protocol.json")

    def test_no_runtime_probe_occurs_before_required_label_review(self):
        with patch("run.subprocess.run") as process:
            with self.assertRaisesRegex(ValueError,"labels-review"):run.evaluate(self.arguments(execute=True))
            process.assert_not_called()

    def test_label_reviews_bind_every_reference_and_cannot_be_incomplete_or_reused_after_change(self):
        destination = self.approved_labels()
        record = protocol.read(destination)
        protocol.require_labels(self.spec,record)
        record["entries"][0]["verdict"]="needs_changes"
        record["digest"]=protocol.digest({k:v for k,v in record.items() if k!="digest"})
        with self.assertRaisesRegex(ValueError,"approved"):protocol.require_labels(self.spec,record)
        record["entries"][0]["verdict"]="approved";record["protocol_digest"]="different protocol"
        record["digest"]=protocol.digest({k:v for k,v in record.items() if k!="digest"})
        with self.assertRaisesRegex(ValueError,"bind"):protocol.require_labels(self.spec,record)
        packet = self.root/"labels"
        case = protocol.read(packet/"reviewer/packet.json")["cases"][0]
        (packet/"reviewer"/case["files"]["reference"]["path"]).write_text("{}")
        with self.assertRaisesRegex(ValueError,"changed"):review.submit(packet,packet/"reviewer/answers.json",self.root/"changed-review.json")

    def test_output_packet_hides_configuration_scores_and_keeps_candidate_identity(self):
        campaign,_ = self.completed_campaign()
        packet = self.root/"outputs"
        review.output_packet(campaign,packet)
        visible = "\n".join(p.read_text() for p in (packet/"reviewer").rglob("*.json"))
        for forbidden in ["single_agent","routed_workflow","same_model_workflow","codex-quality","elapsed_seconds","deterministic_pass"]:
            self.assertNotIn(forbidden,visible)
        self.assertEqual(len(protocol.read(packet/"operator.json")["mapping"]),3)
        case = protocol.read(packet/"reviewer/packet.json")["cases"][0]
        (packet/"reviewer"/case["files"]["candidate"]["path"]).write_text("changed")
        with self.assertRaisesRegex(ValueError,"changed"):review.analyze(packet)

    def test_human_acceptance_cannot_override_deterministic_failure_or_imply_known_cost(self):
        campaign,rows = self.completed_campaign()
        packet = self.root/"outputs";review.output_packet(campaign,packet)
        pending = review.analyze(packet)
        self.assertEqual(sum(v["unresolved"] for v in pending["by_configuration"].values()),2)
        self.assertFalse(pending["superiority_established"])
        answers = protocol.read(packet/"reviewer/answers.json")
        answers.update(reviewer="Synthetic test reviewer",reviewed_at=int(time.time()),independent_review_attested=True)
        for entry in answers["entries"]:
            entry.update(verdict="accepted",notes="Synthetic rubric acceptance only.",human_minutes=.5,critical_errors=[],substantive_edits=0)
        protocol.write(packet/"reviewer/answers.json",answers)
        submitted = self.root/"output-review.json";review.submit(packet,packet/"reviewer/answers.json",submitted)
        result = review.analyze(packet,submitted)
        self.assertEqual(sum(v["accepted"] for v in result["by_configuration"].values()),2)
        self.assertEqual(sum(v["human_reviewed"] for v in result["by_configuration"].values()),3)
        self.assertTrue(all(v["cost_usd"]["sum_if_complete"] is None for v in result["by_configuration"].values()))
        self.assertEqual(len(result["paired_groups"]),1)
        self.assertFalse(result["qualifies_routing"])
        with self.assertRaisesRegex(ValueError,"append-only"):review.submit(packet,packet/"reviewer/answers.json",submitted)

    def test_acceptance_with_critical_errors_or_required_edits_is_rejected(self):
        campaign,_ = self.completed_campaign();packet = self.root/"outputs";review.output_packet(campaign,packet)
        answers = protocol.read(packet/"reviewer/answers.json")
        answers.update(reviewer="Synthetic test reviewer",reviewed_at=int(time.time()),independent_review_attested=True)
        for entry in answers["entries"]:
            entry.update(verdict="accepted",notes="Test only",critical_errors=["unsupported claim"],substantive_edits=0)
        protocol.write(packet/"reviewer/answers.json",answers)
        with self.assertRaisesRegex(ValueError,"zero critical"):review.submit(packet,packet/"reviewer/answers.json",self.root/"bad-review.json")

    def test_removed_attempts_or_changed_runtime_artifacts_cannot_disappear_from_review(self):
        campaign,rows = self.completed_campaign()
        protocol.write(campaign/"results.json",rows[:2])
        with self.assertRaisesRegex(ValueError,"incomplete or duplicated"):review.output_packet(campaign,self.root/"missing")
        protocol.write(campaign/"results.json",rows)
        (campaign/rows[0]["artifact_directory"]/"outputs.json").write_text('{"flow.result":"different"}')
        with self.assertRaisesRegex(ValueError,"changed"):review.output_packet(campaign,self.root/"changed")

    def test_legacy_initial_failures_remain_in_the_denominator_without_promotion(self):
        campaign,rows = self.completed_campaign()
        protocol.write(campaign/"manifest.json",{"schema_version":1})
        protocol.write(campaign/"initial-attempts.json",[{"configuration":"single_agent","status":"interrupted","quality":None}])
        packet = self.root/"legacy";review.output_packet(campaign,packet)
        result = review.analyze(packet)
        self.assertEqual(result["attempted"],4)
        self.assertEqual(result["by_configuration"]["single_agent"]["runtime_failures"],1)
        self.assertEqual(result["paired_groups"],[])
        self.assertFalse(result["superiority_established"])

    def test_running_and_unstarted_rows_remain_explicitly_incomplete(self):
        campaign,rows = self.completed_campaign()
        rows[0].update(status="running",quality=None);rows[1].update(status="not_started",attempted=False,quality=None)
        protocol.write(campaign/"results.json",rows)
        packet = self.root/"unfinished";review.output_packet(campaign,packet)
        result = review.analyze(packet)
        self.assertEqual(result["attempted"],2)
        self.assertEqual(sum(c["not_started"] for c in result["by_configuration"].values()),1)
        self.assertEqual(sum(c["incomplete_attempts"] for c in result["by_configuration"].values()),1)
        self.assertTrue(all(g["routed_minus_single"] is None for g in result["paired_groups"]))

    def test_attempt_is_recorded_before_launch_and_interrupt_does_not_erase_schedule(self):
        args = self.arguments(execute=True);args.labels_review=self.approved_labels()
        def launch(*unused):
            rows = protocol.read(args.output/"results.json")
            self.assertEqual(len(rows),3)
            self.assertEqual(rows[0]["status"],"running")
            self.assertTrue(rows[0]["attempted"])
            raise KeyboardInterrupt()
        process = SimpleNamespace(returncode=0,stdout="ChatGPT",stderr="")
        with patch("run.subprocess.run",return_value=process),patch("run.start_daemon",side_effect=launch),patch.dict("os.environ",{"OPENAI_API_KEY":"","AZURE_OPENAI_API_KEY":""}):
            result = run.evaluate(args)
        self.assertEqual(result["attempted_runs"],1)
        self.assertEqual([r["status"] for r in result["results"]],["interrupted","not_started","not_started"])
        self.assertIsNone(result["results"][0]["quality"]["deterministic_pass"])

    def test_whole_attempt_deadline_prevents_native_run_after_slow_startup(self):
        args = self.arguments(execute=True);args.labels_review=self.approved_labels()
        clock = [0]
        def launch(directory,*unused):
            state=directory/"state";protocol.write(state/"mcp/ea-0/context.json",{"serve":{"token":"fixture"}})
            clock[0]+=31
            return Mock(),None,"http://unused.invalid",state,None
        process = SimpleNamespace(returncode=0,stdout="ChatGPT",stderr="")
        with patch("run.subprocess.run",return_value=process),patch("run.start_daemon",side_effect=launch),patch("run.request") as request,patch("run.time.monotonic",side_effect=lambda:clock[0]),patch.dict("os.environ",{"OPENAI_API_KEY":"","AZURE_OPENAI_API_KEY":""}):
            result=run.evaluate(args)
            request.assert_not_called()
        self.assertEqual([r["status"] for r in result["results"]],["failed","failed","not_started"])


if __name__ == "__main__":
    unittest.main()
