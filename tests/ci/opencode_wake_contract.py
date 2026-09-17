#!/usr/bin/env python3
"""Verify installed OpenCode's context-only vs inference-starting HTTP contract.
Uses an isolated project and a loopback mock model, with a provider allowlist.
"""
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

inference = threading.Event()


class Mock(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        inference.set()
        self.send_response(400)
        self.end_headers()
        self.wfile.write(b'{"error":{"message":"local inference observed"}}')


def main():
    cli = shutil.which("opencode")
    if not cli:
        raise SystemExit("OpenCode CLI required for this optional contract test")
    mock = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    process = None
    try:
        with tempfile.TemporaryDirectory(prefix="omar-opencode-contract-") as directory:
            root = Path(directory)
            config = {"enabled_providers": ["omar_mock"], "model": "omar_mock/mock", "plugin": [],
                      "provider": {"omar_mock": {"npm": "@ai-sdk/openai-compatible", "name": "OMAR local test",
                                   "options": {"baseURL": f"http://127.0.0.1:{mock.server_port}/v1", "apiKey": "local-only"},
                                   "models": {"mock": {"name": "Mock", "limit": {"context": 32000, "output": 1000}}}}}}
            env = {"PATH": os.environ["PATH"], "OPENCODE_CONFIG_CONTENT": json.dumps(config),
                   "OPENCODE_CONFIG_DIR": str(root / "config"), "OPENCODE_DISABLE_DEFAULT_PLUGINS": "1",
                   "OPENCODE_DISABLE_MODELS_FETCH": "1", "XDG_DATA_HOME": str(root / "data"),
                   "XDG_STATE_HOME": str(root / "state"), "XDG_CONFIG_HOME": str(root / "xdg-config"),
                   "XDG_CACHE_HOME": str(root / "cache")}
            with (root / "server.log").open("w+") as log:
                process = subprocess.Popen([cli, "serve", "--hostname", "127.0.0.1", "--port", str(port)],
                                           cwd=root, env=env, stdout=log, stderr=log)

                def request(path, body=None):
                    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}",
                          data=None if body is None else json.dumps(body).encode(),
                          headers={"Content-Type": "application/json"})
                    with urllib.request.urlopen(req, timeout=15) as response:
                        raw = response.read()
                        return response.status, json.loads(raw) if raw else None

                end = time.monotonic() + 30
                while True:
                    try:
                        request("/global/health")
                        break
                    except Exception:
                        if time.monotonic() >= end or process.poll() is not None:
                            log.seek(0)
                            raise AssertionError(log.read()[-3000:])
                        time.sleep(0.1)
                session = request("/session", {})[1]["id"]
                request(f"/session/{session}/message", {"noReply": True,
                        "model": {"providerID": "omar_mock", "modelID": "mock"},
                        "parts": [{"type": "text", "text": "Context only", "synthetic": True}]})
                assert not inference.wait(1), "noReply unexpectedly started inference"
                code, _ = request(f"/session/{session}/prompt_async", {
                        "parts": [{"type": "text", "text": "Act on this event", "synthetic": True}]})
                assert code == 204
                if not inference.wait(20):
                    log.seek(0)
                    raise AssertionError("prompt_async never started inference: " + log.read()[-3000:])
                print("PASS: installed OpenCode noReply stays idle; prompt_async returns 204 and starts local inference", flush=True)
    finally:
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        mock.shutdown()
        mock.server_close()


if __name__ == "__main__":
    main()
