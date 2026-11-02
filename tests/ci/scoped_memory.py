#!/usr/bin/env python3
"""Scoped memory through real session IPC, CLI and concurrent MCP processes."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()

with tempfile.TemporaryDirectory(prefix="omar-memory-") as folder:
    root = Path(folder)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("OMAR_") and k not in ("TMUX", "TMUX_PANE")}
    env.update(OMAR_HOME=str(root / "state"), TERM="xterm-256color")
    sessions = []

    def cli(*args, ok=True):
        result = subprocess.run([str(BIN), *args], cwd=root, env=env,
                                text=True, capture_output=True, timeout=90)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return json.loads(result.stdout) if ok else result.stderr

    def memory(ea, *args, ok=True, runtime="memory"):
        return cli("-s", runtime, "--ea", str(ea), "memory", *args, ok=ok)

    def mcp(context, name, arguments):
        request = dict(jsonrpc="2.0", id=1, method="tools/call",
                       params=dict(name=name, arguments=arguments))
        result = subprocess.run([str(BIN), "mcp-server", "--context-file", str(context)],
                                cwd=root, env=env, input=json.dumps(request) + "\n",
                                text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, result.stderr
        response = json.loads(result.stdout)["result"]
        assert not response.get("isError"), response
        return json.loads(response["content"][0]["text"])

    try:
        for name in ["memory", "unrelated"]:
            sessions.append(cli("up", "-s", name, "--no-ea", "--json"))
        session = sessions[0]
        state = Path(session["directory"])
        peer = cli("-s", "memory", "ea", "create", "--name", "related")["id"]
        outsider = cli("-s", "memory", "ea", "create", "--name", "unrelated")["id"]
        project = memory(0, "project-create", "Research")
        for ea in [0, peer]:
            memory(ea, "project-link", project["id"])
        record = memory(0, "save", "--scope", "project", "--key", "report-format",
                        "--kind", "preference", "--text", "Use five sections",
                        "--source", "Operator request")
        assert memory(peer, "recall")["records"][0]["id"] == record["id"]
        assert memory(outsider, "recall")["records"] == []
        assert memory(0, "recall", runtime="unrelated")["records"] == []
        print("PASS: a project preference reaches a linked EA, not an unlinked EA or another runtime")

        changed = memory(peer, "edit", record["id"], "--revision", "1",
                         "--text", "Use three sections", "--source", "Operator correction")
        assert changed["value"]["revision"] == 2
        rejected = memory(0, "edit", record["id"], "--revision", "1",
                          "--text", "Stale overwrite", "--source", "Old edit", ok=False)
        assert "revision conflict" in rejected, rejected
        print("PASS: correction persists; stale revision rejected:", rejected.strip())

        # Separate real MCP processes exercise the OS lock, not just threads.
        context = root / "context.json"
        context.write_text(json.dumps(dict(omar_dir=str(state), ea_id=0,
            session_prefix="omar-agent-", default_command="claude", default_workdir=str(root),
            health_idle_warning=15, tmux_server=session["tmux_server"])))

        def propose(n):
            return mcp(context, "memory_propose", dict(scope="ea", key=f"candidate-{n}",
                kind="preference", text=f"Candidate {n}", source="Source chat"))

        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            candidates = list(pool.map(propose, range(12)))
        assert len({item["id"] for item in candidates}) == 12
        assert len(memory(0, "list")["records"]) == 13
        assert len(memory(0, "recall")["records"]) == 1
        accepted = memory(0, "confirm", candidates[0]["id"], "--revision", "1")
        assert accepted["value"]["status"] == "confirmed"
        assert len(memory(0, "recall")["records"]) == 2
        print("PASS: 12 concurrent MCP writers retained; proposals need exact operator confirmation")

        # `down` preserves state; `up` restarts the actual pinned runtime.
        stopped = subprocess.run([str(BIN), "down", "-s", "memory"], cwd=root, env=env,
                                 text=True, capture_output=True, timeout=90)
        assert stopped.returncode == 0, stopped.stderr
        restarted = cli("up", "-s", "memory", "--no-ea", "--json")
        assert restarted["incarnation"] != session["incarnation"]
        assert memory(peer, "recall")["records"][0]["value"]["text"] == "Use three sections"
        print("PASS: runtime restart retains corrected project knowledge and revision history")

        memory(peer, "forget", record["id"], "--revision", "2")
        assert memory(peer, "recall")["records"] == []
        forgotten = memory(peer, "save", "--scope", "project", "--key", "report-format",
                           "--kind", "preference", "--text", "Use five sections",
                           "--source", "Operator request", ok=False)
        assert "forgotten" in forgotten
        serialized = (state / "knowledge/store.json").read_text()
        assert "Use five sections" not in serialized and "Use three sections" not in serialized
        print("PASS: forgetting removes old/current text and blocks retry resurrection")

        # A surviving EA requests daemon-owned deletion of the original chat.
        survivor_context = json.loads(context.read_text())
        survivor_context["ea_id"] = peer
        context.write_text(json.dumps(survivor_context))
        assert mcp(context, "delete_ea", dict(ea_id=0))["deleted_ea"] == 0
        stored = json.loads((state / "knowledge/store.json").read_text())
        assert "0" not in stored["links"]
        assert all(item["scope"] != dict(kind="ea", id=0) for item in stored["records"].values())
        assert len(stored["records"]) == 1, "only the shared tombstone survives"
        print("PASS: daemon-owned EA deletion removes private memories and project membership")
    finally:
        for session in reversed(sessions):
            subprocess.run([str(BIN), "rm", "-s", session["name"]], cwd=root, env=env,
                           text=True, capture_output=True, timeout=90)
