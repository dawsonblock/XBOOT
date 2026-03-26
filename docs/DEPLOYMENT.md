# Deployment

## Required runtime assumptions

- Linux host with `/dev/kvm`
- Firecracker installed and version-pinned by you
- guest kernel image
- one or more guest rootfs images with the worker assets installed
- templates created by this upgraded build so `template.manifest.json` includes sha256 fields

## Recommended environment

```bash
export ZEROBOOT_AUTH_MODE=prod
export ZEROBOOT_API_KEYS_FILE=/etc/zeroboot/api_keys.json
export ZEROBOOT_REQUEST_LOG_PATH=/var/lib/zeroboot/requests.jsonl
export ZEROBOOT_LOG_CODE=false
export ZEROBOOT_MAX_REQUEST_BODY_BYTES=$((256 * 1024))
export ZEROBOOT_MAX_CODE_BYTES=$((128 * 1024))
export ZEROBOOT_MAX_STDIN_BYTES=$((64 * 1024))
export ZEROBOOT_MAX_STDOUT_BYTES=$((64 * 1024))
export ZEROBOOT_MAX_STDERR_BYTES=$((64 * 1024))
export ZEROBOOT_MAX_TOTAL_OUTPUT_BYTES=$((96 * 1024))
export ZEROBOOT_MAX_BATCH_SIZE=16
export ZEROBOOT_MAX_TIMEOUT_SECS=30
export ZEROBOOT_MAX_CONCURRENT_REQUESTS=32
export ZEROBOOT_TRUSTED_PROXIES=127.0.0.1
export ZEROBOOT_HEALTH_CACHE_TTL_SECS=10
export ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true
export ZEROBOOT_ALLOWED_FIRECRACKER_VERSION="firecracker v1.8.0"
```

## Build flow

```bash
make build
make guest-python PY_ROOTFS_TEMPLATE=/path/to/base-rootfs-tree
make image-python
make template-python
```

`make guest-python` and `make guest-node` build deterministic staging trees under `build/staging/...`.
`make image-python` and `make image-node` turn those staging trees into ext4 artifacts with `mkfs.ext4 -d`.
`template.manifest.json` now records language, protocol version, Firecracker version, and sha256 hashes for kernel, rootfs, and snapshot files.

## Start the server

```bash
./target/release/zeroboot serve "python:/var/lib/zeroboot/templates/python,node:/var/lib/zeroboot/templates/node" 8080
```

At startup the server now:

- verifies template sizes and sha256 hashes
- checks manifest language and protocol version
- optionally checks Firecracker version if `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION` is set
- quarantines bad templates instead of loading them

Useful probes:

- `/live` — process liveness only
- `/ready` — startup verification state only; quarantined templates appear here immediately
- `/health` and `/v1/health` — cached deep guest probes for templates that passed startup verification
- `/v1/metrics` — Prometheus metrics including template readiness, quarantine count, process RSS, and execution-slot capacity gauges

## Systemd

Use `deploy/zeroboot.service` as the baseline. It now:

- points at `/var/lib/zeroboot/templates/...`
- supports an optional `/etc/zeroboot/zeroboot.env`
- enables strict template hash verification by default
- sets conservative restart and memory limits

## Preflight

Run `scripts/preflight.sh` against the kernel and rootfs artifacts before deployment.
Generate API keys with `scripts/make_api_keys.py --output api_keys.json`.

If `ZEROBOOT_WORKDIR` points at a template directory, `preflight.sh` also validates `template.manifest.json`.
If `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION` is set, `preflight.sh` rejects mismatched Firecracker binaries.

## Remaining gaps

- no warm VM pool yet
- no live KVM/Firecracker CI lane yet
- no signed artifact promotion flow yet
- plain-text API key storage still needs a follow-up hardening pass
