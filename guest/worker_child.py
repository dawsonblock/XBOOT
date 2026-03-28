#!/usr/bin/env python3
import builtins
import contextlib
import gc
import io
import json
import os
import shutil
import signal
import sys
import tempfile
import traceback

try:
    import resource
except ImportError:  # pragma: no cover
    resource = None

FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2
TRUNCATION_MARKER = b"\n[truncated]\n"


def truncate_with_marker(data: bytes, limit: int):
    if len(data) <= limit:
        return data, False
    if limit <= len(TRUNCATION_MARKER):
        return TRUNCATION_MARKER[:limit], True
    return data[: limit - len(TRUNCATION_MARKER)] + TRUNCATION_MARKER, True


def write_response(request_id: bytes, exit_code: int, error_type: str, stdout: bytes, stderr: bytes, flags: int) -> None:
    header = f"WRK1R {len(request_id)} {exit_code} {error_type} {len(stdout)} {len(stderr)} {flags}\n"
    sys.stdout.buffer.write(header.encode("utf-8"))
    sys.stdout.buffer.write(request_id)
    sys.stdout.buffer.write(stdout)
    sys.stdout.buffer.write(stderr)
    sys.stdout.buffer.flush()


def timeout_handler(_signum, _frame):
    raise TimeoutError("execution timed out")


def apply_limits(timeout_ms: int, limits: dict) -> None:
    if resource is None:
        return

    def clamp_and_set(kind, desired):
        soft, hard = resource.getrlimit(kind)
        if hard == resource.RLIM_INFINITY:
            target = desired
        else:
            target = min(desired, hard)
        if soft == resource.RLIM_INFINITY:
            target_soft = target
        else:
            target_soft = min(target, soft) if soft < target else target
        try:
            resource.setrlimit(kind, (target_soft, target))
        except (ValueError, OSError):
            # Some developer hosts do not allow tightening every limit locally.
            # The Linux guest path still applies shell-level ulimit caps first.
            pass

    cpu_seconds = max(1, int((timeout_ms + 1999) / 1000))
    memory_bytes = int(limits.get("memory_bytes", 512 * 1024 * 1024))
    nofile = int(limits.get("nofile", 64))
    nproc = int(limits.get("nproc", 16))
    fsize_bytes = int(limits.get("fsize_bytes", 2 * 1024 * 1024))
    clamp_and_set(resource.RLIMIT_CPU, cpu_seconds)
    if hasattr(resource, "RLIMIT_AS"):
        clamp_and_set(resource.RLIMIT_AS, memory_bytes)
    clamp_and_set(resource.RLIMIT_NOFILE, nofile)
    if hasattr(resource, "RLIMIT_NPROC"):
        clamp_and_set(resource.RLIMIT_NPROC, nproc)
    clamp_and_set(resource.RLIMIT_FSIZE, fsize_bytes)


def directory_size(path: str) -> int:
    total = 0
    for root, _dirs, files in os.walk(path):
        for filename in files:
            try:
                total += os.path.getsize(os.path.join(root, filename))
            except OSError:
                continue
    return total


def main():
    payload = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    request_id = payload["request_id"].encode("utf-8", "replace")
    timeout_ms = max(1, int(payload.get("timeout_ms", 30000)))
    code = str(payload.get("code", ""))
    stdin_data = str(payload.get("stdin", ""))
    limits = payload.get("limits", {})
    max_stdout = int(limits.get("stdout_bytes", 64 * 1024))
    max_stderr = int(limits.get("stderr_bytes", 64 * 1024))
    max_tmp_bytes = int(limits.get("tmp_bytes", 16 * 1024 * 1024))

    signal.signal(signal.SIGALRM, timeout_handler)
    signal.setitimer(signal.ITIMER_REAL, timeout_ms / 1000.0)
    apply_limits(timeout_ms, limits)

    scratch = tempfile.mkdtemp(prefix="zeroboot-")
    old_environ = dict(os.environ)
    old_stdin = sys.stdin
    stdout_io = io.StringIO()
    stderr_io = io.StringIO()
    exit_code = 0
    error_type = "ok"
    try:
        os.environ.clear()
        os.environ.update(
            {
                "HOME": scratch,
                "TMPDIR": scratch,
                "ZEROBOOT_TMPDIR": scratch,
                "ZEROBOOT_OFFLINE": "1",
            }
        )
        sys.stdin = io.StringIO(stdin_data)
        globals_dict = {"__name__": "__main__", "__builtins__": builtins}
        with contextlib.redirect_stdout(stdout_io), contextlib.redirect_stderr(stderr_io):
            exec(compile(code, "<zeroboot>", "exec"), globals_dict, globals_dict)
        if directory_size(scratch) > max_tmp_bytes:
            exit_code = 1
            error_type = "runtime"
            stderr_io.write("temporary directory quota exceeded\n")
    except TimeoutError:
        exit_code = -1
        error_type = "timeout"
        stderr_io.write("execution timed out\n")
    except BaseException:
        exit_code = 1
        error_type = "runtime"
        traceback.print_exc(file=stderr_io)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        sys.stdin = old_stdin
        os.environ.clear()
        os.environ.update(old_environ)
        gc.collect()
        shutil.rmtree(scratch, ignore_errors=True)

    stdout_bytes, stdout_truncated = truncate_with_marker(
        stdout_io.getvalue().encode("utf-8", "replace"), max_stdout
    )
    stderr_bytes, stderr_truncated = truncate_with_marker(
        stderr_io.getvalue().encode("utf-8", "replace"), max_stderr
    )
    flags = 0
    if stdout_truncated:
        flags |= FLAG_STDOUT_TRUNCATED
    if stderr_truncated:
        flags |= FLAG_STDERR_TRUNCATED

    write_response(request_id, exit_code, error_type, stdout_bytes, stderr_bytes, flags)


if __name__ == "__main__":
    main()
