import json
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CHILD = ROOT / "guest" / "worker_child.py"


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


class GuestWorkerSubprocessTests(unittest.TestCase):
    def run_child(self, code: str):
        payload = {
            "request_id": "t1",
            "timeout_ms": 2000,
            "code": code,
            "stdin": "",
            "limits": {
                "stdout_bytes": 1024,
                "stderr_bytes": 1024,
                "tmp_bytes": 4096,
                "memory_bytes": 256 * 1024 * 1024,
                "nofile": 32,
                "nproc": 8,
                "fsize_bytes": 4096,
            },
        }
        proc = subprocess.run(
            [sys.executable, str(CHILD)],
            input=json.dumps(payload).encode("utf-8"),
            capture_output=True,
            timeout=5,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr.decode("utf-8", "replace"))
        return parse_response(proc.stdout)

    def test_child_does_not_expect_code_in_environment(self):
        request_id, exit_code, error_type, stdout, _stderr, _flags = self.run_child(
            "import os\nprint('ZEROBOOT_EXEC_CODE' in os.environ)"
        )
        self.assertEqual(request_id, b"t1")
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, "ok")
        self.assertEqual(stdout.strip(), b"False")

    def test_child_exits_after_one_execution(self):
        request_id, exit_code, error_type, stdout, _stderr, _flags = self.run_child('print("test")')
        self.assertEqual(request_id, b"t1")
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, "ok")
        self.assertEqual(stdout.strip(), b"test")

    def test_temp_directory_is_scoped_per_request(self):
        code = (
            "import os\n"
            "tmp = os.environ['ZEROBOOT_TMPDIR']\n"
            "open(os.path.join(tmp, 'note.txt'), 'w').write('hello')\n"
            "print(os.path.exists(os.path.join(tmp, 'note.txt')))\n"
        )
        _, exit_code, error_type, stdout, _stderr, _flags = self.run_child(code)
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, "ok")
        self.assertEqual(stdout.strip(), b"True")


if __name__ == "__main__":
    unittest.main()
