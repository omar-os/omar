"""The reset must replay native launch arguments without another shell expansion."""
import shutil
import subprocess
import time
import unittest
import uuid
from context_interventions import launch_argv


@unittest.skipUnless(shutil.which('tmux'), 'requires tmux')
class LaunchRoundTrip(unittest.TestCase):
    def test_native_tmux_roundtrip_preserves_expansion_and_literal_dollars(self):
        server = 'omar-bench-test-' + uuid.uuid4().hex[:10]
        def tmux(*arguments):
            return subprocess.check_output(['tmux', '-L', server, *arguments], text=True).strip()
        try:
            command = 'x="$(printf hello)"; printf "%s\\n" "$x"; printf "%s\\n" \'$literal\'; sleep 30'
            tmux('new-session', '-d', '-s', 'test', 'sh', '-lc', command)
            encoded = tmux('display-message', '-p', '-t', 'test', '#{pane_start_command}')
            self.assertEqual(launch_argv(encoded), ['sh', '-lc', command])
            tmux('respawn-pane', '-k', '-t', 'test', *launch_argv(encoded))
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                captured = tmux('capture-pane', '-p', '-t', 'test')
                if captured == 'hello\n$literal': break
                time.sleep(.05)
            self.assertEqual(captured, 'hello\n$literal')
        finally:
            subprocess.run(['tmux', '-L', server, 'kill-server'], capture_output=True)


if __name__ == '__main__':
    unittest.main()
