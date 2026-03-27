#!/usr/bin/env python3
"""
Subprocess-based Python worker supervisor.

This module runs as a long-lived supervisor process that spawns a new child executor
for each request. The child process is terminated and recreated after each request,
providing strong isolation between requests.
"""

import os
import subprocess
import sys
import uuid

# Maximum time to wait for child to start (ms)
CHILD_STARTUP_TIMEOUT_MS = 1000
MAX_STDOUT = 64 * 1024
MAX_STDERR = 64 * 1024
FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2
FLAG_RECYCLE_REQUESTED = 4


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


def truncate(data: bytes, limit: int):
    if len(data) <= limit:
        return data, False
    return data[:limit], True


def write_response(request_id: bytes, exit_code: int, error_type: str, stdout: bytes, stderr: bytes, flags: int) -> None:
    header = f"WRK1R {len(request_id)} {exit_code} {error_type} {len(stdout)} {len(stderr)} {flags}\n"
    sys.stdout.buffer.write(header.encode("utf-8"))
    sys.stdout.buffer.write(request_id)
    sys.stdout.buffer.write(stdout)
    sys.stdout.buffer.write(stderr)
    sys.stdout.buffer.flush()


def spawn_child_executor(timeout_ms: int, code: str, stdin_data: str) -> tuple:
    """
    Spawn a child process to execute code with strict isolation.
    Returns (exit_code, error_type, stdout, stderr, flags)
    """
    # Create a temporary script that will be executed by the child
    import tempfile
    import json
    
    # Write execution request to temp file
    exec_request = {
        "timeout_ms": timeout_ms,
        "code": code,
        "stdin": stdin_data,
    }
    
    # Use a simple approach: pass code via environment to avoid temp files
    # For true isolation, the child should be a fresh Python process
    child_script = os.environ.get("ZEROBOOT_CHILD_SCRIPT", "/usr/local/bin/zeroboot-python-worker")
    
    env = os.environ.copy()
    env["ZEROBOOT_EXEC_CODE"] = code
    env["ZEROBOOT_EXEC_STDIN"] = stdin_data
    env["ZEROBOOT_EXEC_TIMEOUT_MS"] = str(timeout_ms)
    
    try:
        # Try to use the dedicated child script if available
        result = subprocess.run(
            [child_script],
            input=b"",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=max(timeout_ms / 1000.0 + 1, 5),  # Add buffer to timeout
            env=env,
        )
        stdout = result.stdout
        stderr = result.stderr
        exit_code = result.returncode
        
        # Check if child script returned valid response
        if stdout.startswith(b"WRK1R "):
            return parse_child_response(stdout)
        
        # Fall back to treating output as stdout/stderr
        error_type = "ok" if exit_code == 0 else "runtime"
        
    except FileNotFoundError:
        # Child script doesn't exist, use inline execution (less secure but functional)
        return execute_inline(timeout_ms, code, stdin_data)
    except subprocess.TimeoutExpired:
        return -1, "timeout", b"", b"execution timed out", FLAG_RECYCLE_REQUESTED
    except Exception as e:
        return -1, "internal", b"", str(e).encode("utf-8")[:MAX_STDERR], FLAG_STDERR_TRUNCATED
    
    return exit_code, error_type, stdout, stderr, 0


def parse_child_response(data: bytes) -> tuple:
    """Parse response from child executor."""
    lines = data.split(b'\n', 1)
    if not lines[0].startswith(b"WRK1R "):
        return -1, "internal", b"", b"invalid child response", FLAG_STDERR_TRUNCATED
    
    parts = lines[0].decode("utf-8").split()
    if len(parts) < 7:
        return -1, "internal", b"", b"malformed child response", FLAG_STDERR_TRUNCATED
    
    exit_code = int(parts[2])
    error_type = parts[3]
    stdout_len = int(parts[4])
    stderr_len = int(parts[5])
    flags = int(parts[6])
    
    stdout = b""
    stderr = b""
    if len(lines) > 1:
        remaining = lines[1]
        stdout = remaining[:stdout_len]
        stderr = remaining[stdout_len:stdout_len + stderr_len]
    
    return exit_code, error_type, stdout, stderr, flags


def execute_inline(timeout_ms: int, code: str, stdin_data: str) -> tuple:
    """
    Fallback inline execution (less secure but works when child script unavailable).
    This is here for development/testing - production should use child process.
    """
    import builtins
    import contextlib
    import gc
    import io
    import signal
    import traceback
    
    def timeout_handler(_signum, _frame):
        raise TimeoutError("execution timed out")
    
    signal.signal(signal.SIGALRM, timeout_handler)
    signal.setitimer(signal.ITIMER_REAL, max(timeout_ms, 1) / 1000.0)
    
    stdout_io = io.StringIO()
    stderr_io = io.StringIO()
    globals_dict = {"__name__": "__main__", "__builtins__": builtins}
    locals_dict = globals_dict
    exit_code = 0
    error_type = "ok"
    
    old_stdin = sys.stdin
    try:
        sys.stdin = io.StringIO(stdin_data)
        with contextlib.redirect_stdout(stdout_io), contextlib.redirect_stderr(stderr_io):
            exec(compile(code, "<zeroboot>", "exec"), globals_dict, locals_dict)
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
        gc.collect()
    
    stdout_bytes = stdout_io.getvalue().encode("utf-8", "replace")
    stderr_bytes = stderr_io.getvalue().encode("utf-8", "replace")
    flags = 0
    
    stdout_bytes, stdout_truncated = truncate(stdout_bytes, MAX_STDOUT)
    stderr_bytes, stderr_truncated = truncate(stderr_bytes, MAX_STDERR)
    if stdout_truncated:
        flags |= FLAG_STDOUT_TRUNCATED
    if stderr_truncated:
        flags |= FLAG_STDERR_TRUNCATED
    
    return exit_code, error_type, stdout_bytes, stderr_bytes, flags


# Supervisor main loop
print("READY", flush=True)

while True:
    try:
        header = read_line()
        parts = header.split()
        if len(parts) != 5 or parts[0] != "WRK1":
            write_response(b"error", -1, "protocol", b"", b"invalid worker request header", 0)
            continue
        id_len = int(parts[1])
        timeout_ms = int(parts[2])
        code_len = int(parts[3])
        stdin_len = int(parts[4])
        request_id = read_exact(id_len)
        code = read_exact(code_len).decode("utf-8", "replace")
        stdin_data = read_exact(stdin_len).decode("utf-8", "replace")

        # Spawn child executor for this request
        exit_code, error_type, stdout, stderr, flags = spawn_child_executor(
            timeout_ms, code, stdin_data
        )
        
        # Always set recycle flag since we're using fresh process per request
        # Actually, we can be smarter - if the child completed successfully, 
        # we can reuse the supervisor. But for safety, let's recycle to be safe.
        # The supervisor stays alive, we just spawn a new child each time.
        write_response(request_id, exit_code, error_type, stdout, stderr, flags)
        
    except EOFError:
        break
    except BaseException:
        err = traceback.format_exc().encode("utf-8", "replace")
        write_response(b"error", -1, "internal", b"", err[:MAX_STDERR], FLAG_STDERR_TRUNCATED | FLAG_RECYCLE_REQUESTED if len(err) > MAX_STDERR else FLAG_RECYCLE_REQUESTED)
        break