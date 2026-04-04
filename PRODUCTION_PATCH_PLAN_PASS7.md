# Zeroboot pass7 patch plan

This pass does two things:

1. fixes concrete defects that were already present in pass6
2. leaves a file-by-file map for the next real production pass

## Applied in this archive

### 1. `verify.sh`
- fixed broken exit-code extraction
- replaced fragile `sed` parsing with `awk`-based extraction for stdout and exit code
- added an explicit cargo precheck so the failure mode is clear

### 2. `src/api/handlers.rs`
- added `zeroboot_memory_usage_bytes` by reading process RSS from `/proc/self/statm`
- added execution slot gauges:
  - `zeroboot_execution_slots_available`
  - `zeroboot_execution_slots_used`
  - `zeroboot_execution_slots_capacity`

### 3. `deploy/grafana-dashboard.json`
- removed stale queries against metrics the server does not emit
- removed the hard-coded Grafana Cloud datasource UID
- aligned panels with current server metrics

### 4. docs
- updated API, deployment, and README text to match the current metrics surface

### 5. tests
- added `tests/test_grafana_dashboard_metrics.py` so the dashboard stops drifting away from emitted metrics

## Next pass: file-by-file production work

### P0 — trust and release hygiene

Template signing, hashed API key verification, and startup validation are implemented.
The remaining work in this area:

#### `.github/workflows/ci.yml`
- add a self-hosted Linux KVM lane
- run live template boot + test-exec against Python and Node templates

### P1 — latency and throughput

#### new: `src/vmm/pool.rs`
- build a bounded warm pool per language
- background refill with health accounting

#### `src/api/handlers.rs`
- serve from pool before falling back to cold fork
- emit cold-start vs warm-hit metrics

#### new: `src/vmm/pool_metrics.rs`
- pool depth
- refill failures
- warm hit rate

### P2 — deployment hardening

#### `deploy/zeroboot.service`
- consider Firecracker jailer integration
- tighten device access to only the required KVM surface

#### `deploy/deploy.sh`
- move from ad-hoc SCP deployment to versioned release directories
- add health-checked rollout and rollback

### P3 — auth and secrets

API key hashing (HMAC-SHA256 + server pepper) and constant-time comparison are implemented
in `src/auth.rs`. The remaining work:

#### `scripts/make_api_keys.py`
- emit one-time-display secrets and hashed server records

## Validation target for the next pass

A pass can reasonably be called production-grade only after all of these are true:

- live KVM CI exists
- warm pools exist
- dashboard, docs, and emitted metrics stay in sync
