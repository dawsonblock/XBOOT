import json
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COMPARE = ROOT / "scripts" / "bench_compare.py"


def artifact(p95_ms: float) -> dict:
    return {
        "generated_at": 1,
        "server_url": "http://localhost:8080",
        "modes": [
            {
                "mode": "warm_pooled_strict",
                "targets": {"python": 1},
                "results": [
                    {
                        "scenario": "python_tiny_expression",
                        "language": "python",
                        "concurrency": 1,
                        "samples": 8,
                        "successes": 8,
                        "avg_latency_ms": 5.0,
                        "p50_latency_ms": 4.5,
                        "p95_latency_ms": p95_ms,
                        "p99_latency_ms": p95_ms + 1.0,
                    }
                ],
            }
        ],
    }


class BenchmarkOutputSchemaTests(unittest.TestCase):
    def test_artifact_shape_contains_required_keys(self):
        sample = artifact(10.0)
        self.assertIn("generated_at", sample)
        self.assertIn("server_url", sample)
        self.assertIn("modes", sample)
        result = sample["modes"][0]["results"][0]
        self.assertIn("scenario", result)
        self.assertIn("language", result)
        self.assertIn("concurrency", result)
        self.assertIn("p95_latency_ms", result)

    def test_bench_compare_reports_delta_table(self):
        with tempfile.TemporaryDirectory() as tmp:
            baseline = Path(tmp) / "baseline.json"
            candidate = Path(tmp) / "candidate.json"
            baseline.write_text(json.dumps(artifact(10.0)))
            candidate.write_text(json.dumps(artifact(7.5)))

            proc = subprocess.run(
                ["python3", str(COMPARE), str(baseline), str(candidate)],
                check=True,
                capture_output=True,
                text=True,
            )

        self.assertIn("Baseline:", proc.stdout)
        self.assertIn("Candidate:", proc.stdout)
        self.assertIn("python_tiny_expression", proc.stdout)
        self.assertIn("-25.0%", proc.stdout)


if __name__ == "__main__":
    unittest.main()
