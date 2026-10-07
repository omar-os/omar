#!/usr/bin/env python3
"""Login-free session ownership, nesting, readiness, client and shutdown contracts."""
import concurrent.futures
import hashlib
import fcntl
import struct
import termios
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import socket
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("OMAR_BIN", REPO / "target/debug/omar")).resolve()
COMPILER = Path(os.environ.get("OMARC_BIN", REPO / "lang/.lake/build/bin/omarc")).resolve()
assert COMPILER.is_file(), "build omarc before this test"

with tempfile.TemporaryDirectory(prefix="omar-sessions-") as folder:
    root = Path(folder)
    env = {k: v for k, v in os.environ.items() if not k.startswith("OMAR_") and k not in ("TMUX", "TMUX_PANE")}
    env.update(OMAR_HOME=str(root / "state"), OMARC_BIN=str(COMPILER), TERM="xterm-256color")
    sessions = []

    def cli(*args, ok=True, context=None):
        result = subprocess.run([str(BIN), *args], cwd=root, env=context or env,
                                text=True, capture_output=True, timeout=90)
        assert (result.returncode == 0) == ok, (args, result.stdout, result.stderr)
        return result.stdout if ok else result.stderr

    def up(name, context=None, checkpoint=False):
        result = json.loads(cli("up", "--name", name, "--no-ea", "--json", *(["--checkpoint"] if checkpoint else []), context=context))
        sessions.append(result)
        assert result["state"] == "ready"
        assert hashlib.sha256(Path(result["executable"]).read_bytes()).hexdigest() == result["build_id"]
        return result

    def info(name):
        return json.loads(cli("info", "-s", name))

    def wait_status(session, run, expected):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            state = json.loads(cli("--session", session, "status", run))["status"]
            if state == expected:
                return
            assert state != "failed", info(session)
            time.sleep(.1)
        raise AssertionError((expected, state))

    def terminal_attach(name, context=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
        process = subprocess.Popen([str(BIN), "attach", "-s", name, "--tui"], env=context or env, stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        return process, master

    def terminal_exit(process, master, timeout=15):
        # Keep draining the pty: a blocked write would stall the tmux client.
        deadline = time.monotonic() + timeout
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                try:
                    os.read(master, 65536)
                except OSError:
                    break
        return process.wait(timeout=5)

    def terminal_expect(master, text):
        data = b""
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                data += os.read(master, 65536)
                if text.encode() in data:
                    return
        raise AssertionError((text, data[-2000:]))

    try:
        for args in [("--help",), ("ea", "--help"), ("workspace", "--help"), ("ea", "event", "schedule", "--help")]:
            cli(*args)
        assert not Path(env["OMAR_HOME"]).exists(), "help wrote runtime state"
        cli("runs", ok=False)
        bare = subprocess.run([str(BIN)], cwd=root, env=env, text=True, capture_output=True)
        assert bare.returncode != 0 and "Usage:" in bare.stdout + bare.stderr, "bare omar must print the command list"
        outer = up("outer")
        inherited = dict(env, OMAR_SESSION_ID=outer["name"], OMAR_STATE_DIR=outer["directory"],
                         OMAR_TMUX_SERVER=outer["tmux_server"], OMAR_EA_ID="99", TMUX="parent")
        inner = up("inner", inherited)
        assert outer["url"] != inner["url"] and outer["socket"] != inner["socket"]
        assert cli("attach", "-s", "outer", "--web", "--print-url").strip() == outer["url"]
        assert "--tui" in cli("attach", "-s", "outer", ok=False) or "--web" in cli("attach", "-s", "outer", ok=False)
        assert outer["tmux_server"] != inner["tmux_server"]
        assert len(json.loads(cli("ls", "--json"))) == 2
        cli("up", "--name", "outer", "--no-ea", ok=False)
        cli("--session", "inner", "ea", "list", context=inherited)
        assert json.loads(cli("--session", "outer", "ea", "create", "--name", "Second"))["id"] == 1
        assert len(info("inner")["eas"]) == 1
        assert len(info("outer")["eas"]) == 2
        # Forwarded operations stay in the daemon and respect compact -s syntax.
        cli("-sinner", "ea", "event", "list")
        # Same-name startup is serialized before either daemon is ready.
        with concurrent.futures.ThreadPoolExecutor() as pool:
            results = list(pool.map(lambda _: subprocess.run([str(BIN), "up", "--name", "race", "--no-ea", "--json"],
                                  env=env, text=True, capture_output=True, timeout=90), range(2)))
        assert sorted(p.returncode == 0 for p in results) == [False, True]
        sessions.append(json.loads(next(p.stdout for p in results if p.returncode == 0)))
        cli("down", "-s", "race")
        # Without --checkpoint a stopped session is gone, like a tmux session.
        assert "race" not in {s["name"] for s in json.loads(cli("ls", "--json"))}
        kept = up("kept", checkpoint=True)
        cli("down", "-s", "kept")
        assert info("kept")["session"]["state"] == "stopped" and Path(kept["directory"]).is_dir()
        # Foreground serve honours --json for its startup record.
        foreground = subprocess.Popen([str(BIN), "--json", "serve", "--name", "fg", "--no-ea"], cwd=root, env=env,
                                      text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        lines = []
        for line in foreground.stdout:
            lines.append(line)
            if line.rstrip() == "}": break
        started = json.loads("".join(lines))
        sessions.append(started)
        assert started["name"] == "fg" and started["state"] == "ready", started
        cli("down", "-s", "fg")
        assert foreground.wait(timeout=30) == 0
        # Occupied endpoint fails readiness instead of advertising a dead URL.
        with socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0)); occupied.listen()
            error = cli("up", "--name", "conflict", "--no-ea", "--address",
                        f"127.0.0.1:{occupied.getsockname()[1]}", ok=False)
            assert "startup failed" in error and "runtime.log" in error
        assert info("conflict")["session"]["state"] == "failed"
        # A separately built runtime selects its own sibling compiler, even
        # inside a parent that exports its pinned OMARC_BIN.
        with tempfile.TemporaryDirectory(prefix="omar-build-") as build:
            built = Path(build)
            shutil.copyfile(BIN, built / "omar")
            (built / "omar").chmod(0o700)
            marker = "#!/bin/sh\necho selected-build-compiler\n"
            (built / "omarc").write_text(marker)
            (built / "omarc").chmod(0o700)
            context = dict(inherited, OMARC_BIN=str(Path(outer["directory"]) / "bin/omarc"))
            output = subprocess.run([str(built / "omar"), "up", "--name", "build", "--no-ea", "--json"],
                                    cwd=root, env=context, text=True, capture_output=True, timeout=90)
            assert output.returncode == 0, output.stderr
            selected = json.loads(output.stdout)
            sessions.append(selected)
            assert (Path(selected["directory"]) / "bin/omarc").read_text() == marker
            cli("down", "-s", "build")
        # Browser disconnection no longer shuts down a managed runtime after 10s.
        host, port = inner["url"].removeprefix("http://").split(":")
        with socket.create_connection((host, int(port))) as browser:
            browser.sendall(b"GET /v1/chat/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
            assert b"200 OK" in browser.recv(4096)
        # A running timer topology keeps doing useful work independently of clients.
        program = root / "clock.omar"
        program.write_text('team Clock { timer tick(0, 1s) output stamp : int reaction(tick) -> stamp {= stamp = Some(1); =} }\nmain ClockRun { clock = Clock() }\n')
        once = root / "once.omar"
        once.write_text('team Once { timer tick(1ns, 0) output done : int reaction(tick) -> done {= done = Some(1); =} }\nmain OnceRun { once = Once() }\n')
        # --wait prints exactly one structured result: the terminal record.
        assert json.loads(cli("--json", "--session", "outer", "start", str(once), "--wait"))["status"] == "completed"
        # Without a selected runtime, run creates a session, prints the summary, and shuts it down.
        summary = subprocess.run([str(BIN), "run", str(once), "--wait"], cwd=root, env=env, text=True, capture_output=True, timeout=120)
        assert summary.returncode == 0 and "Topology 'OnceRun' completed" in summary.stdout and "Output once.done = 1" in summary.stdout, summary
        created = re.search(r"Started session (\S+);", summary.stderr)
        assert created, summary.stderr
        assert created.group(1) not in {s["name"] for s in json.loads(cli("ls", "--json"))}, "run --wait left its own session behind"
        run_a = json.loads(cli("--session", "outer", "start", str(program)))["run_id"]
        run_b = json.loads(cli("--session", "inner", "start", str(program)))["run_id"]
        wait_status("outer", run_a, "running")
        wait_status("inner", run_b, "running")
        # The terminal dashboard attaches as a client inside the session's tmux
        # server; z detaches, the dashboard and everything the runtime owns go on.
        terminal, master = terminal_attach("outer")
        try:
            terminal_expect(master, "OMAR outer")
            os.write(master, b"z")
            assert terminal_exit(terminal, master) == 0
        finally:
            if terminal.poll() is None: terminal.kill(); terminal.wait()
            os.close(master)
        assert subprocess.run(["tmux", "-L", outer["tmux_server"], "has-session", "-t", "=omar-dashboard"],
                              capture_output=True).returncode == 0, "z stopped the dashboard"
        # From a workload pane on the session's own tmux server, attach joins the
        # dedicated dashboard session instead of running a second dashboard there.
        tmux = lambda *args: subprocess.run(["tmux", "-L", outer["tmux_server"], *args], capture_output=True, text=True)
        assert tmux("new-session", "-d", "-s", "worker-pane", "sh").returncode == 0
        pane = tmux("display-message", "-p", "-t", "worker-pane", "#{pane_id}").stdout.strip()
        in_pane = dict(env, TMUX=f"/tmp/tmux-{os.getuid()}/{outer['tmux_server']},0,0", TMUX_PANE=pane)
        terminal, master = terminal_attach("outer", in_pane)
        try:
            terminal_expect(master, "OMAR outer")
            os.write(master, b"z")
            assert terminal_exit(terminal, master) == 0
        finally:
            if terminal.poll() is None: terminal.kill(); terminal.wait()
            os.close(master)
        assert "omar" not in tmux("list-panes", "-t", "worker-pane", "-F", "#{pane_current_command}").stdout
        assert tmux("list-sessions", "-F", "#{session_name}").stdout.split().count("omar-dashboard") == 1
        time.sleep(11)
        assert info("inner")["session"]["state"] == "ready"
        wait_status("outer", run_a, "running")
        # Q in an attached dashboard stops that session only, after confirmation.
        terminal, master = terminal_attach("inner")
        try:
            terminal_expect(master, "OMAR inner")
            os.write(master, b"Q")
            terminal_expect(master, "Stop this session?")
            os.write(master, b"y")
            terminal_exit(terminal, master)
        finally:
            if terminal.poll() is None: terminal.kill(); terminal.wait()
            os.close(master)
        record = Path(env["OMAR_HOME"]) / "registry" / f"{inner['name']}.json"
        deadline = time.monotonic() + 20
        # A session without a checkpoint removes its record as it stops.
        while record.exists() and json.loads(record.read_text())["state"] != "stopped":
            assert time.monotonic() < deadline, record.read_text()
            time.sleep(.2)
        wait_status("outer", run_a, "running")
        assert "inner" not in {s["name"] for s in json.loads(cli("ls", "--json"))}
        cli("--session", "outer", "stop", run_a)
        wait_status("outer", run_a, "stopped")
        assert info("outer")["session"]["state"] == "ready"
        # Assistants receive the owned HTTP/MCP context, including newly added EAs.
        fake_bin = root / "fake-bin"
        fake_bin.mkdir()
        fake = fake_bin / "claude"
        fake.write_text("#!/usr/bin/env python3\nimport time\nprint('Claude Code ready', flush=True)\nwhile True: time.sleep(1)\n")
        fake.chmod(0o700)
        fake_env = dict(env, PATH=str(fake_bin) + os.pathsep + env["PATH"])
        managed = json.loads(cli("-a", "claude", "up", "--name", "assistants", "--json", context=fake_env))
        sessions.append(managed)
        cli("-s", "assistants", "ea", "create", "--name", "Research", "--agent", "claude")
        cli("-s", "assistants", "ea", "start", "Research")
        owned = info("assistants")
        assert {a["ea_id"] for a in owned["agents"]} == {0, 1}, owned
        for ea_id in (0, 1):
            context = json.loads((Path(managed["directory"]) / f"mcp/ea-{ea_id}/context.json").read_text())
            assert context["tmux_server"] == managed["tmux_server"]
            assert context["serve"]["endpoint"] == managed["url"].removeprefix("http://")
        assert not (Path(managed["directory"]) / "active_ea").exists(), "client targeting changed global selection"
        # Attaching with assistants running shows them and leaves them running after detach.
        terminal, master = terminal_attach("assistants")
        try:
            terminal_expect(master, "OMAR assistants")
            # A fresh dashboard opens EA 0, whatever other clients targeted before.
            terminal_expect(master, "Executive Assistant (Default)")
            os.write(master, b"z")
            assert terminal_exit(terminal, master) == 0
        finally:
            if terminal.poll() is None: terminal.kill(); terminal.wait()
            os.close(master)
        assert {a["ea_id"] for a in info("assistants")["agents"]} == {0, 1}, "detaching stopped assistants"
        slow = root / "slow.omar"
        reaction_pid = root / "reaction.pid"
        slow.write_text('team Slow { timer tick(1ns, 0) output done : int reaction(tick) -> done {= '
                        + 'std::fs::write(' + json.dumps(str(reaction_pid)) + ', std::process::id().to_string()).unwrap(); '
                        + 'std::thread::sleep(std::time::Duration::from_secs(300)); done = Some(1); =} } '
                        + 'main SlowRun { slow = Slow() }')
        cli("-s", "assistants", "start", str(slow))
        deadline = time.monotonic() + 30
        while not reaction_pid.exists():
            assert time.monotonic() < deadline, info("assistants")
            time.sleep(.1)
        pid = int(reaction_pid.read_text())
        for worktree in (Path(managed["directory"]) / "workspaces").glob("*/worktree"):
            assert not (worktree / "state").exists(), "workspace copied another session's private state"
        cli("down", "-s", "assistants", "--force")
        status = subprocess.run(["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True)
        assert status.returncode != 0 or status.stdout.strip().startswith("Z"), "force left code reaction alive"
        # A stale registry record never causes a PID-only signal or wrong attachment.
        stale = up("stale")
        os.kill(stale["pid"], signal.SIGKILL)
        time.sleep(.2)
        cli("down", "-s", stale["name"], ok=False)
        assert info("outer")["session"]["state"] == "ready"
        print("PASS: session isolation, nested routing, help, startup races/failure, dashboard attach/detach, browser close, live topologies, targeted shutdown")
    finally:
        for session in sessions:
            subprocess.run([str(BIN), "down", "-s", session["name"], "--force", "--timeout", "5"],
                           env=env, capture_output=True, timeout=15)
            subprocess.run(["tmux", "-L", session["tmux_server"], "kill-server"], capture_output=True)
