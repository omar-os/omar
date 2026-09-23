#!/usr/bin/env python3
"""Verify installed Claude retains its coding prompt with OMAR append flags.
All inference requests go to a loopback mock; no provider credentials are used.
Optional local contract test; requires the Claude CLI on PATH.
"""
import http.server
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading

requests = queue.Queue()


class Mock(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if "messages" in body:
            requests.put(body)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"id": "msg_mock", "type": "message", "role": "assistant",
                                    "model": "claude-sonnet-4-6", "content": [{"type": "text", "text": "OK"}],
                                    "stop_reason": "end_turn", "stop_sequence": None,
                                    "usage": {"input_tokens": 1, "output_tokens": 1}}).encode())


def main():
    cli = shutil.which("claude")
    if not cli:
        raise SystemExit("Claude CLI required for this optional contract test")
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="omar-claude-contract-") as directory:
            root = Path(directory)
            prompt = root / "coordination.md"
            marker = "OMAR_CONTRACT_COORDINATION_POLICY_7392"
            prompt.write_text(marker)
            env = {"PATH": os.environ["PATH"], "CLAUDE_CONFIG_DIR": str(root / "config"),
                   "ANTHROPIC_API_KEY": "local-mock-only", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{server.server_port}",
                   "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_TELEMETRY": "1",
                   "DISABLE_ERROR_REPORTING": "1", "API_TIMEOUT_MS": "10000"}
            for flag, value in [("--append-system-prompt", marker), ("--append-system-prompt-file", str(prompt))]:
                process = subprocess.run([cli, "-p", "Reply OK", "--model", "claude-sonnet-4-6",
                                          "--max-turns", "1", "--settings", '{"disableAllHooks":true}',
                                          "--setting-sources", "", flag, value], cwd=root, env=env,
                                         capture_output=True, text=True, timeout=35)
                bodies = []
                while not requests.empty():
                    bodies.append(requests.get_nowait())
                systems = [json.dumps(body.get("system", "")) for body in bodies]
                assert any(marker in system and len(system) > 5000 for system in systems), (
                    flag, process.returncode, process.stderr[-2000:], [len(s) for s in systems])
                assert any("software engineering" in system.lower() or "coding" in system.lower()
                           for system in systems if marker in system), "default coding policy missing"
                print(f"PASS: installed Claude sends default coding policy plus OMAR policy with {flag}", flush=True)
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
