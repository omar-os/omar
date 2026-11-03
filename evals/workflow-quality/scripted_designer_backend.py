#!/usr/bin/env python3
"""Native-protocol test double. No provider imports, network clients or model calls.

Invoked only by verify_designer_runtime.py from a restricted test PATH. Uses
Omar's real MCP sidecar and loopback daemon with authored proposals and outputs.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

if sys.argv[1:] == ["login", "status"]:
    print("ChatGPT scripted authentication fixture (not actual access)"); raise SystemExit()
if "--version" in sys.argv:
    print("codex scripted-designer-fixture-v1"); raise SystemExit()

sys.path.insert(0, os.environ["OMAR_DESIGNER_FIXTURE_ROOT"])
from designer_journey import ANSWER, BRIEF, reference
from program import build

prompt = sys.stdin.read()
context_path = Path(os.environ["OMAR_MCP_CONTEXT_FILE"])
context = json.loads(context_path.read_text())
fixture_dir = Path(os.environ["OMAR_DESIGNER_FIXTURE_DIR"])

def record_exception(kind, error, traceback):
    import traceback as trace
    with (fixture_dir / "scripted-errors.log").open("a") as stream:
        trace.print_exception(kind, error, traceback, file=stream)
    sys.__excepthook__(kind, error, traceback)
sys.excepthook = record_exception


def rpc(method, params):
    response = subprocess.run([os.environ["OMAR_DESIGNER_FIXTURE_BINARY"], "mcp-server", "--context-file", str(context_path)],
        input=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n", capture_output=True, text=True, timeout=20, check=True)
    return json.loads(response.stdout)["result"]


def tool(name, arguments=None):
    value = rpc("tools/call", {"name": name, "arguments": arguments or {}})
    if value.get("isError"): raise RuntimeError(f"{name}: {value}")
    return value["structuredContent"]


print(json.dumps({"type": "thread.started", "thread_id": "scripted-native-designer-session"}), flush=True)
if context.get("topology"):
    pending = tool("omar_pending")["pending"]
    if len(pending) != 1: raise ValueError(f"Expected one pending fixture invocation: {pending}")
    invocation = pending[0]
    index = int(invocation["reaction"].rsplit(".", 1)[1])
    result = json.dumps({"decision": "proceed", "recommendation": "Proceed: the score meets policy and the review is approved.", "claims": [{"text": "The score is 92.", "source_ids": ["measurement"]}, {"text": "The final review approved release.", "source_ids": ["review"]}], "limitations": ["Only the supplied release notes were checked."], "audience": "external_customers"})
    port, value = {0: ("flow.evidence", "Supplied policy, score and approval."), 1: ("flow.raw", result), 2: ("flow.result", result)}[index]
    tool("omar_set_port", {"invocation_id": invocation["invocation_id"], "port": port, "value": value})
    tool("omar_complete", {"invocation_id": invocation["invocation_id"]})
else:
    listed = {t["name"] for t in rpc("tools/list", {})["tools"]}
    if len(listed) != 6 or "spawn_agent" in listed: raise ValueError("Design scope not enforced")
    refused = rpc("tools/call", {"name": "spawn_agent", "arguments": {}})
    if not refused.get("isError") or "design-only" not in json.dumps(refused): raise ValueError("Direct management call was not refused")
    catalog = tool("omar_get_execution_profiles")
    tool("omar_get_capability_evidence")
    usage = catalog["research"]
    tool("omar_research_workflow", {"session_id": usage["session_id"], "request_id": "read-request", "lookup": {"kind": "operator_message", "message_sequence": usage["operator_sequence"]}})
    task = {"id": "guided-designer-fixture", "family": "document_synthesis", "instruction": "Explain whether to proceed with the release", **json.loads(BRIEF.split("\n", 1)[1])}
    proposal = build(task, "same_model_workflow", "codex-quality", "codex-quality")
    proposal["plan"]["intent"]["message_sequences"] = [usage["operator_sequence"]]
    review = re.search(r"Workflow review context: read plan ([^,]+), revision (\d+)", prompt)
    if review:
        saved = tool("omar_get_workflow_plan", {"id": review[1], "revision": int(review[2])})
        proposal["plan"]["revises"] = reference(saved)
        for step, old in zip(proposal["plan"]["steps"], saved["draft"]["steps"]): step["logical_id"] = old["logical_id"]
        proposal["program"] = proposal["program"].replace("Write raw as the complete requested JSON", "Set audience to external_customers; omit internal reviewer names. Write raw as the complete requested JSON")
        proposal["plan"]["intent"]["constraints"].append(ANSWER)
        proposal["plan"]["intent"]["requirements"] = [{"id": "audience", "text": ANSWER, "origin": {"kind": "user_confirmed", "confirmation": {"kind": "review_answer", "plan": reference(saved), "question_id": "audience", "answer": ANSWER}}}]
    else:
        proposal["plan"]["questions"] = [{"id": "audience", "question": "Who is the intended audience?", "reason": "The audience changes disclosure and terminology.", "target_reactions": ["flow.reaction.1"], "options": ["Internal team", "External customers"], "required": True}]
    tool("omar_propose_design", proposal)
print(json.dumps({"type": "turn.completed", "usage": None}), flush=True)
