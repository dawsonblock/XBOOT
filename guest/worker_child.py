#!/usr/bin/env python3
"""Worker child process - thin orchestration wrapper.

This is a minimal wrapper that:
1. Parses input from supervisor
2. Applies resource limits
3. Executes user code with timeout handling
4. Returns framed response
5. Exits cleanly (cleanup before response, no cleanup after)
"""

import gc
import json
import os
import shutil
import signal
import sys
import tempfile
from typing import Tuple, Optional

from executor import execute_code
from limits import apply_limits, limit_profile
from protocol import encode_response, decode_payload


FLAG_STDOUT_TRUNCATED = 1
FLAG_STDERR_TRUNCATED = 2
TRUNCATION_MARKER = b"\n[truncated]\n"

# Global flag for timeout detection
_timed_out = False


def timeout_handler(_signum, _frame):
    """Handle SIGALRM - set flag and raise exception to interrupt execution."""
    global _timed_out
    _timed_out = True
    raise TimeoutError("execution timed out")


def directory_size(path: str) -> int:
    """Calculate total size of directory in bytes."""
    total = 0
    for root, _dirs, files in os.walk(path):
        for filename in files:
            try:
                total += os.path.getsize(os.path.join(root, filename))
            except OSError:
                continue
    return total


def truncate_with_marker(data: bytes, limit: int) -> Tuple[bytes, bool]:
    """Truncate data with marker if it exceeds limit."""
    if len(data) <= limit:
        return data, False
    if limit <= len(TRUNCATION_MARKER):
        return TRUNCATION_MARKER[:limit], True
    return data[: limit - len(TRUNCATION_MARKER)] + TRUNCATION_MARKER, True


def main() -> int:
    """Main entry point.
    
    Returns:
        Exit code (0 for success, 1 for error)
    """
    global _timed_out
    
    # Default response values for early errors
    request_id = b"error"
    exit_code = -1
    error_type = "internal"
    stdout_str = ""
    stderr_str = ""
    stdout_bytes = b""
    stderr_bytes = b""
    flags = 0
    scratch: Optional[str] = None
    limit_failures: list[str] = []
    
    try:
        # Read and parse payload
        payload = decode_payload(sys.stdin.buffer.read())
        request_id = payload["request_id"].encode("utf-8", "replace")
        
        # Get limits from payload
        limits = payload["limits"]
        max_stdout = int(limits.get("stdout_bytes", 64 * 1024))
        max_stderr = int(limits.get("stderr_bytes", 64 * 1024))
        max_tmp_bytes = int(limits.get("tmp_bytes", 16 * 1024 * 1024))
        
        # Create scratch directory
        scratch = tempfile.mkdtemp(prefix="zeroboot-")
        
        # Set up minimal environment
        old_environ = dict(os.environ)
        os.environ.clear()
        os.environ.update({
            "HOME": scratch,
            "TMPDIR": scratch,
            "TMP": scratch,
            "TEMP": scratch,
            "ZEROBOOT_TMPDIR": scratch,
            "ZEROBOOT_OFFLINE": "1",
            "ZEROBOOT_CHILD_LIMIT_PROFILE": limit_profile(),
        })
        
        # Apply resource limits BEFORE setting up timeout
        timeout_ms = payload["timeout_ms"]
        cpu_seconds = max(1, int((timeout_ms + 1999) / 1000))
        limit_failures = apply_limits(
            cpu_seconds=cpu_seconds,
            memory_bytes=int(limits.get("memory_bytes", 512 * 1024 * 1024)),
            nofile=int(limits.get("nofile", 64)),
            nproc=int(limits.get("nproc", 16)),
            fsize_bytes=int(limits.get("fsize_bytes", 2 * 1024 * 1024)),
        )
        
        # Set up timeout handler
        _timed_out = False
        signal.signal(signal.SIGALRM, timeout_handler)
        signal.setitimer(signal.ITIMER_REAL, timeout_ms / 1000.0)
        
        try:
            # Execute user code
            exec_exit, stdout_str, stderr_str, exception = execute_code(
                code=payload["code"],
                stdin_data=payload["stdin"],
            )
            
            # Check for quota violations
            if directory_size(scratch) > max_tmp_bytes:
                exit_code = 1
                error_type = "runtime"
                stderr_str += "\ntemporary directory quota exceeded\n"
            elif exception is not None:
                exit_code = 1
                error_type = "runtime"
            else:
                exit_code = 0
                error_type = "ok"
                
        except TimeoutError:
            # Timeout occurred during execution
            exit_code = -1
            error_type = "timeout"
            stdout_str = ""
            stderr_str = "execution timed out\n"
            
        finally:
            # Cancel timer
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, signal.SIG_DFL)
        
        # Add limit warnings only if there were actual failures and execution wasn't clean
        if limit_failures and error_type != "ok":
            stderr_str += "\nlimit setup degraded: " + ", ".join(limit_failures) + "\n"
        
        # Truncate output
        stdout_bytes, stdout_trunc = truncate_with_marker(
            stdout_str.encode("utf-8", "replace"), max_stdout
        )
        stderr_bytes, stderr_trunc = truncate_with_marker(
            stderr_str.encode("utf-8", "replace"), max_stderr
        )
        
        flags = 0
        if stdout_trunc:
            flags |= FLAG_STDOUT_TRUNCATED
        if stderr_trunc:
            flags |= FLAG_STDERR_TRUNCATED
        
        # Cleanup BEFORE writing response (avoid cleanup-induced -9)
        os.environ.clear()
        os.environ.update(old_environ)
        gc.collect()
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)
        
        # Write response
        response = encode_response(
            request_id=request_id,
            exit_code=exit_code,
            error_type=error_type,
            stdout=stdout_bytes,
            stderr=stderr_bytes,
            flags=flags,
        )
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()
        
        # Clean exit - no signals, no cleanup after this point
        return 0
        
    except json.JSONDecodeError as e:
        # Protocol error - invalid JSON (classified as internal per test expectations)
        error_msg = f"JSONDecodeError: invalid JSON payload: {e}".encode("utf-8", "replace")
        response = encode_response(
            request_id=request_id,
            exit_code=-1,
            error_type="internal",
            stdout=b"",
            stderr=error_msg,
            flags=FLAG_STDERR_TRUNCATED if len(error_msg) > 64 * 1024 else 0,
        )
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()
        return 0
        
    except Exception as e:
        # Internal error
        error_msg = str(e).encode("utf-8", "replace")
        response = encode_response(
            request_id=request_id,
            exit_code=-1,
            error_type="internal",
            stdout=b"",
            stderr=error_msg,
            flags=FLAG_STDERR_TRUNCATED if len(error_msg) > 64 * 1024 else 0,
        )
        sys.stdout.buffer.write(response)
        sys.stdout.buffer.flush()
        return 1


if __name__ == "__main__":
    sys.exit(main())
