import importlib.util
import sys
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCALER_PATH = ROOT / "scripts" / "warm_pool_scaler.py"
SPEC = importlib.util.spec_from_file_location("warm_pool_scaler", SCALER_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules["warm_pool_scaler"] = MODULE
SPEC.loader.exec_module(MODULE)


class DummyResponse:
    def __init__(self, payload):
        self.payload = payload

    def raise_for_status(self):
        return None

    def json(self):
        return self.payload


class WarmPoolScalerTests(unittest.TestCase):
    def test_fetches_real_admin_pool_shape(self):
        autoscaler = MODULE.WarmPoolAutoscaler(
            server_url="http://localhost:8080",
            admin_token="zb_admin.test",
            min_size=1,
            max_size=4,
            dry_run=True,
        )
        payload = {
            "generated_at": 123,
            "status": "ok",
            "lanes": {
                "python": {
                    "idle": 1,
                    "active": 0,
                    "target_idle": 1,
                    "waiters": 0,
                    "healthy_idle": 1,
                    "quarantined": 0,
                    "avg_borrow_latency_ms": 12.5,
                    "recent_recycles": [],
                }
            },
        }
        with mock.patch.object(MODULE.requests, "get", return_value=DummyResponse(payload)) as get_mock:
            lanes = autoscaler.get_pool_state()

        self.assertIn("python", lanes)
        self.assertEqual(lanes["python"].avg_borrow_latency_ms, 12.5)
        self.assertIn("/v1/admin/pool", get_mock.call_args.args[0])

    def test_run_once_scales_per_language_targets(self):
        autoscaler = MODULE.WarmPoolAutoscaler(
            server_url="http://localhost:8080",
            admin_token="zb_admin.test",
            min_size=1,
            max_size=4,
            dry_run=False,
        )
        payload = {
            "generated_at": 123,
            "status": "ok",
            "lanes": {
                "python": {
                    "idle": 0,
                    "active": 1,
                    "target_idle": 1,
                    "waiters": 2,
                    "healthy_idle": 0,
                    "quarantined": 0,
                    "avg_borrow_latency_ms": 250.0,
                    "recent_recycles": [],
                },
                "node": {
                    "idle": 1,
                    "active": 0,
                    "target_idle": 1,
                    "waiters": 0,
                    "healthy_idle": 1,
                    "quarantined": 0,
                    "avg_borrow_latency_ms": 10.0,
                    "recent_recycles": [],
                },
            },
        }
        with mock.patch.object(MODULE.requests, "get", return_value=DummyResponse(payload)):
            with mock.patch.object(MODULE.requests, "post", return_value=DummyResponse({})) as post_mock:
                success = autoscaler.run_once()

        self.assertTrue(success)
        self.assertIn("/v1/admin/scale", post_mock.call_args.args[0])
        self.assertEqual(
            post_mock.call_args.kwargs["json"],
            {"targets": {"python": 2, "node": 1}},
        )


if __name__ == "__main__":
    unittest.main()
