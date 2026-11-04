#!/usr/bin/env python3
"""Rust messaging gateway -> real isolated OMAR runs. No Node, credentials or models."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
COMPILER = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()
assert BIN.is_file() and COMPILER.is_file(), "build omar and omarc first"

with tempfile.TemporaryDirectory(prefix="omar-rust-messaging-") as folder:
    root = Path(folder)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("OMAR_", "SLACK_")) and k not in ("TMUX", "TMUX_PANE")}
    (root / "xdg").mkdir(mode=0o700)
    env.update(OMAR_HOME=str(root / "state"), OMARC_BIN=str(COMPILER), TERM="xterm-256color",
               XDG_RUNTIME_DIR=str(root / "xdg"))

    def cli(*args):
        result = subprocess.run([str(BIN), *args], cwd=REPO, env=env,
                                text=True, capture_output=True, timeout=90)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    def get(path):
        with urllib.request.urlopen(runtime_url + path, timeout=10) as response:
            return json.load(response)

    def exercise(config, expected, count):
        config_path = root / "config.json"
        config_path.write_text(json.dumps(config))
        test_env = dict(env, OMAR_MESSAGING_TEST_CONFIG=str(config_path),
                        OMAR_MESSAGING_TEST_OUTPUT=expected)
        result = subprocess.run([
            "cargo", "test", "--locked", "--bin", "omar", "messaging::tests::real_runtime",
            "--", "--exact", "--ignored", "--nocapture",
        ], cwd=REPO, env=test_env, text=True, capture_output=True, timeout=180)
        assert result.returncode == 0, (result.stdout, result.stderr)
        if config.get("autoChat"):
            resolved = next(line.split("=", 1)[1] for line in result.stdout.splitlines() if line.startswith("AUTO_CHAT_URL="))
            with urllib.request.urlopen(resolved + "/v1/runs", timeout=10) as response:
                runs = json.load(response)["runs"]
        else:
            runs = get("/v1/runs")["runs"]
        assert len(runs) == count, runs
        assert all(run["status"] == "completed" for run in runs), runs
        return runs

    session = None
    try:
        session = json.loads(cli("up", "--name", "rust-messaging-test", "--no-ea", "--json"))
        runtime_url = session["url"]
        chat = get("/v1/chat")
        runtime_url += "/chats/" + chat["id"]
        config = json.loads((REPO / "integrations/slack/config.example.json").read_text())
        config.update(runtimeUrl=runtime_url, workspaceId="T1", allowedUsers=["U1"],
                      allowedChannels=["C1"], programPath=str(REPO / "integrations/slack/demo.omar"))
        expected = config["rawInputs"][0].split("=", 1)[1]
        exercise(config, expected, 1)
        print("PASS: Rust Slack normalization -> duplicate event -> one real demo run -> correct thread/output")

        research = json.loads((REPO / "integrations/slack/config.research.example.json").read_text())
        source = (REPO / "integrations/slack/research.omar").read_text().replace(": Codex", ": Stub")
        fixture = root / "research.omar"
        fixture.write_text(source)
        research.update(runtimeUrl=runtime_url, workspaceId="T1", allowedUsers=["U1"],
                        allowedChannels=["C1"], programPath=str(fixture),
                        files={"schemas/research-status.json": str(REPO / "integrations/slack/schemas/research-status.json")})
        runs = exercise(research, "stub", 2)
        reviewed = next(run for run in runs if run["team"] == "SlackMarketResearch")
        assert reviewed["outputs"]["research.status"] == "complete"
        assert reviewed["outputs"]["edit.status"] == "complete"
        print("PASS: Rust gateway -> two-agent research graph + imported enum -> final report (Stub agents)")

        neutral = dict(runtimeUrl=runtime_url, platform="mock-chat", accountId="mock-account",
                       allowedUsers=["mock-user"], allowedConversations=["mock-room"], command="demo",
                       outputPort="run.result", timeoutSeconds=30,
                       programPath=str(REPO / "integrations/slack/demo.omar"),
                       rawInputs=["run.request=Mock adapter -> Rust core -> OMAR"])
        exercise(neutral, "Mock adapter -> Rust core -> OMAR", 3)
        print("PASS: mock adapter -> Rust shared core -> real OMAR -> threadless reply")

        discord = dict(runtimeUrl=runtime_url, platform="discord", accountId="999",
                       allowedUsers=["123"], command="demo", outputPort="run.result",
                       programPath=str(REPO / "integrations/slack/demo.omar"),
                       rawInputs=["run.request=Discord DM -> Rust core -> OMAR"])
        exercise(discord, "Discord DM -> Rust core -> OMAR", 4)
        print("PASS: Discord DM normalization -> duplicate -> one real OMAR run -> reply destination")

        # One gateway, one registry, sequentially execute three different real graphs.
        workflow_keys = ["programPath", "outputPort", "timeoutSeconds", "files", "inputs", "rawInputs"]
        registry = {key: config[key] for key in ["runtimeUrl", "workspaceId", "allowedUsers", "allowedChannels"]}
        registry["workflows"] = {
            "demo": {key: value for key, value in config.items() if key in workflow_keys},
            "research": {key: value for key, value in research.items() if key in workflow_keys},
        }
        checklist = json.loads((REPO / "integrations/slack/config.workflows.example.json").read_text())["workflows"]["checklist"]
        checklist["programPath"] = str(REPO / "integrations/slack/checklist.omar")
        registry["workflows"]["checklist"] = checklist
        registry_path = root / "registry.json"
        registry_path.write_text(json.dumps(registry))
        registry_env = dict(env, OMAR_MESSAGING_TEST_CONFIG=str(registry_path),
                            OMAR_MESSAGING_REGISTRY_EXPECTED=json.dumps({"demo": expected, "research": "stub", "checklist": checklist["rawInputs"][0].split("=", 1)[1]}))
        result = subprocess.run([
            "cargo", "test", "--locked", "--bin", "omar", "messaging::tests::real_registry",
            "--", "--exact", "--ignored", "--nocapture",
        ], cwd=REPO, env=registry_env, text=True, capture_output=True, timeout=180)
        assert result.returncode == 0, (result.stdout, result.stderr)
        runs = get("/v1/runs")["runs"]
        assert len(runs) == 7, runs
        assert all(run["status"] == "completed" for run in runs), runs
        print("PASS: one Rust registry/gateway -> list -> newly added checklist + demo + research sequentially; duplicates suppressed")

        for platform in ("slack", "discord"):
            automatic = dict(runtimeUrl=runtime_url.split("/chats/")[0], autoChat=True,
                             platform=platform, accountId="999", allowedUsers=["123"],
                             allowedConversations=["456"] if platform == "slack" else [],
                             command="demo", outputPort="run.result", timeoutSeconds=30,
                             programPath=str(REPO / "integrations/slack/demo.omar"),
                             rawInputs=["run.request=Automatic dedicated chat works"])
            exercise(automatic, "Automatic dedicated chat works", 1)
            exercise(automatic, "Automatic dedicated chat works", 2)
            print(f"PASS: {platform} automatic dedicated chat -> real workflow -> reconnect reuses chat")
        assert get("/v1/chat")["id"] == chat["id"], "Automatic setup changed the UI's selected chat"

        def dedicated_ids():
            result = {}
            for platform in ("slack", "discord"):
                body = json.dumps({"platform": platform, "accountId": "999"}).encode()
                request = urllib.request.Request(runtime_url.split("/chats/")[0] + "/v1/chats/messaging", data=body,
                                                 headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(request, timeout=10) as response:
                    result[platform] = json.load(response)["id"]
            return result
        before_restart = dedicated_ids()
        cli("down", "-s", session["name"])
        socket_parent = Path(session["socket"]).parent
        socket_parent.rename(socket_parent.with_name(socket_parent.name + "-before-restart"))
        session = json.loads(cli("up", "--name", session["name"], "--no-ea", "--json"))
        assert socket_parent.is_dir(), "Runtime did not recreate its temporary socket directory"
        assert dedicated_ids() == before_restart, "Dedicated chats changed after runtime restart"
        print("PASS: deleted temporary socket directory -> named runtime restart -> same port and dedicated chat IDs")

    finally:
        if session:
            cli("down", "-s", session["name"])
            cli("rm", "-s", session["name"])
