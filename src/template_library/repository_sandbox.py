"""Run owned repository candidates in a pinned local Docker image, never the host.

Protected expectations remain in the caller and are not mounted or sent here.
This is a container boundary, not proof against a Docker/kernel vulnerability.
"""
import json
import functools
import os
import re
import selectors
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

DRIVER = '''import copy, importlib, json, sys
sys.path.insert(0, '/workspace')
handle = getattr(importlib.import_module(MODULE_NAME), FUNCTION_NAME)
for line in sys.stdin:
    request = json.loads(line)
    original = copy.deepcopy(request)
    try:
        value = handle(request)
        response = {'value': value, 'input_unchanged': request == original}
    except Exception as error:
        response = {'error_type': type(error).__name__}
    print(json.dumps(response, allow_nan=False), flush=True)
'''


class InfrastructureError(RuntimeError):
    pass


@functools.lru_cache(maxsize=1)
def docker_connection():
    environment = dict(os.environ)
    context = environment.get("DOCKER_CONTEXT")
    endpoint = environment.get("DOCKER_HOST") if not context else None
    if not endpoint:
        # Context inspection reads local client metadata; it does not contact
        # the engine. A remote context must fail before any engine request.
        arguments = ["docker", "context", "inspect", "--format", "{{json .Endpoints.docker.Host}}"]
        if context:
            arguments.append(context)
        try:
            metadata = subprocess.run(arguments, capture_output=True, text=True, timeout=10, check=True)
            endpoint = json.loads(metadata.stdout)
        except (OSError, subprocess.SubprocessError, ValueError) as error:
            raise InfrastructureError("Cannot inspect the local Docker context") from error
    if not isinstance(endpoint, str) or not endpoint.startswith("unix:///") or "\n" in endpoint:
        raise InfrastructureError("Repository evaluation requires a local Unix-socket Docker daemon")
    environment.pop("DOCKER_CONTEXT", None)
    environment.pop("DOCKER_HOST", None)
    return ["docker", "--host", endpoint], environment


def command(args, timeout=15):
    prefix, environment = docker_connection()
    try:
        result = subprocess.run([*prefix, *args], env=environment, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise InfrastructureError(str(error)) from error
    if result.returncode:
        raise InfrastructureError(result.stderr[-2000:] or f"Docker exited {result.returncode}")
    return result.stdout


def resolve_image(reference):
    """Read local metadata only. Missing images are never pulled implicitly."""
    value = command(["image", "inspect", "--format", "{{.Id}}", reference]).strip()
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        raise InfrastructureError("The local image did not resolve to one immutable image ID")
    return value


def run(repository, requests, image, *, timeout=15, max_output_bytes=65536, cancel_path=None, entrypoint="candidate_app:handle"):
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise InfrastructureError("Repository execution requires a pinned local image ID")
    repository = Path(repository).resolve(strict=True)
    if "," in str(repository) or "\n" in str(repository):
        raise InfrastructureError("Unsupported mount path")
    if not 0 < timeout <= 60 or not 0 < max_output_bytes <= 1048576:
        raise ValueError("Invalid container time/output bounds")
    if not re.fullmatch(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*:[A-Za-z_]\w*", entrypoint, re.ASCII):
        raise InfrastructureError("Invalid Python JSON API entrypoint")
    payload = ("\n".join(json.dumps(r, allow_nan=False) for r in requests) + "\n").encode()
    if len(payload) > 262144:
        raise ValueError("Protected request payload exceeds 256 KiB")
    name = "omar-repository-eval-" + uuid.uuid4().hex
    created = False
    process = None
    started = time.monotonic()
    def stopped_before_start():
        if cancel_path is not None and Path(cancel_path).exists():
            raise InfrastructureError("Repository verification cancelled")
    stopped_before_start()
    with tempfile.TemporaryDirectory(prefix="omar-repository-driver-") as temporary:
        root = Path(temporary).resolve()
        root.chmod(0o755)
        driver = root / "driver.py"
        module, function = entrypoint.split(":")
        driver.write_text(DRIVER.replace("MODULE_NAME", repr(module)).replace("FUNCTION_NAME", repr(function)))
        driver.chmod(0o444)
        input_file = root / "requests.jsonl"
        input_file.write_bytes(payload)
        try:
            # Cleanup attempts removal even if create times out after the engine
            # created the container but before the client observed success.
            created = True
            command(["create", "--name", name, "--label", "omar.evaluation=repository-repair",
                     "--pull=never", "--interactive", "--network=none", "--read-only",
                     "--user=65534:65534", "--cap-drop=ALL", "--security-opt=no-new-privileges:true",
                     "--pids-limit=32", "--memory=256m", "--memory-swap=256m", "--cpus=1",
                     "--log-driver=none", "--no-healthcheck", "--workdir=/workspace",
                     "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=16m",
                     "--mount", f"type=bind,source={repository},target=/workspace,readonly",
                     "--mount", f"type=bind,source={driver},target=/runner.py,readonly",
                     "--entrypoint=/usr/local/bin/python3", image, "-I", "-B", "/runner.py"])
            stopped_before_start()
            configuration = json.loads(command(["inspect", name]))[0]
            host = configuration["HostConfig"]
            mounts = configuration["Mounts"]
            expected = {"/workspace": str(repository), "/runner.py": str(driver)}
            valid = (configuration["Image"] == image and configuration["Config"]["User"] == "65534:65534"
                     and host["NetworkMode"] == "none" and host["ReadonlyRootfs"] and not host["Privileged"]
                     and host["CapDrop"] == ["ALL"] and host["Memory"] == 268435456
                     and host["MemorySwap"] == 268435456 and host["NanoCpus"] == 1000000000
                     and host["PidsLimit"] == 32 and "no-new-privileges:true" in host["SecurityOpt"]
                     and len(mounts) == 2 and all(not m["RW"] and m["Type"] == "bind"
                         and expected.get(m["Destination"]) == m["Source"] for m in mounts))
            if not valid:
                raise InfrastructureError("Container isolation configuration did not match the required contract")
            stopped_before_start()
            stdout = bytearray()
            stderr = bytearray()
            stopped = None
            with input_file.open("rb") as source, selectors.DefaultSelector() as selector:
                prefix, environment = docker_connection()
                process = subprocess.Popen([*prefix, "start", "--attach", "--interactive", name], env=environment,
                                           stdin=source, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                for stream, target in [(process.stdout, stdout), (process.stderr, stderr)]:
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ, target)
                deadline = time.monotonic() + timeout
                while selector.get_map():
                    if cancel_path is not None and Path(cancel_path).exists():
                        stopped = "cancelled"
                        break
                    if time.monotonic() >= deadline:
                        stopped = "timeout"
                        break
                    for key, _ in selector.select(min(0.1, max(0, deadline-time.monotonic()))):
                        chunk = os.read(key.fd, 4096)
                        if not chunk:
                            selector.unregister(key.fileobj)
                        else:
                            allowance = max_output_bytes - len(stdout) - len(stderr)
                            key.data.extend(chunk[:allowance])
                            if len(chunk) > allowance:
                                stopped = "output_limit"
                                break
                    if stopped:
                        break
                if stopped:
                    try:
                        command(["kill", name])
                    except InfrastructureError:
                        if json.loads(command(["inspect", "--format", "{{json .State}}", name]))["Running"]:
                            raise
                    # After a flood, the attach client's pipe can remain full
                    # even after the container stops. Stop that local client
                    # instead of waiting for it to flush unbounded output.
                    process.kill()
                process.wait(timeout=5)
            state = json.loads(command(["inspect", "--format", "{{json .State}}", name]))
            if state.get("Error"):
                raise InfrastructureError(state["Error"])
            return {"container_name":name,"image":image,"exit_code":state["ExitCode"],"oom_killed":state["OOMKilled"],
                    "stop_reason":stopped,"stdout":stdout.decode("utf-8",errors="replace"),
                    "stderr":stderr.decode("utf-8",errors="replace"),"elapsed_seconds":round(time.monotonic()-started,3),
                    "isolation":{"network":"none","root_readonly":True,"candidate_readonly":True,"uid":65534,
                        "capabilities":[],"new_privileges":False,"memory_bytes":268435456,"pids":32,
                        "cpus":1,"timeout_seconds":timeout,"output_bytes":max_output_bytes,
                        "protected_expectations_mounted":False,"host_credentials_mounted":False}}
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError, KeyError) as error:
            raise InfrastructureError("Repository execution infrastructure failed: " + str(error)) from error
        finally:
            if created:
                command(["rm", "--force", name])
            if process is not None:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
                process.stdout.close()
                process.stderr.close()
