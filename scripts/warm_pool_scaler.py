#!/usr/bin/env python3
"""
Warm pool autoscaler for the pooled strict lane.

This script talks to the real admin API:
  - GET  /v1/admin/pool
  - POST /v1/admin/scale

It scales targets per language based on idle capacity, waiters, and borrow latency.
"""

import argparse
import os
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Dict, Optional

import requests


@dataclass
class LaneMetrics:
    idle: int
    active: int
    target_idle: int
    waiters: int
    healthy_idle: int
    quarantined: int
    avg_borrow_latency_ms: float


@dataclass
class ScaleDecision:
    action: str
    target_idle: int
    reason: str


class WarmPoolAutoscaler:
    SCALE_UP_WAITER_THRESHOLD = 1
    SCALE_UP_LATENCY_MS = 200.0
    SCALE_DOWN_LATENCY_MS = 25.0
    COOLDOWN_SECONDS = 60

    def __init__(self, server_url: str, admin_token: str, min_size: int, max_size: int, dry_run: bool = False):
        self.server_url = server_url.rstrip("/")
        self.admin_token = admin_token
        self.min_size = min_size
        self.max_size = max_size
        self.dry_run = dry_run
        self.last_scale_time = datetime.min

    def _headers(self) -> Dict[str, str]:
        return {"Authorization": f"Bearer {self.admin_token}"}

    def get_pool_state(self) -> Optional[Dict[str, LaneMetrics]]:
        try:
            resp = requests.get(
                f"{self.server_url}/v1/admin/pool",
                headers=self._headers(),
                timeout=5,
            )
            resp.raise_for_status()
            data = resp.json()
            lanes = {}
            for language, lane in data.get("lanes", {}).items():
                lanes[language] = LaneMetrics(
                    idle=int(lane.get("idle", 0)),
                    active=int(lane.get("active", 0)),
                    target_idle=int(lane.get("target_idle", 0)),
                    waiters=int(lane.get("waiters", 0)),
                    healthy_idle=int(lane.get("healthy_idle", 0)),
                    quarantined=int(lane.get("quarantined", 0)),
                    avg_borrow_latency_ms=float(lane.get("avg_borrow_latency_ms", 0.0)),
                )
            return lanes
        except Exception as exc:
            print(f"Failed to fetch pool state: {exc}", file=sys.stderr)
            return None

    def decide_lane(self, language: str, lane: LaneMetrics) -> ScaleDecision:
        if lane.target_idle < self.min_size:
            return ScaleDecision("scale_up", self.min_size, "below configured minimum")

        if lane.waiters >= self.SCALE_UP_WAITER_THRESHOLD:
            return ScaleDecision(
                "scale_up",
                min(lane.target_idle + 1, self.max_size),
                f"waiters={lane.waiters}",
            )

        if lane.avg_borrow_latency_ms >= self.SCALE_UP_LATENCY_MS:
            return ScaleDecision(
                "scale_up",
                min(lane.target_idle + 1, self.max_size),
                f"avg_borrow_latency_ms={lane.avg_borrow_latency_ms:.1f}",
            )

        cooldown_ready = datetime.now() - self.last_scale_time >= timedelta(seconds=self.COOLDOWN_SECONDS)
        if (
            cooldown_ready
            and lane.idle > self.min_size
            and lane.waiters == 0
            and lane.active == 0
            and lane.avg_borrow_latency_ms <= self.SCALE_DOWN_LATENCY_MS
        ):
            return ScaleDecision(
                "scale_down",
                max(lane.target_idle - 1, self.min_size),
                "idle pool exceeds current demand",
            )

        return ScaleDecision("maintain", lane.target_idle, f"{language} balanced")

    def scale_pool(self, targets: Dict[str, int]) -> bool:
        if self.dry_run:
            print(f"[DRY RUN] Would set targets to {targets}")
            return True
        try:
            resp = requests.post(
                f"{self.server_url}/v1/admin/scale",
                headers=self._headers(),
                json={"targets": targets},
                timeout=10,
            )
            resp.raise_for_status()
            return True
        except Exception as exc:
            print(f"Failed to scale pool: {exc}", file=sys.stderr)
            return False

    def run_once(self) -> bool:
        lanes = self.get_pool_state()
        if lanes is None:
            return False
        if not lanes:
            print("No pool lanes available")
            return False

        desired_targets: Dict[str, int] = {}
        any_change = False
        for language, lane in sorted(lanes.items()):
            decision = self.decide_lane(language, lane)
            desired_targets[language] = decision.target_idle
            if decision.action != "maintain":
                any_change = True
            print(
                f"{language}: idle={lane.idle} active={lane.active} target={lane.target_idle} "
                f"waiters={lane.waiters} avg_borrow_latency_ms={lane.avg_borrow_latency_ms:.1f} "
                f"-> {decision.action} ({decision.reason})"
            )

        if not any_change:
            return True

        success = self.scale_pool(desired_targets)
        if success:
            self.last_scale_time = datetime.now()
        return success

    def run_loop(self, interval: int) -> None:
        while True:
            try:
                self.run_once()
            except KeyboardInterrupt:
                print("\nStopping autoscaler")
                return
            except Exception as exc:
                print(f"Autoscaler error: {exc}", file=sys.stderr)
            time.sleep(interval)


def main() -> int:
    parser = argparse.ArgumentParser(description="Warm pool autoscaler")
    parser.add_argument("--server-url", required=True, help="Zeroboot server URL")
    parser.add_argument("--admin-token", help="Admin API bearer token")
    parser.add_argument("--min", type=int, default=1, help="Minimum idle target per language")
    parser.add_argument("--max", type=int, default=8, help="Maximum idle target per language")
    parser.add_argument("--interval", type=int, default=10, help="Autoscaler loop interval in seconds")
    parser.add_argument("--policy", choices=["conservative", "aggressive", "balanced"], default="balanced")
    parser.add_argument("--dry-run", action="store_true", help="Print intended changes without applying them")
    parser.add_argument("--once", action="store_true", help="Run one iteration and exit")
    args = parser.parse_args()

    admin_token = args.admin_token or os.environ.get("ZEROBOOT_ADMIN_API_KEY")
    if not admin_token:
        print("Missing admin token. Pass --admin-token or set ZEROBOOT_ADMIN_API_KEY.", file=sys.stderr)
        return 2

    if args.policy == "aggressive":
        WarmPoolAutoscaler.SCALE_UP_WAITER_THRESHOLD = 0
        WarmPoolAutoscaler.SCALE_UP_LATENCY_MS = 100.0
        WarmPoolAutoscaler.COOLDOWN_SECONDS = 30
    elif args.policy == "conservative":
        WarmPoolAutoscaler.SCALE_UP_WAITER_THRESHOLD = 2
        WarmPoolAutoscaler.SCALE_UP_LATENCY_MS = 350.0
        WarmPoolAutoscaler.COOLDOWN_SECONDS = 120

    autoscaler = WarmPoolAutoscaler(
        server_url=args.server_url,
        admin_token=admin_token,
        min_size=args.min,
        max_size=args.max,
        dry_run=args.dry_run,
    )
    if args.once:
        return 0 if autoscaler.run_once() else 1
    autoscaler.run_loop(args.interval)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
