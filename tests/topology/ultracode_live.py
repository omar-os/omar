#!/usr/bin/env python3
"""Live smoke test of Omar-backed ultracode, for the `omar-live` runner.

Starts a real Omar runtime, asks its Claude EA for a parallel 2-agent Claude
Code Workflow routed through the `omar` helper (one default backend, one
codex), and checks from disk that each workflow agent ran as an Omar worker
and left no Claude or Codex session history behind. Costs a few cents of
Claude and Codex quota. Needs claude and codex logged in, tmux, and a built
omar (OMAR_BIN, else `cargo build --bin omar` into target/debug).
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CLAUDE_PROJECTS = Path.home() / ".claude/projects"
CODEX_SESSIONS = Path.home() / ".codex/sessions"
DEADLINE_S = int(os.environ.get("ULTRACODE_TIMEOUT_SECONDS", "360"))
failures = []


def check(name, ok, detail=""):
    print(f"{'PASS' if ok else 'FAIL'} {name}" + (f": {detail}" if detail and not ok else ""), flush=True)
    if not ok:
        failures.append(name)


def files(root, pattern):
    return {str(p) for p in root.rglob(pattern) if p.is_file()} if root.exists() else set()


def top_level_transcripts(root):
    """Session transcripts directly under a project dir; stand-in ones sit deeper, under <session>/subagents/."""
    return {str(p) for p in root.glob("*/*.jsonl")} if root.exists() else set()


def stray_transcripts(root, slug_dir, before):
    """(top-level transcripts in slug_dir, new top-level transcripts elsewhere)."""
    now = top_level_transcripts(root)
    own = {p for p in now if Path(p).parent == slug_dir}
    return sorted(own), sorted(now - before - own)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def pane(tmux, session):
    return run(["tmux", "-L", tmux, "capture-pane", "-p", "-t", session]).stdout


def send(tmux, session, *keys, literal=False):
    run(["tmux", "-L", tmux, "send-keys", "-t", session, *(["-l"] if literal else []), *keys])


def wait_for(deadline, condition, what):
    while time.time() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(1)
    raise TimeoutError(f"timed out waiting for {what}")


def journal_results(journal):
    if not journal:
        return []
    lines = [json.loads(l) for l in journal.read_text().splitlines() if l.strip()]
    return [l for l in lines if l.get("type") == "result"]


def find_journal(slug_dir):
    found = sorted(slug_dir.glob("*/subagents/workflows/*/journal.jsonl")) if slug_dir.exists() else []
    return found[0] if found else None


def forget_trust(workdir):
    """Drop the folder-trust entry Claude added for the temp workdir."""
    path = Path.home() / ".claude.json"
    try:
        config = json.loads(path.read_text())
        if config.get("projects", {}).pop(str(workdir), None) is not None:
            tmp = path.with_suffix(f".json.{os.getpid()}")
            tmp.write_text(json.dumps(config, indent=2))
            tmp.replace(path)
    except (OSError, ValueError):
        pass


def main():
    omar = os.environ.get("OMAR_BIN")
    if not omar:
        subprocess.run(["cargo", "build", "--quiet", "--bin", "omar"], cwd=ROOT, check=True)
        omar = str(ROOT / "target/debug/omar")
    run_id = uuid.uuid4().hex[:8]
    session = f"ultracode-live-{run_id}"
    names = {"default": f"count-default-{run_id}", "codex": f"count-codex-{run_id}"}
    workdir = Path(tempfile.mkdtemp(prefix="omar-ultracode-")).resolve()
    slug_dir = CLAUDE_PROJECTS / re.sub(r"[^A-Za-z0-9]", "-", str(workdir))
    expected = len(list((ROOT / "src/backend").glob("*.rs")))
    claude_before = top_level_transcripts(CLAUDE_PROJECTS)
    codex_before = files(CODEX_SESSIONS, "*")
    # Export on its own line: every child below inherits it.
    env = {**os.environ, "OMAR_STATE_DIR": str(workdir)}
    env.pop("TMUX", None)
    env.pop("TMUX_PANE", None)
    os.environ.update(env)
    seen = {}  # worker tmux session -> [first, last] time seen
    stop = threading.Event()
    tmux_server = None

    def poll():
        while not stop.is_set():
            out = run(["tmux", "-L", tmux_server, "ls", "-F", "#{session_name}"]).stdout.split()
            for name in out:
                seen.setdefault(name, [time.time(), 0])[1] = time.time()
            stop.wait(1)

    poller = None
    try:
        started = time.time()
        subprocess.run([omar, "up", "--name", session, "--workdir", str(workdir)], check=True, stdout=subprocess.DEVNULL)
        tmux_server = json.loads(run([omar, "-s", session, "info", "--json"]).stdout)["session"]["tmux_server"]
        poller = threading.Thread(target=poll, daemon=True)
        poller.start()
        ea = "omar-agent-ea-0"
        deadline = time.time() + DEADLINE_S

        def ready():
            text = pane(tmux_server, ea)
            if "Yes, I trust this folder" in text:
                send(tmux_server, ea, "Down")
                time.sleep(0.5)
                send(tmux_server, ea, "Enter")
                return False
            return "bypass permissions" in text and "❯" in text

        wait_for(time.time() + 120, ready, "the EA prompt")
        schema = "{type:'object', properties:{count:{type:'integer'}}, required:['count']}"
        task = (f"Count the .rs files directly in {ROOT}/src/backend (not in subdirectories) "
                "and return {count: <integer>}.")
        send(tmux_server, ea, f"ultracode: Use the Workflow tool to run one script that starts two agents in "
             f"parallel with Promise.all, both through the omar helper: "
             f"omar('{names['default']}', '{task}', {{schema: {schema}}}) with no backend, and "
             f"omar('{names['codex']}', '{task}', {{backend: 'codex', schema: {schema}}}). "
             "Return both results.", literal=True)
        time.sleep(1)
        send(tmux_server, ea, "Enter")

        done_at = []

        def finished():
            journal = find_journal(slug_dir)
            if re.search(r"done \d+:\d\d", pane(tmux_server, ea)):
                done_at.append(time.time())
                # The EA may say "done" while the workflow still runs; allow it a grace period.
                return len(journal_results(journal)) >= 2 or time.time() - done_at[0] > 30
            return False

        try:
            wait_for(deadline, finished, "the EA to finish")
        except TimeoutError as err:
            check("EA finishes within the deadline", False, str(err))
        time.sleep(5)  # worker teardown and Codex thread deletion
        stop.set()
        poller.join()
        print(f"workflow ran in {time.time() - started:.0f}s")

        journal = find_journal(slug_dir)
        check("Claude workflow journal exists", journal is not None, f"nothing under {slug_dir}")
        results = journal_results(journal)
        labels = {}
        for line in (journal.read_text().splitlines() if journal else []):
            entry = json.loads(line)
            if entry.get("type") == "started":
                labels[entry["agentId"]] = entry["label"]
        by_name = {}
        for entry in results:
            label = labels.get(entry["agentId"], "")
            for kind, name in names.items():
                if label.startswith(name):
                    by_name[kind] = entry
        check("journal has both agents' results", len(by_name) == 2 and len(results) == 2, f"{results}")
        for kind, entry in by_name.items():
            meta = journal.parent / f"agent-{entry['agentId']}.meta.json"
            agent_type = json.loads(meta.read_text()).get("agentType") if meta.exists() else None
            check(f"{kind} agent ran as the omar stand-in", agent_type == "omar", f"agentType={agent_type}")
            check(f"{kind} agent result equals the true count ({expected})", entry["result"] == {"count": expected},
                  f"{entry['result']}")

        def spawn_backends(kind):
            entry = by_name.get(kind)
            path = journal.parent / f"agent-{entry['agentId']}.jsonl" if entry else None
            calls = []
            for line in path.read_text().splitlines() if path and path.exists() else []:
                content = json.loads(line).get("message", {}).get("content")
                for block in content if isinstance(content, list) else []:
                    if block.get("type") == "tool_use" and block["name"] == "mcp__omar-hold__spawn_agent":
                        calls.append(block["input"].get("backend"))
            return calls

        check("codex stand-in called spawn_agent with backend codex", "codex" in spawn_backends("codex"),
              f"{spawn_backends('codex')}")
        check("default stand-in called spawn_agent without backend codex",
              spawn_backends("default") and "codex" not in spawn_backends("default"), f"{spawn_backends('default')}")

        workers = {kind: seen.get(f"omar-agent-0-{name}") for kind, name in names.items()}
        check("poller saw both worker tmux sessions", all(workers.values()), f"saw {sorted(seen)}")
        if all(workers.values()):
            (a1, b1), (a2, b2) = workers.values()
            check("workers ran at the same time", max(a1, a2) <= min(b1, b2), f"{workers}")

        # Workers inherit the EA's workdir, so their transcripts would land in slug_dir too.
        own, extra = stray_transcripts(CLAUDE_PROJECTS, slug_dir, claude_before)
        check("only the EA's own Claude transcript in its project", len(own) == 1, f"{own}")
        check("no new Claude transcript elsewhere", not extra, f"{extra}")
        extra = files(CODEX_SESSIONS, "*") - codex_before
        check("no new Codex session file", not extra, f"{sorted(extra)}")
        if failures:
            print(f"--- EA pane ---\n{pane(tmux_server, ea)}")
    except Exception as err:  # noqa: BLE001
        check("test ran to completion", False, repr(err))
        if tmux_server:
            print(f"--- EA pane ---\n{pane(tmux_server, 'omar-agent-ea-0')}")
    finally:
        stop.set()
        subprocess.run([omar, "rm", "-s", session], capture_output=True)
        shutil.rmtree(workdir, ignore_errors=True)
        shutil.rmtree(slug_dir, ignore_errors=True)
        forget_trust(workdir)
    print(f"{'FAIL' if failures else 'PASS'}: {', '.join(failures) or 'all assertions held'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
