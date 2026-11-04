"""Exported fixes must apply exactly, including files without final newlines."""
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "src/template_library"))
from repository_grade import apply_patch, fingerprint


class PatchExportTests(unittest.TestCase):
    def test_newline_variants_apply_without_changing_content(self):
        for before, after in [("old", "new\n"), ("old\n", "new"), ("old", "new")]:
            with self.subTest(before=before, after=after), tempfile.TemporaryDirectory() as temporary:
                root = pathlib.Path(temporary)
                files = {"sample.txt": before}
                result = apply_patch({"files": files, "repository_digest": fingerprint(files),
                                      "allowed_patch_paths": ["sample.txt"]},
                                     {"changes": [{"path": "sample.txt", "content": after}],
                                      "explanation": "Owned export fixture"}, root / "candidate")
                checkout = root / "checkout"
                checkout.mkdir()
                (checkout / "sample.txt").write_text(before)
                subprocess.run(["git", "apply", "-"], cwd=checkout,
                               input=result["diff"], text=True, check=True, capture_output=True)
                self.assertEqual((checkout / "sample.txt").read_text(), after)


if __name__ == "__main__":
    unittest.main()
