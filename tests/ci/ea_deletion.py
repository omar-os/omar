#!/usr/bin/env python3
"""Daemon-owned EA deletion through the real TUI, MCP, scheduler and topology VM."""
import concurrent.futures
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time
import urllib.error
import urllib.request

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
COMPILER = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()
assert COMPILER.is_file(), "build omarc first"

with tempfile.TemporaryDirectory(prefix="omar-delete-") as folder:
    root = Path(folder)
    env = {k: v for k, v in os.environ.items() if not k.startswith("OMAR_") and k not in ("TMUX", "TMUX_PANE")}
    env.update(OMAR_HOME=str(root / "state"), OMARC_BIN=str(COMPILER), TERM="xterm-256color")
    fake_bin = root / "bin"
    fake_bin.mkdir()
    fake = fake_bin / "claude"
    fake.write_text("#!/usr/bin/env python3\nimport time\nprint('Claude Code ready', flush=True)\nwhile True: time.sleep(1)\n")
    fake.chmod(0o700)
    env["PATH"] = str(fake_bin) + os.pathsep + env["PATH"]
    sessions = []
    terminal = None
    master = None
    current_session = None

    def cli(*args):
        result = subprocess.run([str(BIN), *args], env=env, cwd=root, text=True, capture_output=True, timeout=120)
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return json.loads(result.stdout) if result.stdout.lstrip().startswith(("{", "[")) else result.stdout

    def up(name):
        session = cli("-a", "claude", "up", "--name", name, "--no-ea", "--json")
        sessions.append(session)
        return session

    def rpc(session, **operation):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(100)
            stream.connect(session["socket"])
            stream.sendall(json.dumps(dict(protocol=session["protocol"], id=session["name"], incarnation=session["incarnation"], operation=operation)).encode() + b"\n")
            return json.loads(stream.makefile("r").readline())

    def http(session, path, body=None):
        request = urllib.request.Request(session["url"] + path, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def expect(text, timeout=15):
        data = b""
        deadline = time.monotonic() + timeout
        screen = ""
        while time.monotonic() < deadline:
            if current_session is not None:
                screen = tmux(current_session, "capture-pane", "-p", "-t", "=omar-dashboard:").stdout
                if text in screen:
                    return
            if select.select([master], [], [], .1)[0]:
                data += os.read(master, 65536)
                if text.encode() in data:
                    return
        raise AssertionError((text, screen, data[-500:]))

    def attach(session, ea=None):
        global terminal, master, current_session
        current_session = session
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
        args = [str(BIN), "attach", "-s", session["name"], "--tui"]
        if ea is not None:
            args += ["--ea", str(ea)]
        terminal = subprocess.Popen(args, env=env, stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        expect("OMAR " + session["name"])

    def detach():
        global terminal, master
        os.write(master, b"z")
        deadline = time.monotonic() + 15
        while terminal.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                try:
                    os.read(master, 65536)
                except OSError:
                    break
        assert terminal.wait(timeout=5) == 0
        os.close(master)
        master = terminal = None

    def tmux(session, *args):
        return subprocess.run(["tmux", "-L", session["tmux_server"], *args], capture_output=True, text=True)

    def ready(session):
        record = cli("info", "-s", session["name"])["session"]
        assert (record["pid"], record["incarnation"], record["state"]) == (session["pid"], session["incarnation"], "ready"), record

    def wait_run(session, run, expected):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            status = cli("-s", session["name"], "status", run)["status"]
            if status == expected:
                return
            assert status != "failed", status
            time.sleep(.1)
        raise AssertionError((run, expected, status))

    def mcp(session, actor, name, arguments):
        context = Path(session["directory"]) / f"mcp/ea-{actor}/context.json"
        # Deliberately inherit another session; the context's owner must win.
        routing = dict(env, OMAR_SESSION_ID="inner", OMAR_STATE_DIR=sessions[1]["directory"])
        request = dict(jsonrpc="2.0", id=1, method="tools/call", params=dict(name=name, arguments=arguments))
        result = subprocess.run([str(BIN), "mcp-server", "--context-file", str(context)], env=routing,
                                input=json.dumps(request) + "\n", text=True, capture_output=True, timeout=100)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)["result"]

    try:
        outer, inner = up("outer"), up("inner")
        state = Path(outer["directory"])
        assert "only EA" in rpc(outer, op="delete_ea", ea="0")["error"]
        assert "not found" in rpc(outer, op="delete_ea", ea="99")["error"]
        review = cli("-s", "outer", "ea", "create", "--name", "review")["id"]
        cli("-s", "outer", "ea", "start", "review")
        cli("-s", "outer", "--ea", "review", "spawn", "-n", "worker", "-c", "claude")
        cli("-s", "inner", "ea", "start", "0")
        for ea in (0, review):
            cli("-s", "outer", "--ea", str(ea), "ea", "event", "schedule", "--receiver", "ea", "--payload", "retained event", "--in-seconds", "3600")
        note = state / f"manager_notes_ea{review}.md"
        note.write_text("remove selected EA notes")
        attach(outer, review)
        os.write(master, b"D")
        expect("Delete this EA?")
        os.write(master, b"n")
        time.sleep(.2)
        assert len(cli("-s", "outer", "ea", "list")) == 2
        os.write(master, b"D")
        expect("Delete this EA?")
        os.write(master, b"y")
        expect("Deleted EA", timeout=35)
        assert not (state / f"ea/{review}").exists()
        assert not (state / f"mcp/ea-{review}").exists()
        assert not note.exists()
        assert json.loads((state / "active_ea").read_text()) == 0
        assert not any(event["ea_id"] == review for event in json.loads((state / "scheduled_events.json").read_text()))
        assert tmux(outer, "has-session", "-t", f"=omar-agent-ea-{review}").returncode != 0
        assert tmux(outer, "has-session", "-t", f"=omar-agent-{review}-worker").returncode != 0
        ready(outer)
        ready(inner)
        assert tmux(inner, "has-session", "-t", "=omar-agent-ea-0").returncode == 0
        os.write(master, b"D")
        expect("Cannot delete the only EA")
        detach()
        # Real active, paused and completed topologies are owned by the daemon.
        active = cli("-s", "outer", "ea", "create", "--name", "running")["id"]
        program = root / "clock.omar"
        program.write_text('team Clock { timer tick(0, 1s) output stamp : int reaction(tick) -> stamp {= stamp = Some(1); =} }\nmain ClockRun { clock = Clock() }\n')
        cli("-s", "outer", "ea", "start", str(active))
        run = cli("-s", "outer", "--ea", str(active), "start", str(program))
        assert run["status"] in ("starting", "running")
        # Hold a real cross-process admission while deletion begins. HTTP and
        # MCP must reject the retiring EA before that request has drained.
        chats = http(outer, "/v1/chats")[1]["conversations"]
        target_chat = next(chat["id"] for chat in chats if chat["ea_id"] == active)
        surviving_chat = next(chat["id"] for chat in chats if chat["ea_id"] == 0)
        with (state / f"ea-lifecycle/{active}.lock").open("r+") as held:
            fcntl.flock(held, fcntl.LOCK_SH)
            with concurrent.futures.ThreadPoolExecutor() as pool:
                deletion = pool.submit(rpc, outer, op="delete_ea", ea=str(active))
                try:
                    deadline = time.monotonic() + 5
                    while not (state / f"ea-lifecycle/{active}.json").exists():
                        assert time.monotonic() < deadline
                        time.sleep(.01)
                    assert not deletion.done()
                    creation = pool.submit(rpc, outer, op="create_ea", name="concurrent")
                    assert http(outer, f"/chats/{target_chat}/v1/chat")[0] >= 400
                    assert mcp(outer, active, "list_agents", {})["isError"]
                    assert http(outer, f"/chats/{surviving_chat}/v1/chat")[0] == 200
                    assert not mcp(outer, 0, "list_agents", {}).get("isError")
                finally:
                    fcntl.flock(held, fcntl.LOCK_UN)
                assert "result" in deletion.result(timeout=60)
                created = creation.result(timeout=20)["result"]["id"]
                assert "result" in rpc(outer, op="delete_ea", ea=str(created))
        assert not (state / f"ea/{active}").exists()
        ready(outer)
        for status in ("paused", "completed"):
            target = cli("-s", "outer", "ea", "create", "--name", status)["id"]
            if status == "paused":
                parked = cli("-s", "outer", "--ea", str(target), "start", str(program))
                cli("-s", "outer", "--ea", str(target), "pause", parked["run_id"], "--wait")
            else:
                once = root / "once.omar"
                once.write_text('team Once { timer tick(1ns, 0) output done : int reaction(tick) -> done {= done = Some(1); =} }\nmain OnceRun { once = Once() }\n')
                cli("-s", "outer", "--ea", str(target), "start", str(once), "--fg")
            assert "result" in rpc(outer, op="delete_ea", ea=str(target))
            assert not (state / f"ea/{target}").exists()
            ready(outer)
        # Fail registry commit after resources are staged, crash the daemon,
        # then verify startup completes the journal without losing other EAs.
        fault = cli("-s", "outer", "ea", "create", "--name", "faulted")["id"]
        fault_state = state / f"ea/{fault}"
        fault_state.mkdir(exist_ok=True)
        (fault_state / "sentinel").write_text("staged state")
        registry_lock = state / "eas.lock"
        saved_lock = state / "eas.lock.saved"
        registry_lock.rename(saved_lock)
        registry_lock.mkdir()
        try:
            failed = rpc(outer, op="delete_ea", ea=str(fault))
            assert "error" in failed, failed
            assert any(ea["id"] == fault for ea in json.loads((state / "eas.json").read_text()))
            assert (state / f"ea-lifecycle/deleted-{fault}/0/sentinel").read_text() == "staged state"
        finally:
            registry_lock.rmdir()
            saved_lock.rename(registry_lock)
        os.kill(outer["pid"], signal.SIGKILL)
        time.sleep(.2)
        outer.update(cli("up", "--name", "outer", "--no-ea", "--json"))
        assert all(ea["id"] != fault for ea in cli("-s", "outer", "ea", "list"))
        assert json.loads((state / f"ea-lifecycle/{fault}.json").read_text())["complete"]
        ready(outer)
        ready(inner)
        # EA 0 deletion leaves the shared history and the other EA's endpoints.
        survivor = cli("-s", "outer", "ea", "create", "--name", "survivor")["id"]
        assert survivor > active
        cli("-s", "outer", "ea", "start", str(survivor))
        survivor_run = cli("-s", "outer", "--ea", str(survivor), "start", str(program))["run_id"]
        chats = http(outer, "/v1/chats")[1]["conversations"]
        chat = next(chat["id"] for chat in chats if chat["ea_id"] == survivor)
        context = json.loads((state / f"mcp/ea-{survivor}/context.json").read_text())
        assert http(outer, f"/chats/{chat}/v1/agent/reply", dict(token=context["serve"]["token"], text="survivor history"))[0] == 202
        cli("-s", "outer", "--ea", str(survivor), "ea", "event", "schedule", "--receiver", "ea", "--payload", "survivor", "--in-seconds", "3600")
        result = mcp(outer, survivor, "delete_ea", dict(ea_id=0))
        assert not result.get("isError"), result
        assert [ea["id"] for ea in cli("-s", "outer", "ea", "list")] == [survivor]
        assert not (state / "ea/0").exists()
        wait_run(outer, survivor_run, "running")
        assert tmux(outer, "has-session", "-t", f"=omar-agent-ea-{survivor}").returncode == 0
        assert http(outer, f"/chats/{chat}/v1/chat")[1]["messages"][0]["text"] == "survivor history"
        assert any(event["ea_id"] == survivor for event in json.loads((state / "scheduled_events.json").read_text()))
        ready(outer)
        attach(outer)
        detach()
        cli("down", "-s", "outer")
        restarted = cli("up", "--name", "outer", "--no-ea", "--json")
        outer.update(restarted)
        wait_run(outer, survivor_run, "paused")
        cli("-s", "outer", "resume", survivor_run)
        wait_run(outer, survivor_run, "running")
        assert http(outer, f"/chats/{chat}/v1/chat")[1]["messages"][0]["text"] == "survivor history"
        assert not (state / "ea/0").exists()
        attach(outer)
        detach()
        assert not (state / "ea/0").exists()
        ready(inner)
        print("PASS: D confirmation/cancellation, daemon isolation, live topology teardown, event/MCP cleanup, EA0 history and restart")
    finally:
        if terminal is not None:
            terminal.kill()
            terminal.wait()
        if master is not None:
            os.close(master)
        for session in sessions:
            subprocess.run([str(BIN), "down", "-s", session["name"], "--force", "--timeout", "5"], env=env, capture_output=True, timeout=15)
            tmux(session, "kill-server")
