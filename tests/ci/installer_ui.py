#!/usr/bin/env python3
"""Offline installer checks using real archives/install and a PTY for animation.

Mocks only downloads, platform detection and sudo. No network or privileges.
"""
import errno
import io
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import tarfile
import tempfile
import time

INSTALLER = Path(__file__).resolve().parents[2] / "install.sh"


def executable(path, body):
    path.write_text(body)
    path.chmod(0o755)


def run_case(name, *, terminal=False, overrides=None, compiler=True, failure=False,
             interrupt=False, sudo=False):
    with tempfile.TemporaryDirectory(prefix="omar-installer-test-") as root:
        root = Path(root)
        binaries = root / "bin"
        binaries.mkdir()
        temp = root / "tmp"
        temp.mkdir()
        destination = root / "installed binaries"
        if not sudo:
            destination.mkdir()
        archive = root / "release.tar.gz"
        members = ["omar", "omar-slack", "omar-computer"] + (["omarc"] if compiler else [])
        with tarfile.open(archive, "w:gz") as tar:
            for member in members:
                data = f"#!/bin/sh\nprintf '{member} fixture\\n'\n".encode()
                entry = tarfile.TarInfo("release/" + member)
                entry.size = len(data)
                entry.mode = 0o755
                tar.addfile(entry, io.BytesIO(data))
        executable(binaries / "curl", """#!/usr/bin/env python3
import json, os, pathlib, shutil, sys, time
args = sys.argv[1:]
url = next(arg for arg in args if arg.startswith('https://'))
with open(os.environ['FIXTURE_REQUESTS'], 'a') as f: f.write(url + '\\n')
pathlib.Path(os.environ['FIXTURE_PID']).write_text(str(os.getpid()))
time.sleep(float(os.environ.get('FIXTURE_DELAY', '0.15')))
if os.environ.get('FIXTURE_FAILURE'):
    print('download fixture: connection refused', file=sys.stderr)
    sys.exit(22)
out = pathlib.Path(args[args.index('-o') + 1])
if '/releases/latest' in url: out.write_text(json.dumps({'tag_name': 'v9.8.7'}))
else: shutil.copyfile(os.environ['FIXTURE_ARCHIVE'], out)
""")
        executable(binaries / "uname", """#!/bin/sh
case "$1" in -s) printf '%s\\n' "${FIXTURE_OS:-Linux}" ;; -m) printf '%s\\n' "${FIXTURE_ARCH:-x86_64}" ;; esac
""")
        executable(binaries / "sudo", """#!/bin/sh
printf '%s\\n' "$*" >> "$FIXTURE_SUDO_LOG"
if [ "$1" = -v ]; then
  printf 'fixture sudo authentication\\n'
  exit 0
fi
exec "$@"
""")
        env = dict(os.environ, PATH=f"{binaries}:{os.environ['PATH']}",
                   OMAR_INSTALL_DIR=str(destination), TMPDIR=str(temp), TERM="xterm-256color",
                   LC_ALL="C.UTF-8", COLUMNS="80", FIXTURE_ARCHIVE=str(archive),
                   FIXTURE_REQUESTS=str(root / "requests"), FIXTURE_PID=str(root / "curl.pid"),
                   FIXTURE_SUDO_LOG=str(root / "sudo.log"))
        env.pop("NO_COLOR", None)
        env.pop("OMAR_VERSION", None)
        if failure:
            env["FIXTURE_FAILURE"] = "1"
        if interrupt:
            env["FIXTURE_DELAY"] = "30"
        env.update(overrides or {})
        master, slave = pty.openpty() if terminal else (None, None)
        # Feed the script over stdin just like curl | sh, even with a PTY output.
        proc = subprocess.Popen(["sh"], stdin=subprocess.PIPE, stdout=slave or subprocess.PIPE,
                                stderr=slave or subprocess.PIPE, env=env, start_new_session=True)
        output = bytearray()
        try:
            if terminal:
                os.close(slave)
                proc.stdin.write(INSTALLER.read_bytes())
                proc.stdin.close()
                deadline = time.monotonic() + 15
                signalled = False
                while time.monotonic() < deadline:
                    if interrupt and not signalled and (root / "curl.pid").exists():
                        proc.send_signal(signal.SIGTERM)
                        signalled = True
                    if select.select([master], [], [], .05)[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError as error:
                            if error.errno == errno.EIO:
                                break
                            raise
                        if not data:
                            break
                        output.extend(data)
                    if proc.poll() is not None:
                        # Drain any buffered output until the PTY closes.
                        continue
                proc.wait(timeout=2)
            else:
                stdout, stderr = proc.communicate(INSTALLER.read_bytes(), timeout=15)
                output.extend(stdout + stderr)
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
            if master is not None:
                os.close(master)
        text = output.decode(errors="replace")
        if interrupt:
            assert proc.returncode == 143, (name, proc.returncode, text)
            child = int((root / "curl.pid").read_text())
            try:
                os.kill(child, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError(f"{name}: download child survived")
        elif failure:
            assert proc.returncode == 22, (name, proc.returncode, text)
            assert "connection refused" in text and "Installed successfully" not in text, text
            assert not list(destination.iterdir())
        else:
            assert proc.returncode == 0, (name, proc.returncode, text)
            assert "Installed successfully" in text and "omar serve --ui" in text, text
            assert sorted(p.name for p in destination.iterdir()) == sorted(members)
            for member in members:
                assert subprocess.check_output([destination / member], text=True).strip() == member + " fixture"
            requests = (root / "requests").read_text()
            if "OMAR_VERSION" in env:
                assert "/releases/latest" not in requests
                assert f"/v{env['OMAR_VERSION']}/" in requests
            else:
                assert "/releases/latest" in requests and "/v9.8.7/" in requests
            if sudo:
                assert (root / "sudo.log").read_text().startswith("-v\n")
        assert not list(temp.iterdir()), (name, "temporary files leaked")
        styled = terminal and env.get("TERM") != "dumb" and "NO_COLOR" not in env
        animated = styled and int(env["COLUMNS"]) >= 60
        if animated:
            assert "\x1b[?25l" in text and "\x1b[?25h" in text, text
            assert text.rfind("\x1b[?25h") > text.rfind("\x1b[?25l"), text
            assert "\x1b[38;2;140;99;242m" in text, text
        elif not styled:
            assert "\x1b" not in text, text
        else:
            assert "\x1b[?25l" not in text and "\x1b[5A" not in text, text
        if env.get("LC_ALL") == "C":
            assert text.isascii(), text
        print(f"PASS: {name}")
        return text


if __name__ == "__main__":
    run_case("piped latest release")
    run_case("pinned legacy archive", compiler=False, overrides={"OMAR_VERSION": "0.3.3"})
    preview = run_case("purple animated terminal", terminal=True)
    run_case("NO_COLOR", terminal=True, overrides={"NO_COLOR": ""})
    run_case("dumb terminal", terminal=True, overrides={"TERM": "dumb"})
    run_case("ASCII terminal", terminal=True, overrides={"LC_ALL": "C"})
    run_case("narrow terminal", terminal=True, overrides={"COLUMNS": "35"})
    run_case("download failure", terminal=True, failure=True)
    run_case("interrupted download", terminal=True, interrupt=True)
    run_case("foreground sudo", terminal=True, sudo=True)
    run_case("macOS ARM", overrides={"FIXTURE_OS": "Darwin", "FIXTURE_ARCH": "arm64"})
    if os.environ.get("OMAR_INSTALLER_PREVIEW"):
        Path(os.environ["OMAR_INSTALLER_PREVIEW"]).write_text(preview)
