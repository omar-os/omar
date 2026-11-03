import json
import os
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
from grade import grade
from repository_fixtures import generate
from repository_grade import apply_patch, fingerprint, grade_repository
from repository_sandbox import run, resolve_image, docker_connection, InfrastructureError

ROOT=Path(__file__).resolve().parent


def fixture():
    name="repo-schedule-01.json"
    return (json.loads((ROOT/"repository-repair/inputs"/name).read_text()),
            json.loads((ROOT/"repository-repair/references"/name).read_text()))


class RepositoryContractTests(unittest.TestCase):
    def test_all_comparison_configs_bind_the_same_private_runtime_suite(self):
        from program import build
        from repository_protocol import suite_for, check_for, bind_catalog
        task,reference=fixture()
        suite=suite_for(task,reference,"sha256:"+"a"*64)
        for configuration in ["single_agent","same_model_workflow","routed_workflow"]:
            proposal=build(task,configuration,"quality","economy",repository_verifier=suite)
            checks=[step["output_checks"] for step in proposal["plan"]["steps"] if step["output_checks"]]
            self.assertEqual(checks,[[check_for(suite)],[check_for(suite)]])
            self.assertNotIn("protected_tests",proposal["program"])
            self.assertNotIn("oracle_patch",proposal["inputs"]["flow.request"])
            bind_catalog(proposal,{"repository_verifiers":[{"id":suite["id"],"digest":"host-authoritative-digest"}]})
            self.assertTrue(all(c["verifier_digest"]=="host-authoritative-digest" for step in proposal["plan"]["steps"] for c in step["output_checks"]))

    def test_remote_engine_is_rejected_before_contact(self):
        docker_connection.cache_clear()
        try:
            with patch.dict(os.environ, {"DOCKER_HOST":"tcp://example.invalid:2375","DOCKER_CONTEXT":""}), patch("repository_sandbox.subprocess.run") as launch:
                with self.assertRaises(InfrastructureError):docker_connection()
                launch.assert_not_called()
        finally:
            docker_connection.cache_clear()

    def test_generator_is_reproducible_with_repository_groups_and_private_references(self):
        with tempfile.TemporaryDirectory() as folder:
            generate(Path(folder))
            paths=sorted((ROOT/"repository-repair/inputs").glob("*.json"))
            self.assertEqual(len(paths),30)
            groups={"development":set(),"reserved":set()}
            for path in paths:
                task=json.loads(path.read_text())
                self.assertEqual(path.read_bytes(),(Path(folder)/"inputs"/path.name).read_bytes())
                self.assertEqual(task["repository_digest"],fingerprint(task["files"]))
                self.assertGreaterEqual(len(task["files"]),7)
                self.assertNotIn("oracle_patch",task)
                self.assertNotIn("protected_tests",task)
                groups[task["split"]].add(task["group"])
            self.assertEqual(len(groups["development"]),4)
            self.assertEqual(len(groups["reserved"]),2)
            self.assertFalse(groups["development"] & groups["reserved"])

    def test_patches_are_real_source_changes_and_cannot_modify_tests_or_escape(self):
        task,reference=fixture()
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            patch=apply_patch(task,reference["oracle_patch"],root/"candidate")
            self.assertIn("--- a/candidate_app/policy.py",patch["diff"])
            self.assertNotEqual(patch["before_digest"],patch["after_digest"])
            self.assertEqual((root/"candidate/tests/test_public.py").read_text(),task["files"]["tests/test_public.py"])
            for name in ["../outside","/tmp/outside","tests/test_public.py","candidate_app/../outside",".git/config"]:
                with self.assertRaises(ValueError):
                    apply_patch(task,{"changes":[{"path":name,"content":"bad"}],"explanation":"test"},root/"bad")
            duplicate={"changes":reference["oracle_patch"]["changes"]*2,"explanation":"duplicate"}
            with self.assertRaises(ValueError):apply_patch(task,duplicate,root/"bad")
            with self.assertRaises(ValueError):apply_patch(task,{"changes":[],"explanation":"empty"},root/"bad")
            changed={**task,"repository_digest":"0"*64}
            with self.assertRaises(ValueError):apply_patch(changed,reference["oracle_patch"],root/"bad")
            self.assertFalse((root/"bad").exists())

    def test_missing_execution_boundary_is_unknown_never_a_quality_pass(self):
        task,ref=fixture()
        result=grade(task,ref,ref["oracle_patch"])
        self.assertIsNone(result["deterministic_pass"])
        self.assertEqual(result["evaluation_status"],"infrastructure_failure")

    def test_host_io_failures_do_not_become_candidate_quality_failures(self):
        task,ref=fixture()
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            with patch("repository_grade.run",side_effect=OSError("unavailable test infrastructure")):
                result=grade_repository(task,ref,ref["oracle_patch"],image="unused",artifacts=root/"attempt")
                self.assertIsNone(result["deterministic_pass"])
                self.assertEqual(result["evaluation_status"],"infrastructure_failure")
            before=(root/"attempt/grade.json").read_bytes()
            again=grade_repository(task,ref,ref["oracle_patch"],image="unused",artifacts=root/"attempt")
            self.assertIsNone(again["deterministic_pass"])
            self.assertEqual((root/"attempt/grade.json").read_bytes(),before)


@unittest.skipUnless(os.environ.get("OMAR_REPOSITORY_TEST_IMAGE"),"explicit local container checks; no model calls")
class RepositoryContainerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.image=resolve_image(os.environ["OMAR_REPOSITORY_TEST_IMAGE"])

    def test_boundary_denies_host_reads_writes_and_external_network(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);root.chmod(0o755)
            secret=root/"host-only-canary";secret.write_text("must not reach candidate")
            repo=root/"candidate";repo.mkdir();(repo/"candidate_app").mkdir()
            (repo/"candidate_app/__init__.py").write_text('''import os, socket
def handle(request):
    denied=[]
    for path in [request['host_path'], '/workspace/forbidden-write', '/forbidden-write']:
        try:
            with open(path, 'r' if path == request['host_path'] else 'w') as stream: stream.read() if path == request['host_path'] else stream.write('bad')
            denied.append(False)
        except OSError: denied.append(True)
    try:
        socket.create_connection(('192.0.2.1',443),timeout=0.2).close()
        network=False
    except OSError: network=True
    return {'denied':denied,'network_denied':network,'uid':os.getuid(),'socket_mounted':os.path.exists('/var/run/docker.sock')}
''')
            result=run(repo,[{"host_path":str(secret)}],self.image)
            self.assertEqual(result["exit_code"],0,result)
            self.assertEqual(json.loads(result["stdout"])["value"],{"denied":[True,True,True],"network_denied":True,"uid":65534,"socket_mounted":False})
            self.assertEqual(secret.read_text(),"must not reach candidate")
            self.assertFalse((repo/"forbidden-write").exists())

    def test_time_and_output_bounds_stop_candidates(self):
        for source,expected in [("def handle(request):\n while True: pass\n","timeout"),
                                ("def handle(request):\n while True: print('x'*8192,flush=True)\n","output_limit")]:
            with tempfile.TemporaryDirectory() as folder:
                root=Path(folder);root.chmod(0o755);(root/"candidate_app").mkdir()
                (root/"candidate_app/__init__.py").write_text(source)
                result=run(root,[{}],self.image,timeout=2,max_output_bytes=8192)
                self.assertEqual(result["stop_reason"],expected,result)
                self.assertLessEqual(len(result["stdout"].encode())+len(result["stderr"].encode()),8192)

    def test_actual_repository_patch_passes_and_visible_only_patch_fails_regressions(self):
        task,ref=fixture()
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            good=grade_repository(task,ref,ref["oracle_patch"],image=self.image,artifacts=root/"good")
            self.assertTrue(good["deterministic_pass"],good)
            self.assertTrue((root/"good/candidate.patch").is_file())
            bad={"changes":[{"path":"candidate_app/api.py","content":"def handle(request):\n    return [[1,6]]\n"}],"explanation":"Hardcoded visible example"}
            result=grade_repository(task,ref,bad,image=self.image,artifacts=root/"bad")
            self.assertFalse(result["deterministic_pass"],result)
            self.assertTrue(any(error.startswith("protected_regression_failure") for error in result["errors"]))
            self.assertIsNone(result["semantic_acceptance"])


if __name__=="__main__": unittest.main()
