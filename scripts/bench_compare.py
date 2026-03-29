#!/usr/bin/env python3
import argparse
import json
from pathlib import Path


def load_artifact(path: Path) -> dict:
    return json.loads(path.read_text())


def flatten(artifact: dict) -> dict:
    rows = {}
    for mode in artifact.get("modes", []):
        mode_name = mode.get("mode", "unknown")
        for result in mode.get("results", []):
            key = (
                mode_name,
                result.get("scenario"),
                result.get("language"),
                result.get("concurrency"),
            )
            rows[key] = result
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description="Compare XBOOT benchmark artifacts")
    parser.add_argument("baseline", help="Baseline benchmark artifact JSON")
    parser.add_argument("candidate", help="Candidate benchmark artifact JSON")
    args = parser.parse_args()

    baseline_path = Path(args.baseline)
    candidate_path = Path(args.candidate)
    baseline = flatten(load_artifact(baseline_path))
    candidate = flatten(load_artifact(candidate_path))

    print(f"Baseline: {baseline_path}")
    print(f"Candidate: {candidate_path}")
    print()
    print("| Mode | Scenario | Lang | Concurrency | Baseline P95 ms | Candidate P95 ms | Delta % |")
    print("| --- | --- | --- | ---: | ---: | ---: | ---: |")
    for key in sorted(set(baseline) & set(candidate)):
        before = baseline[key]
        after = candidate[key]
        baseline_p95 = float(before.get("p95_latency_ms", 0.0))
        candidate_p95 = float(after.get("p95_latency_ms", 0.0))
        delta_pct = 0.0
        if baseline_p95 > 0:
            delta_pct = ((candidate_p95 - baseline_p95) / baseline_p95) * 100.0
        mode, scenario, language, concurrency = key
        print(
            f"| {mode} | {scenario} | {language} | {concurrency} | "
            f"{baseline_p95:.2f} | {candidate_p95:.2f} | {delta_pct:+.1f}% |"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
