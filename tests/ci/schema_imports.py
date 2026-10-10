#!/usr/bin/env python3
"""Exercise omarc's real filesystem import boundary without model calls."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
COMPILER = ROOT / "lang/.lake/build/bin/omarc"


class SchemaImports(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="omar-schema-imports-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / "project with spaces"
        self.project.mkdir()
        (self.project / "schemas").mkdir()
        self.schema = self.project / "schemas/decision.json"
        self.source = self.project / "review.omar"
        self.output = self.root / "compiled.json"
        self.schema.write_text(json.dumps({"type":"string", "enum":["continue","stop"]}))
        self.source.write_text('type Decision from "schemas/decision.json"\n'
            'team Review { input decision : Decision } main { review = Review() }')

    def compile(self):
        return subprocess.run([str(COMPILER), str(self.source), str(self.output)],
            cwd=self.root, capture_output=True, text=True, timeout=15)

    def test_relative_import_is_relative_to_source_not_cwd(self):
        result = self.compile()
        self.assertEqual(result.returncode, 0, result.stderr)
        ports = [item for item in json.loads(self.output.read_text())["instructions"]
            if item["op"] == "define_port"]
        self.assertEqual(ports[0]["type"], 'string in ["continue","stop"]')

    def test_absolute_import_path(self):
        self.source.write_text(f'type Decision from "{self.schema}" '
            'team T { input token : Decision } main { t = T() }')
        result = self.compile()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_import_fails_with_name_and_path(self):
        self.schema.unlink()
        result = self.compile()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Decision", result.stderr)
        self.assertIn("decision.json", result.stderr)
        self.assertFalse(self.output.exists())

    def test_invalid_and_unsupported_schemas_fail(self):
        for schema in ["not json", "[]", "{}", '{"type":"string","enum":[]}',
            '{"type":"string","enum":[1]}', '{"type":"string","enum":["x","x"]}',
            '{"type":"object","properties":{}}',
            '{"type":"string","enum":["x"],"minLength":2}',
            '{"type":"string","enum":["x"],"$ref":"remote.json"}']:
            with self.subTest(schema=schema):
                self.schema.write_text(schema)
                result = self.compile()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("schema type 'Decision'", result.stderr)
                self.assertFalse(self.output.exists())

    def test_escaped_unicode_and_metadata_survive(self):
        values = ['a"b', "line\nend", "雪", ""]
        self.schema.write_text(json.dumps({"type":"string", "enum":values,
            "title":"Decision", "description":"Choose one", "$schema":
            "https://json-schema.org/draft/2020-12/schema"}))
        result = self.compile()
        self.assertEqual(result.returncode, 0, result.stderr)
        instructions = json.loads(self.output.read_text())["instructions"]
        port = next(item for item in instructions if item["op"] == "define_port")
        self.assertEqual(json.loads(port["type"].removeprefix("string in ")), values)
        declared = next(item for item in instructions if item["op"] == "define_type")
        self.assertEqual(declared["name"], "Decision")
        self.assertEqual(declared["type"], port["type"])
        self.assertEqual(declared["title"], "Decision")
        self.assertEqual(declared["description"], "Choose one")


if __name__ == "__main__":
    if not COMPILER.exists():
        raise SystemExit("Build omarc first: cd lang && lake build")
    unittest.main()
