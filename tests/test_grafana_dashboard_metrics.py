import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DASHBOARD = ROOT / "deploy" / "grafana-dashboard.json"

ALLOWED_METRICS = {
    "zeroboot_concurrent_forks",
    "zeroboot_exec_time_milliseconds_bucket",
    "zeroboot_execution_slots_available",
    "zeroboot_execution_slots_used",
    "zeroboot_fork_time_milliseconds_bucket",
    "zeroboot_language_executions_total",
    "zeroboot_memory_usage_bytes",
    "zeroboot_queue_wait_time_milliseconds_bucket",
    "zeroboot_template_ready",
    "zeroboot_total_errors",
    "zeroboot_total_executions",
    "zeroboot_total_time_milliseconds_bucket",
    "zeroboot_total_timeouts",
}


class GrafanaDashboardMetricTests(unittest.TestCase):
    def test_dashboard_only_uses_supported_metric_prefixes(self):
        data = json.loads(DASHBOARD.read_text())
        exprs = []
        for panel in data.get("panels", []):
            for target in panel.get("targets", []):
                expr = target.get("expr")
                if expr:
                    exprs.append(expr)

        seen = set()
        for expr in exprs:
            seen.update(re.findall(r"zeroboot_[a-z0-9_]+", expr))

        self.assertTrue(seen, "dashboard should reference zeroboot metrics")
        self.assertTrue(seen.issubset(ALLOWED_METRICS), sorted(seen - ALLOWED_METRICS))

    def test_dashboard_uses_portable_prometheus_input(self):
        data = json.loads(DASHBOARD.read_text())
        for panel in data.get("panels", []):
            ds = panel.get("datasource", {})
            self.assertEqual(ds.get("uid"), "${DS_PROMETHEUS}")


if __name__ == "__main__":
    unittest.main()
