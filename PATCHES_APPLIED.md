# Zeroboot upgraded pass7

This archive applies a focused operational cleanup on top of `upgraded-pass6`.

## Applied in this pass

- Fixed `verify.sh` so smoke validation extracts exit codes correctly.
- Replaced fragile stdout parsing in `verify.sh` with `awk`-based extraction.
- Added process RSS metric `zeroboot_memory_usage_bytes`.
- Added execution-slot gauges:
  - `zeroboot_execution_slots_available`
  - `zeroboot_execution_slots_used`
  - `zeroboot_execution_slots_capacity`
- Rebuilt `deploy/grafana-dashboard.json` so it matches the metrics the server actually emits.
- Removed the hard-coded Prometheus datasource UID from the dashboard and replaced it with Grafana input wiring.
- Updated API, deployment, and README docs to match the current metrics surface.
- Added `tests/test_grafana_dashboard_metrics.py` to catch dashboard drift.
- Added `PRODUCTION_PATCH_PLAN_PASS7.md` with the next file-by-file production roadmap.

## Validation run in this environment

- `python3 -m unittest discover -s tests -p 'test_*.py'`
- `python3 -m py_compile guest/worker.py sdk/python/zeroboot/client.py scripts/make_api_keys.py scripts/validate_template_manifest.py tests/test_grafana_dashboard_metrics.py`
- `bash -n verify.sh deploy/deploy.sh scripts/build_guest_rootfs.sh scripts/build_rootfs_image.sh scripts/preflight.sh`

## Not validated here

- `cargo build` / `cargo test` were not run because the Rust toolchain is not installed in this container.
- live Firecracker/KVM execution was not exercised here.

## Still not complete

- no warm VM pool yet
- no self-hosted KVM CI lane yet
