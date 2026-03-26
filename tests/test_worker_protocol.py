import os
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKER = ROOT / 'guest' / 'worker.py'

class WorkerProtocolTests(unittest.TestCase):
    def run_worker(self, code: str, timeout_ms: int = 2000):
        proc = subprocess.Popen(
            [sys.executable, str(WORKER)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            env={**os.environ, 'ZEROBOOT_WORKER_MAX_REQUESTS': '32'},
        )
        ready = proc.stdout.readline().decode().strip()
        self.assertEqual(ready, 'READY')
        request_id = b't1'
        code_b = code.encode()
        stdin_b = b''
        header = f'WRK1 {len(request_id)} {timeout_ms} {len(code_b)} {len(stdin_b)}\n'.encode()
        assert proc.stdin is not None
        proc.stdin.write(header + request_id + code_b + stdin_b)
        proc.stdin.flush()
        proc.stdin.close()
        assert proc.stdout is not None
        resp_header = proc.stdout.readline().decode().strip().split()
        self.assertEqual(resp_header[0], 'WRK1R')
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

    def test_python_success(self):
        rid, exit_code, error_type, stdout, stderr, flags = self.run_worker('print(1+1)')
        self.assertEqual(rid, b't1')
        self.assertEqual(exit_code, 0)
        self.assertEqual(error_type, 'ok')
        self.assertEqual(stdout.strip(), '2')
        self.assertEqual(stderr, '')
        self.assertEqual(flags & 1, 0)

    def test_python_timeout_requests_recycle(self):
        rid, exit_code, error_type, _stdout, stderr, flags = self.run_worker('while True: pass', timeout_ms=50)
        self.assertEqual(rid, b't1')
        self.assertEqual(exit_code, -1)
        self.assertEqual(error_type, 'timeout')
        self.assertIn('timed out', stderr)
        self.assertNotEqual(flags & 4, 0)

if __name__ == '__main__':
    unittest.main()
