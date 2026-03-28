#!/usr/bin/env python3
import json
import os
import subprocess
import sys

MAX_STDOUT = 64 * 1024
MAX_STDERR = 64 * 1024
FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2
CHILD_TMP_BYTES = 16 * 1024 * 1024
CHILD_MEMORY_BYTES = 512 * 1024 * 1024
CHILD_NOFILE = 64
CHILD_NPROC = 16
CHILD_FSIZE_BYTES = 2 * 1024 * 1024


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
    return {
        "PATH": path,
        "HOME": "/tmp",
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "ZEROBOOT_OFFLINE": "1",
    }


def parse_child_response(data: bytes):
    newline = data.find(b"\n")
    if newline < 0:
        raise ValueError("invalid child response")
    header = data[:newline].decode("utf-8", "replace").strip().split()
    if len(header) != 7 or header[0] != "WRK1R":
        raise ValueError("malformed child response")
    request_id_len = int(header[1])
    stdout_len = int(header[4])
    stderr_len = int(header[5])
    payload = data[newline + 1 + request_id_len :]
    stdout = payload[:stdout_len]
    stderr = payload[stdout_len : stdout_len + stderr_len]
    return int(header[2]), header[3], stdout, stderr, int(header[6])


def child_command(timeout_ms: int) -> list[str]:
    child_script = os.environ.get("ZEROBOOT_CHILD_SCRIPT", "/zeroboot/worker_child.py")
    python_bin = os.environ.get("ZEROBOOT_PYTHON_BIN", "python3")
    cpu_seconds = max(1, int((timeout_ms + 1999) / 1000))
    memory_kib = max(1, CHILD_MEMORY_BYTES // 1024)
    file_kib = max(1, CHILD_FSIZE_BYTES // 1024)
    shell = (
        f"ulimit -t {cpu_seconds}; "
        f"ulimit -v {memory_kib}; "
        f"ulimit -n {CHILD_NOFILE}; "
        f"ulimit -u {CHILD_NPROC}; "
        f"ulimit -f {file_kib}; "
        f"exec {python_bin} {child_script}"
    )
    return ["/bin/sh", "-c", shell]


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

    try:
        result = subprocess.run(
            child_command(timeout_ms),
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=max(timeout_ms / 1000.0 + 2.0, 5.0),
            env=minimal_child_env(),
        )
    except subprocess.TimeoutExpired:
        return -1, "timeout", b"", b"execution timed out\n", 0
    except Exception as exc:  # pragma: no cover - hard failure path
        return -1, "internal", b"", str(exc).encode("utf-8", "replace"), 0

    if result.stdout.startswith(b"WRK1R "):
        return parse_child_response(result.stdout)

    stdout, stdout_truncated = truncate_with_marker(
        result.stdout, MAX_STDOUT, b"\n[truncated]\n"
    )
    stderr, stderr_truncated = truncate_with_marker(
        result.stderr, MAX_STDERR, b"\n[truncated]\n"
    )
    flags = 0
    if stdout_truncated:
        flags |= FLAG_STDOUT_TRUNCATED
    if stderr_truncated:
        flags |= FLAG_STDERR_TRUNCATED
    return result.returncode or 0, ("ok" if result.returncode == 0 else "runtime"), stdout, stderr, flags


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
