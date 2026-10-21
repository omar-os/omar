#!/usr/bin/env python3
"""Mock Slack event -> real isolated OMAR workflow -> captured thread reply.

No Slack credentials, external network calls, or model calls. Requires Node 22,
omar, omarc and tmux, as supplied by the CI Test job.
"""
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

with tempfile.TemporaryDirectory(prefix="omar-slack-mvp-") as folder:
    root = Path(folder)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("OMAR_", "SLACK_")) and k not in ("TMUX", "TMUX_PANE")}
    env.update(OMAR_HOME=str(root / "state"), OMARC_BIN=str(COMPILER), TERM="xterm-256color")

    def cli(*args):
        result = subprocess.run([str(BIN), *args], cwd=REPO, env=env,
                                text=True, capture_output=True, timeout=90)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout

    session = None
    try:
        session = json.loads(cli("up", "--name", "slack-mvp-test", "--no-ea", "--json"))
        with urllib.request.urlopen(session["url"] + "/v1/chat", timeout=5) as response:
            chat = json.load(response)
        config = json.loads((REPO / "integrations/slack/config.example.json").read_text())
        config.update(runtimeUrl=session["url"] + "/chats/" + chat["id"], workspaceId="T1",
                      allowedUsers=["U1"], allowedChannels=["C1"],
                      programPath=str(REPO / "integrations/slack/demo.omar"))
        config_path = root / "config.json"
        config_path.write_text(json.dumps(config))
        script = r"""
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const { loadConfig, OmarRuntime, WorkflowGateway } = await import(pathToFileURL(process.env.SLACK_TEST_REPO + '/integrations/slack/workflow.mjs'));
const { handleEnvelope } = await import(pathToFileURL(process.env.SLACK_TEST_REPO + '/integrations/slack/slack.mjs'));
const config = await loadConfig(process.env.SLACK_TEST_CONFIG);
let posts = 0;
const runtime = new OmarRuntime(config.runtimeUrl, { pollMs: 20, fetchImpl: (url, options) => {
  if (options.method === 'POST') posts++;
  return fetch(url, options);
}});
const replies = [], acks = [];
const gateway = new WorkflowGateway(config, { runtime,
  messenger: { async sendMessage(destination, text) { replies.push({ destination, text }); } },
  log: (line) => { throw new Error(line); },
});
const socket = { send(text) { acks.push(JSON.parse(text)); } };
const envelope = { type: 'events_api', envelope_id: 'env1', payload: {
  type: 'event_callback', team_id: 'T1', event_id: 'Ev1', event: {
    type: 'app_mention', user: 'U1', channel: 'C1', ts: '100.1', thread_ts: '90.1', text: '<@BOT> run demo'
  }
}};
await handleEnvelope(JSON.stringify(envelope), socket, 'BOT', gateway);
await handleEnvelope(JSON.stringify(envelope), socket, 'BOT', gateway);
assert.equal(posts, 1);
assert.equal(acks.length, 2);
assert.equal(replies.length, 2);
assert.deepEqual(replies[1].destination, { accountId: 'T1', conversationId: 'C1', threadId: '90.1' });
assert.match(replies[1].text, /completed\.\nSlack → OMAR → Slack: demo completed\./);
const runs = await runtime.call('/v1/runs');
assert.equal(runs.runs.length, 1);
assert.equal(runs.runs[0].status, 'completed');
assert.equal(runs.runs[0].outputs['run.result'], config.rawInputs[0].split('=').slice(1).join('='));
console.log('PASS: duplicate Slack envelopes -> one real OMAR run -> correct thread and output');
"""
        node_env = dict(env, SLACK_TEST_CONFIG=str(config_path), SLACK_TEST_REPO=str(REPO))
        result = subprocess.run(["node", "--input-type=module", "-"], input=script,
                                env=node_env, cwd=REPO, text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, (result.stdout, result.stderr)
        print(result.stdout.strip())
        # Exercise the actual research graph and imported enum with deterministic
        # backends. This verifies wiring/bundle delivery, not live news quality.
        research = json.loads((REPO / "integrations/slack/config.research.example.json").read_text())
        source = (REPO / "integrations/slack/research.omar").read_text().replace(": Codex", ": Stub")
        fixture = root / "research.omar"
        fixture.write_text(source)
        research.update(runtimeUrl=config["runtimeUrl"], workspaceId="T1",
                        allowedUsers=["U1"], allowedChannels=["C1"], programPath=str(fixture),
                        files={"schemas/research-status.json": str(REPO / "integrations/slack/schemas/research-status.json")})
        config_path.write_text(json.dumps(research))
        research_script = script.replace("run demo", "run research")
        research_script = research_script.replace(
            "assert.match(replies[1].text, /completed\\.\\nSlack → OMAR → Slack: demo completed\\./);",
            "assert.match(replies[1].text, /completed\\.\\nstub/);")
        research_script = research_script.replace("assert.equal(runs.runs.length, 1);", "assert.equal(runs.runs.length, 2); const reviewed = runs.runs.find((run) => run.team === 'SlackMarketResearch'); assert.ok(reviewed);")
        research_script = research_script.replace("runs.runs[0].status", "reviewed.status")
        research_script = research_script.replace("assert.equal(runs.runs[0].outputs['run.result'], config.rawInputs[0].split('=').slice(1).join('='));",
            "assert.equal(reviewed.outputs['edit.report'], 'stub'); assert.equal(reviewed.outputs['research.status'], 'complete'); assert.equal(reviewed.outputs['edit.status'], 'complete');")
        result = subprocess.run(["node", "--input-type=module", "-"], input=research_script,
                                env=node_env, cwd=REPO, text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, (result.stdout, result.stderr)
        print("PASS: research command -> two-agent graph + imported enum -> captured final report (fixture agents)")
    finally:
        if session:
            cli("down", "-s", session["name"])
            cli("rm", "-s", session["name"])
