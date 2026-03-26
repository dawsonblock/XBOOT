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
FLAG_RECYCLE_REQUESTED = 4
MAX_REQUESTS = int(os.environ.get("ZEROBOOT_WORKER_MAX_REQUESTS", "128"))
SAFE_MODULE_PREFIXES = ("encodings", "importlib", "zipimport", "codecs")
BASE_CWD = os.getcwd()
BASE_ENV = dict(os.environ)
BASE_MODULE_NAMES = set(sys.modules)
BASE_BUILTINS = {k: getattr(builtins, k) for k in dir(builtins)}
REQUEST_COUNT = 0


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


def timeout_handler(_signum, _frame):
    raise TimeoutError("execution timed out")


def restore_process_state() -> bool:
    recycle = False
    try:
        os.chdir(BASE_CWD)
    except Exception:
        recycle = True
    try:
        os.environ.clear()
        os.environ.update(BASE_ENV)
    except Exception:
        recycle = True
    for name in list(sys.modules):
        if name in BASE_MODULE_NAMES:
            continue
        if name.startswith(SAFE_MODULE_PREFIXES):
            continue
        sys.modules.pop(name, None)
        recycle = True
    for name, value in BASE_BUILTINS.items():
        try:
            setattr(builtins, name, value)
        except Exception:
            recycle = True
    for name in list(vars(builtins)):
        if name not in BASE_BUILTINS:
            try:
                delattr(builtins, name)
            except Exception:
                recycle = True
    gc.collect()
    return recycle


signal.signal(signal.SIGALRM, timeout_handler)
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

        stdout_io = io.StringIO()
        stderr_io = io.StringIO()
        globals_dict = {"__name__": "__main__", "__builtins__": builtins}
        locals_dict = globals_dict
        exit_code = 0
        error_type = "ok"
        recycle = False
        signal.setitimer(signal.ITIMER_REAL, max(timeout_ms, 1) / 1000.0)
        old_stdin = sys.stdin
        try:
            sys.stdin = io.StringIO(stdin_data)
            with contextlib.redirect_stdout(stdout_io), contextlib.redirect_stderr(stderr_io):
                exec(compile(code, "<zeroboot>", "exec"), globals_dict, locals_dict)
        except TimeoutError:
            exit_code = -1
            error_type = "timeout"
            recycle = True
            stderr_io.write("execution timed out\n")
        except BaseException:
            exit_code = 1
            error_type = "runtime"
            recycle = True
            traceback.print_exc(file=stderr_io)
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            sys.stdin = old_stdin

        stdout_bytes = stdout_io.getvalue().encode("utf-8", "replace")
        stderr_bytes = stderr_io.getvalue().encode("utf-8", "replace")
        flags = 0
        stdout_bytes, stdout_truncated = truncate(stdout_bytes, MAX_STDOUT)
        stderr_bytes, stderr_truncated = truncate(stderr_bytes, MAX_STDERR)
        if stdout_truncated:
            flags |= FLAG_STDOUT_TRUNCATED
            recycle = True
        if stderr_truncated:
            flags |= FLAG_STDERR_TRUNCATED
            recycle = True
        REQUEST_COUNT += 1
        recycle = restore_process_state() or recycle or REQUEST_COUNT >= MAX_REQUESTS
        if recycle:
            flags |= FLAG_RECYCLE_REQUESTED
        write_response(request_id, exit_code, error_type, stdout_bytes, stderr_bytes, flags)
        if recycle:
            break
    except EOFError:
        break
    except BaseException:
        err = traceback.format_exc().encode("utf-8", "replace")
        write_response(b"error", -1, "internal", b"", err[:MAX_STDERR], FLAG_STDERR_TRUNCATED | FLAG_RECYCLE_REQUESTED if len(err) > MAX_STDERR else FLAG_RECYCLE_REQUESTED)
        break
