import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Optional


ROOT = Path(__file__).resolve().parents[1]
NODE = shutil.which("node")
WORKER = ROOT / "guest" / "worker_supervisor.js"
CHILD = ROOT / "guest" / "worker_child.js"


def parse_response(data: bytes):
    header, payload = data.split(b"\n", 1)
    parts = header.decode("utf-8").split()
    request_id_len = int(parts[1])
    exit_code = int(parts[2])
    error_type = parts[3]
    stdout_len = int(parts[4])
    stderr_len = int(parts[5])
    flags = int(parts[6])
    request_id = payload[:request_id_len]
    body = payload[request_id_len:]
    stdout = body[:stdout_len]
    stderr = body[stdout_len : stdout_len + stderr_len]
    return request_id, exit_code, error_type, stdout, stderr, flags


@unittest.skipUnless(NODE, "node is required for Node worker tests")
class NodeWorkerProtocolTests(unittest.TestCase):
    def run_child(self, code: str, *, raw_input: Optional[bytes] = None):
        payload = {
            "request_id": "n1",
            "timeout_ms": 2000,
            "code": code,
            "stdin": "",
            "limits": {
                "stdout_bytes": 1024,
                "stderr_bytes": 1024,
                "tmp_bytes": 4096,
            },
        }
        proc = subprocess.run(
            [NODE, str(CHILD)],
            input=raw_input if raw_input is not None else json.dumps(payload).encode("utf-8"),
            capture_output=True,
            timeout=5,
            env={**os.environ, "ZEROBOOT_CHILD_LIMIT_PROFILE": "compat"},
        )
        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        return parse_response(proc.stdout)

    def run_worker(self, code: str, timeout_ms: int = 2000, *, child_script: Optional[Path] = None):
        proc = subprocess.Popen(
            [NODE, str(WORKER)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={
                **os.environ,
                "ZEROBOOT_CHILD_SCRIPT": str(child_script or CHILD),
                "ZEROBOOT_CHILD_LIMIT_PROFILE": "compat",
            },
        )
        assert proc.stdout is not None
        ready = proc.stdout.readline().decode().strip()
        self.assertEqual(ready, "READY")
        request_id = b"n1"
        code_b = code.encode()
        stdin_b = b""
        header = f"WRK1 {len(request_id)} {timeout_ms} {len(code_b)} {len(stdin_b)}\n".encode()
        assert proc.stdin is not None
        proc.stdin.write(header + request_id + code_b + stdin_b)
        proc.stdin.flush()
        proc.stdin.close()
        resp_header = proc.stdout.readline().decode().strip().split()
        self.assertEqual(resp_header[0], "WRK1R")
        id_len = int(resp_header[1])
        exit_code = int(resp_header[2])
        error_type = resp_header[3]
        stdout_len = int(resp_header[4])
        stderr_len = int(resp_header[5])
        flags = int(resp_header[6])
        rid = proc.stdout.read(id_len)
        stdout = proc.stdout.read(stdout_len)
        stderr = proc.stdout.read(stderr_len)
        proc.stdout.close()
        proc.wait(timeout=5)
        return rid, exit_code, error_type, stdout.decode(), stderr.decode(), flags

    def test_node_child_setup_failure_is_framed(self):
        rid, exit_code, error_type, _stdout, stderr, _flags = self.run_child("", raw_input=b"{bad-json")
        self.assertEqual(rid, b"error")
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, "internal")
        self.assertIn(b"SyntaxError", stderr)

    def test_node_child_temp_environment_aliases_match(self):
        code = (
            "console.log(process.env.TMPDIR === process.env.ZEROBOOT_TMPDIR);"
            "console.log(process.env.TMP === process.env.ZEROBOOT_TMPDIR);"
            "console.log(process.env.TEMP === process.env.ZEROBOOT_TMPDIR);"
            "console.log(process.env.HOME === process.env.ZEROBOOT_TMPDIR);"
            "console.log(process.env.ZEROBOOT_OFFLINE);"
        )
        _rid, exit_code, error_type, stdout, _stderr, _flags = self.run_child(code)
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, "ok")
        self.assertEqual(stdout.splitlines(), [b"true", b"true", b"true", b"true", b"1"])

    def test_node_supervisor_success(self):
        rid, exit_code, error_type, stdout, stderr, flags = self.run_worker("console.log(1 + 1)")
        self.assertEqual(rid, b"n1")
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, "ok")
        self.assertEqual(stdout.strip(), "2")
        self.assertEqual(stderr, "")
        self.assertEqual(flags & 1, 0)

    def test_node_supervisor_timeout(self):
        rid, exit_code, error_type, _stdout, stderr, _flags = self.run_worker("while (true) {}", timeout_ms=50)
        self.assertEqual(rid, b"n1")
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, "timeout")
        self.assertIn("timed out", stderr)

    def test_node_supervisor_malformed_child_frame_is_protocol(self):
        with tempfile.TemporaryDirectory() as tmp:
            child = Path(tmp) / "bad_child.js"
            child.write_text("process.stdout.write('WRK1R bad frame\\n');\n", encoding="utf-8")
            rid, exit_code, error_type, _stdout, stderr, _flags = self.run_worker(
                "console.log('ignored')",
                child_script=child,
            )
        self.assertEqual(rid, b"n1")
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, "protocol")
        self.assertIn("malformed child response", stderr)

    def test_node_supervisor_raw_signal_is_internal(self):
        with tempfile.TemporaryDirectory() as tmp:
            child = Path(tmp) / "sigkill_child.js"
            child.write_text("process.kill(process.pid, 'SIGKILL');\n", encoding="utf-8")
            rid, exit_code, error_type, _stdout, stderr, _flags = self.run_worker(
                "console.log('ignored')",
                child_script=child,
            )
        self.assertEqual(rid, b"n1")
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, "internal")
        self.assertIn("child exited by signal", stderr)

    def test_node_supervisor_short_payload_is_protocol_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            child = Path(tmp) / "short_payload_child.js"
            child.write_text(
                "// Header declares stdout=100 bytes but only writes 5 bytes of payload\n"
                "process.stdout.write('WRK1R 1 0 ok 100 0 0\\n');\n"
                "process.stdout.write(Buffer.from('x'));\n"
                "process.stdout.write(Buffer.from('short'));\n",
                encoding="utf-8",
            )
            rid, exit_code, error_type, _stdout, stderr, _flags = self.run_worker(
                "console.log('ignored')",
                child_script=child,
            )
        self.assertEqual(rid, b"n1")
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, "protocol")
        self.assertIn("malformed child response", stderr)


if __name__ == "__main__":
    unittest.main()
