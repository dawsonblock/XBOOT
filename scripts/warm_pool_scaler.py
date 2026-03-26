#!/usr/bin/env python3
"""
Warm Pool Autoscaler (EXPERIMENTAL)

WARNING: This script is experimental. There is no server-side VM pool
implementation yet. This script assumes metrics that don't exist in the
current zeroboot server.

Manages a pool of pre-warmed VMs for fast request handling.
Scales pool size based on demand metrics.

Usage:
    python3 warm_pool_scaler.py --server-url http://localhost:8080 --min 2 --max 10
    python3 warm_pool_scaler.py --server-url http://localhost:8080 --policy aggressive
    python3 warm_pool_scaler.py --server-url http://localhost:8080 --dry-run

The autoscaler:
1. Monitors request queue depth and latency
2. Maintains min idle VMs to reduce cold-start latency
3. Scales up when queue depth exceeds threshold
4. Scales down when idle VMs are unused for cooldown period
"""

import argparse
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, List, Optional

import requests


@dataclass
class PoolMetrics:
    """Current pool metrics."""
    active_count: int
    idle_count: int
    max_count: int
    request_queue_depth: int
    avg_latency_ms: float
    requests_per_second: float


@dataclass
class ScaleDecision:
    """Autoscaling decision."""
    action: str  # "scale_up", "scale_down", "maintain"
    target_count: int
    reason: str


class WarmPoolAutoscaler:
    """Autoscaler for warm VM pool."""
    
    # Scale thresholds
    SCALE_UP_QUEUE_DEPTH = 5  # Scale up when queue exceeds this
    SCALE_DOWN_IDLE_THRESHOLD = 0.8  # Scale down when 80% idle for cooldown
    COOLDOWN_SECONDS = 60
    
    def __init__(self, server_url: str, min_size: int, max_size: int,
                 dry_run: bool = False):
        self.server_url = server_url.rstrip('/')
        self.min_size = min_size
        self.max_size = max_size
        self.dry_run = dry_run
        self.last_scale_time = datetime.min
        self.current_target = min_size
        
    def get_pool_metrics(self) -> Optional[PoolMetrics]:
        """Fetch current pool metrics from server."""
        try:
            resp = requests.get(f"{self.server_url}/v1/metrics", timeout=5)
            if resp.status_code != 200:
                return None
                
            data = resp.json()
            
            # Parse metrics - these depend on actual server response format
            return PoolMetrics(
                active_count=data.get("active_vms", 0),
                idle_count=data.get("idle_vms", 0),
                max_count=self.max_size,
                request_queue_depth=data.get("queue_depth", 0),
                avg_latency_ms=data.get("avg_latency_ms", 0),
                requests_per_second=data.get("rps", 0),
            )
        except Exception as e:
            print(f"Failed to fetch metrics: {e}", file=sys.stderr)
            return None
    
    def should_scale_up(self, metrics: PoolMetrics) -> bool:
        """Determine if we should scale up."""
        # Always maintain minimum
        if metrics.idle_count < self.min_size:
            return True
            
        # Scale if queue is building up
        if metrics.request_queue_depth > self.SCALE_UP_QUEUE_DEPTH:
            return True
            
        # Scale if latency is high
        if metrics.avg_latency_ms > 500:  # > 500ms latency
            return True
            
        return False
    
    def should_scale_down(self, metrics: PoolMetrics) -> bool:
        """Determine if we should scale down."""
        # Don't scale below minimum
        if metrics.active_count + metrics.idle_count <= self.min_size:
            return False
            
        # Check cooldown
        if datetime.now() - self.last_scale_time < timedelta(seconds=self.COOLDOWN_SECONDS):
            return False
            
        # Scale down if mostly idle
        if metrics.active_count == 0 and metrics.idle_count > 1:
            # Check if idle for a while (simplified check)
            return True
            
        return False
    
    def make_decision(self, metrics: PoolMetrics) -> ScaleDecision:
        """Make scaling decision based on current metrics."""
        current_total = metrics.active_count + metrics.idle_count
        
        if self.should_scale_up(metrics):
            new_target = min(current_total + 2, self.max_size)
            return ScaleDecision(
                action="scale_up",
                target_count=new_target,
                reason=f"queue_depth={metrics.request_queue_depth}, latency={metrics.avg_latency_ms:.0f}ms"
            )
        
        if self.should_scale_down(metrics):
            new_target = max(current_total - 1, self.min_size)
            return ScaleDecision(
                action="scale_down",
                target_count=new_target,
                reason="idle pool, scaling down after cooldown"
            )
        
        return ScaleDecision(
            action="maintain",
            target_count=current_total,
            reason=f"balanced - active={metrics.active_count}, idle={metrics.idle_count}"
        )
    
    def scale_pool(self, target_count: int) -> bool:
        """Request server to scale pool to target count."""
        if self.dry_run:
            print(f"[DRY RUN] Would scale pool to {target_count} VMs")
            return True
            
        try:
            resp = requests.post(
                f"{self.server_url}/v1/admin/scale",
                json={"target_pool_size": target_count},
                timeout=10
            )
            return resp.status_code == 200
        except Exception as e:
            print(f"Failed to scale pool: {e}", file=sys.stderr)
            return False
    
    def run_once(self) -> bool:
        """Run one autoscaling iteration."""
        metrics = self.get_pool_metrics()
        if metrics is None:
            print("Warning: Could not fetch metrics, skipping iteration")
            return False
            
        print(f"Metrics: active={metrics.active_count}, idle={metrics.idle_count}, "
              f"queue={metrics.request_queue_depth}, latency={metrics.avg_latency_ms:.0f}ms")
        
        decision = self.make_decision(metrics)
        print(f"Decision: {decision.action} to {decision.target_count} - {decision.reason}")
        
        if decision.action != "maintain":
            success = self.scale_pool(decision.target_count)
            if success:
                self.last_scale_time = datetime.now()
                self.current_target = decision.target_count
            return success
            
        return True
    
    def run_loop(self, interval: int = 10):
        """Run autoscaling loop."""
        print(f"Starting warm pool autoscaler (min={self.min_size}, max={self.max_size})")
        
        while True:
            try:
                self.run_once()
            except KeyboardInterrupt:
                print("\nStopping autoscaler")
                break
            except Exception as e:
                print(f"Error in autoscaler loop: {e}", file=sys.stderr)
                
            time.sleep(interval)


def main():
    parser = argparse.ArgumentParser(description="Warm Pool Autoscaler")
    parser.add_argument("--server-url", required=True, help="Zeroboot server URL")
    parser.add_argument("--min", type=int, default=2, help="Minimum pool size")
    parser.add_argument("--max", type=int, default=10, help="Maximum pool size")
    parser.add_argument("--interval", type=int, default=10, help="Check interval (seconds)")
    parser.add_argument("--policy", choices=["conservative", "aggressive", "balanced"],
                        default="balanced", help="Scaling policy")
    parser.add_argument("--dry-run", action="store_true", help="Dry run mode")
    parser.add_argument("--once", action="store_true", help="Run once instead of loop")
    
    args = parser.parse_args()
    
    # Adjust thresholds based on policy
    if args.policy == "aggressive":
        WarmPoolAutoscaler.SCALE_UP_QUEUE_DEPTH = 3
        WarmPoolAutoscaler.COOLDOWN_SECONDS = 30
    elif args.policy == "conservative":
        WarmPoolAutoscaler.SCALE_UP_QUEUE_DEPTH = 10
        WarmPoolAutoscaler.COOLDOWN_SECONDS = 120
    
    autoscaler = WarmPoolAutoscaler(
        server_url=args.server_url,
        min_size=args.min,
        max_size=args.max,
        dry_run=args.dry_run,
    )
    
    if args.once:
        autoscaler.run_once()
    else:
        autoscaler.run_loop(args.interval)


if __name__ == "__main__":
    main()