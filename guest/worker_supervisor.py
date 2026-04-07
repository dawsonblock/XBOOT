#!/usr/bin/env python3
import json
import os
import subprocess
import sys

try:
    import resource as _resource
except ImportError:  # pragma: no cover
    _resource = None

MAX_STDOUT = 64 * 1024
MAX_STDERR = 64 * 1024
FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2
CHILD_TMP_BYTES = 16 * 1024 * 1024
CHILD_MEMORY_BYTES = 512 * 1024 * 1024
CHILD_NOFILE = 64
CHILD_NPROC = 16
CHILD_FSIZE_BYTES = 2 * 1024 * 1024
TRUNCATION_MARKER = b"\n[truncated]\n"


def read_exact(n: int) -> bytes:
    data = bytearray()
    while len(data) < n:
        chunk = sys.stdin.buffer.read(n - len(data))
        if not chunk:
            raise EOFError("worker stdin closed")
        data.extend(chunk)
    return bytes(data)


def read_line() -> str:
    line = sys.stdin.buffer.readline()
    if not line:
        raise EOFError("worker stdin closed")
    return line.decode("utf-8", "replace").strip()


def truncate_with_marker(data: bytes, limit: int, marker: bytes):
    if len(data) <= limit:
        return data, False
    if limit <= len(marker):
        return marker[:limit], True
    return data[: limit - len(marker)] + marker, True


def write_response(request_id: bytes, exit_code: int, error_type: str, stdout: bytes, stderr: bytes, flags: int) -> None:
    header = f"WRK1R {len(request_id)} {exit_code} {error_type} {len(stdout)} {len(stderr)} {flags}\n"
    sys.stdout.buffer.write(header.encode("utf-8"))
    sys.stdout.buffer.write(request_id)
    sys.stdout.buffer.write(stdout)
    sys.stdout.buffer.write(stderr)
    sys.stdout.buffer.flush()


def minimal_child_env() -> dict:
    path = os.environ.get("PATH") or "/usr/local/bin:/usr/bin:/bin"
    env = {
        "PATH": path,
        "HOME": "/tmp",
        "TMPDIR": "/tmp",
        "TMP": "/tmp",
        "TEMP": "/tmp",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "ZEROBOOT_OFFLINE": "1",
    }
    profile = os.environ.get("ZEROBOOT_CHILD_LIMIT_PROFILE")
    if profile:
        env["ZEROBOOT_CHILD_LIMIT_PROFILE"] = profile
    return env


def limit_profile() -> str:
    return os.environ.get("ZEROBOOT_CHILD_LIMIT_PROFILE", "guest").strip().lower() or "guest"


def parse_child_response(data: bytes):
    newline = data.find(b"\n")
    if newline < 0:
        raise ValueError("invalid child response")
    header = data[:newline].decode("utf-8", "replace").strip().split()
    if len(header) != 7 or header[0] != "WRK1R":
        raise ValueError("malformed child response")
    try:
        request_id_len = int(header[1])
    except ValueError:
        raise ValueError("malformed child response: non-integer request_id_len field")
    try:
        exit_code = int(header[2])
    except ValueError:
        raise ValueError("malformed child response: non-integer exit_code field")
    try:
        stdout_len = int(header[4])
    except ValueError:
        raise ValueError("malformed child response: non-integer stdout_len field")
    try:
        stderr_len = int(header[5])
    except ValueError:
        raise ValueError("malformed child response: non-integer stderr_len field")
    try:
        flags = int(header[6])
    except ValueError:
        raise ValueError("malformed child response: non-integer flags field")
    if request_id_len < 0 or stdout_len < 0 or stderr_len < 0:
        raise ValueError("malformed child response: negative length field")
    full_payload = data[newline + 1:]
    expected_len = request_id_len + stdout_len + stderr_len
    if len(full_payload) != expected_len:
        raise ValueError(
            f"malformed child response: payload length mismatch "
            f"(got {len(full_payload)}, expected {expected_len})"
        )
    stdout = full_payload[request_id_len : request_id_len + stdout_len]
    stderr = full_payload[request_id_len + stdout_len : request_id_len + stdout_len + stderr_len]
    return exit_code, header[3], stdout, stderr, flags


def _make_child_preexec_fn(timeout_ms: int):
    """Return a preexec_fn that applies process-level OS resource limits.

    Called after fork() but before exec() in the child process so that
    resource limits are owned exclusively by the supervisor, not the child.
    """
    if _resource is None:  # pragma: no cover
        return None

    profile = limit_profile()
    cpu_seconds = max(1, int((timeout_ms + 1999) / 1000))
    memory_bytes = CHILD_MEMORY_BYTES
    nofile = CHILD_NOFILE
    nproc = CHILD_NPROC
    fsize_bytes = CHILD_FSIZE_BYTES

    def _preexec():
        def _clamp(kind, desired):
            soft, hard = _resource.getrlimit(kind)
            target = desired if hard == _resource.RLIM_INFINITY else min(desired, hard)
            if soft == _resource.RLIM_INFINITY or soft >= target:
                target_soft = target
            else:
                target_soft = min(target, soft)
            try:
                _resource.setrlimit(kind, (target_soft, target))
            except (ValueError, OSError):
                pass

        _clamp(_resource.RLIMIT_CPU, cpu_seconds)
        if profile != "compat" and hasattr(_resource, "RLIMIT_AS"):
            _clamp(_resource.RLIMIT_AS, memory_bytes)
        _clamp(_resource.RLIMIT_NOFILE, nofile)
        if profile != "compat" and hasattr(_resource, "RLIMIT_NPROC"):
            _clamp(_resource.RLIMIT_NPROC, nproc)
        _clamp(_resource.RLIMIT_FSIZE, fsize_bytes)

    return _preexec


def spawn_child_executor(request_id: str, timeout_ms: int, code: str, stdin_data: str):
    payload = json.dumps(
        {
            "request_id": request_id,
            "timeout_ms": timeout_ms,
            "code": code,
            "stdin": stdin_data,
            "limits": {
                "stdout_bytes": MAX_STDOUT,
                "stderr_bytes": MAX_STDERR,
                "tmp_bytes": CHILD_TMP_BYTES,
                "memory_bytes": CHILD_MEMORY_BYTES,
                "nofile": CHILD_NOFILE,
                "nproc": CHILD_NPROC,
                "fsize_bytes": CHILD_FSIZE_BYTES,
            },
        }
    ).encode("utf-8")

    child_script = os.environ.get("ZEROBOOT_CHILD_SCRIPT", "/zeroboot/worker_child.py")
    python_bin = os.environ.get("ZEROBOOT_PYTHON_BIN", "python3")
    cmd = [python_bin, child_script]

    try:
        result = subprocess.run(
            cmd,
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=max(timeout_ms / 1000.0 + 2.0, 5.0),
            env=minimal_child_env(),
            preexec_fn=_make_child_preexec_fn(timeout_ms),
        )
    except subprocess.TimeoutExpired:
        return -1, "timeout", b"", b"execution timed out\n", 0
    except Exception as exc:  # pragma: no cover - hard failure path
        return -1, "internal", b"", str(exc).encode("utf-8", "replace"), 0

    if result.stdout.startswith(b"WRK1R "):
        try:
            return parse_child_response(result.stdout)
        except Exception as exc:
            detail = f"malformed child response: {exc}\n".encode("utf-8", "replace")
            stderr, stderr_truncated = truncate_with_marker(
                detail + (result.stderr or b""), MAX_STDERR, TRUNCATION_MARKER
            )
            return -1, "protocol", b"", stderr, FLAG_STDERR_TRUNCATED if stderr_truncated else 0

    stdout, stdout_truncated = truncate_with_marker(
        result.stdout, MAX_STDOUT, TRUNCATION_MARKER
    )
    if result.returncode is not None and result.returncode < 0:
        detail = f"child exited by signal {-result.returncode}\n".encode("utf-8", "replace")
        stderr, stderr_truncated = truncate_with_marker(
            detail + (result.stderr or b""), MAX_STDERR, TRUNCATION_MARKER
        )
        flags = 0
        if stdout_truncated:
            flags |= FLAG_STDOUT_TRUNCATED
        if stderr_truncated:
            flags |= FLAG_STDERR_TRUNCATED
        return -1, "internal", stdout, stderr, flags
    stderr, stderr_truncated = truncate_with_marker(result.stderr, MAX_STDERR, TRUNCATION_MARKER)
    flags = 0
    if stdout_truncated:
        flags |= FLAG_STDOUT_TRUNCATED
    if stderr_truncated:
        flags |= FLAG_STDERR_TRUNCATED
    return (
        result.returncode or 0,
        ("ok" if result.returncode == 0 else "internal"),
        stdout,
        stderr,
        flags,
    )


print("READY", flush=True)

while True:
    try:
        header = read_line().split()
        if len(header) != 5 or header[0] != "WRK1":
            write_response(b"error", -1, "protocol", b"", b"invalid worker request header", 0)
            continue
        request_id = read_exact(int(header[1]))
        timeout_ms = int(header[2])
        code = read_exact(int(header[3])).decode("utf-8", "replace")
        stdin_data = read_exact(int(header[4])).decode("utf-8", "replace")
        exit_code, error_type, stdout, stderr, flags = spawn_child_executor(
            request_id.decode("utf-8", "replace"),
            timeout_ms,
            code,
            stdin_data,
        )
        write_response(request_id, exit_code, error_type, stdout, stderr, flags)
    except EOFError:
        break
    except Exception as exc:  # pragma: no cover - supervisor crash path
        write_response(
            b"error",
            -1,
            "internal",
            b"",
            str(exc).encode("utf-8", "replace")[:MAX_STDERR],
            FLAG_STDERR_TRUNCATED if len(str(exc).encode("utf-8", "replace")) > MAX_STDERR else 0,
        )
        break
