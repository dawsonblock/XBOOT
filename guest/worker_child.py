#!/usr/bin/env python3
"""
Subprocess-based Python worker executor (child).

This script is spawned as a child process for each request, providing
strong isolation between requests. It runs once and exits.
"""

import builtins
import contextlib
import gc
import io
import os
import signal
import sys
import traceback

MAX_STDOUT = 64 * 1024
MAX_STDERR = 64 * 1024
FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2


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


def timeout_handler(_signum, _frame):
    raise TimeoutError("execution timed out")


def main():
    # Get execution parameters from environment
    code = os.environ.get("ZEROBOOT_EXEC_CODE", "")
    stdin_data = os.environ.get("ZEROBOOT_EXEC_STDIN", "")
    timeout_ms = int(os.environ.get("ZEROBOOT_EXEC_TIMEOUT_MS", "30000"))
    
    # Generate a request ID for this execution
    request_id = os.environ.get("ZEROBOOT_REQUEST_ID", "child").encode("utf-8")
    
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
    except BaseException as e:
        exit_code = 1
        error_type = "runtime"
        traceback.print_exc(file=stderr_io)
    except Exception as e:
        exit_code = 1
        error_type = "runtime"
        stderr_io.write(f"unexpected error: {e}\n")
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        sys.stdin = old_stdin
        # Aggressive cleanup - this process will exit anyway
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
    
    write_response(request_id, exit_code, error_type, stdout_bytes, stderr_bytes, flags)


if __name__ == "__main__":
    main()