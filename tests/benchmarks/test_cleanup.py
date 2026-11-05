"""Fixture cleanup must not target unrelated readers or another trial."""
from pathlib import Path
import unittest
from unittest.mock import patch
from workflow import fixture_processes


class CleanupScope(unittest.TestCase):
    def test_only_owned_tree_and_private_native_server_are_selected(self):
        rows = '\n'.join([
            '900001 1 sh -lc fixture',
            '900002 900001 cursor-agent acp',
            '900003 1 codex app-server --listen unix:///tmp/bench-owned/app.sock',
            '900004 1 cat /tmp/bench-owned/report.txt',
            '900005 1 codex app-server --listen unix:///tmp/bench-other/app.sock',
        ])
        with patch('workflow.subprocess.check_output', return_value=rows):
            owned = fixture_processes(Path('/tmp/bench-owned'), [900001])
        self.assertEqual(set(owned), {900001, 900002, 900003})


if __name__ == '__main__':
    unittest.main()
