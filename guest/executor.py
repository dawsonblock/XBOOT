"""Pure code execution module for guest workers.

Handles execution of user code in a clean namespace.
No I/O, no limits, no protocol - just execution.
"""

import builtins
import io
import sys
import traceback
from contextlib import redirect_stdout, redirect_stderr
from typing import Any, Dict, Tuple, Optional


def execute_code(
    code: str,
    stdin_data: str = "",
    env: Optional[Dict[str, str]] = None,
) -> Tuple[int, str, str, Optional[BaseException]]:
    """Execute Python code in a clean namespace.
    
    Args:
        code: Python code to execute
        stdin_data: Data to provide via stdin
        env: Environment variables to set in the execution namespace
        
    Returns:
        Tuple of (exit_code, stdout, stderr, exception)
        - exit_code: 0 for success, 1 for exception
        - stdout: Captured standard output
        - stderr: Captured standard error (includes traceback on exception)
        - exception: The exception object if one occurred, None otherwise
    """
    # Create clean namespace with only builtins
    globals_dict: Dict[str, Any] = {
        "__name__": "__main__",
        "__builtins__": builtins,
    }
    
    # Add environment variables if provided
    if env:
        globals_dict["__env__"] = env
    
    # Capture output
    stdout_buf = io.StringIO()
    stderr_buf = io.StringIO()
    
    # Create StringIO for stdin
    old_stdin = sys.stdin
    sys.stdin = io.StringIO(stdin_data)
    
    exception: Optional[BaseException] = None
    
    try:
        with redirect_stdout(stdout_buf), redirect_stderr(stderr_buf):
            # Compile to catch syntax errors
            compiled = compile(code, "<zeroboot>", "exec")
            exec(compiled, globals_dict, globals_dict)
        
        return 0, stdout_buf.getvalue(), stderr_buf.getvalue(), None
        
    except TimeoutError:
        # Re-raise TimeoutError so caller can handle it
        raise
        
    except BaseException as e:
        exception = e
        # Print traceback to stderr buffer
        traceback.print_exc(file=stderr_buf)
        return 1, stdout_buf.getvalue(), stderr_buf.getvalue(), exception
        
    finally:
        # Restore stdin
        sys.stdin = old_stdin
