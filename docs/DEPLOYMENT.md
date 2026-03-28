# Deployment

## Required runtime assumptions

- Linux host with `/dev/kvm`
- Firecracker installed and version-pinned by you
- guest kernel image
- one or more guest rootfs images with the worker assets installed
- templates created by this upgraded build so `template.manifest.json` includes sha256 fields

## Official artifact set

Fetch the pinned Ubuntu 22.04 / Firecracker 1.12.0 artifact set with:

```bash
bash scripts/fetch_official_artifacts.sh /var/lib/zeroboot/artifacts
```

That downloads and verifies:

- Firecracker `1.12.0` x86_64
- guest kernel `vmlinux-5.10.223`
- base Ubuntu 22.04 rootfs `ubuntu-22.04.ext4`
- Node.js runtime tarball `v20.20.2`

The exact URLs and sha256 values are also locked in [runtime-artifacts.lock.json](/Users/dawsonblock/Downloads/XBOOT-main-2/manifests/runtime-artifacts.lock.json).

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
export ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=true
export ZEROBOOT_KEYRING_PATH=/etc/zeroboot/keyring.json
export ZEROBOOT_ALLOWED_FIRECRACKER_VERSION="1.12.0"
export ZEROBOOT_ALLOWED_FC_BINARY_SHA256=<sha256-of-firecracker-binary>
export ZEROBOOT_RELEASE_CHANNEL=prod
export ZEROBOOT_MIN_FREE_BYTES=$((512 * 1024 * 1024))
export ZEROBOOT_MIN_FREE_INODES=10000
```

This first hardened release is **offline-only**. The current Firecracker path does not configure a guest NIC, so networked execution profiles are intentionally deferred.

## Build flow

```bash
make build
make guest-python PY_ROOTFS_TEMPLATE=/path/to/base-rootfs-tree
make image-python
make template-python
```

For Node guest images, install the pinned Node runtime into the template tree first:

```bash
bash scripts/install_node_runtime.sh /path/to/base-rootfs-tree /var/lib/zeroboot/artifacts
make guest-node NODE_ROOTFS_TEMPLATE=/path/to/base-rootfs-tree
make image-node
make template-node
```

`make guest-python` and `make guest-node` build deterministic staging trees under `build/staging/...`.
`make image-python` and `make image-node` turn those staging trees into ext4 artifacts with `mkfs.ext4 -d`.
`template.manifest.json` now records language, protocol version, Firecracker version, and sha256 hashes for kernel, rootfs, and snapshot files.

## Start the server

```bash
./target/release/zeroboot serve "python:/var/lib/zeroboot/current/templates/python,node:/var/lib/zeroboot/current/templates/node" 8080
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

- points at `/var/lib/zeroboot/current/templates/...`
- runs `verify-startup` in `ExecStartPre=` before the API process is marked ready
- supports an optional `/etc/zeroboot/zeroboot.env`
- enables strict template hash verification by default
- sets conservative restart and memory limits

## Preflight

Run `scripts/preflight.sh` against the kernel and rootfs artifacts before deployment.
Generate API keys with `scripts/make_api_keys.py --pepper-file /etc/zeroboot/pepper --output api_keys.json`.

If `ZEROBOOT_WORKDIR` points at a template directory, `preflight.sh` also validates `template.manifest.json`.
If `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION` or `ZEROBOOT_ALLOWED_FC_BINARY_SHA256` is set, `preflight.sh` rejects mismatched Firecracker binaries.
If `ZEROBOOT_MIN_FREE_BYTES` or `ZEROBOOT_MIN_FREE_INODES` is set, `preflight.sh` rejects nodes below the watermark.

## Release layout

Deployments use one runtime-facing root only:

- `/var/lib/zeroboot/current/bin/zeroboot`
- `/var/lib/zeroboot/current/templates/python`
- `/var/lib/zeroboot/current/templates/node`

`deploy/deploy.sh` stages a new immutable release under `releases/<id>`, verifies it, flips `current`, and restarts the service. Rollback is one symlink flip plus a restart.

## Production Mode Startup Verification

In prod mode, the server enforces fail-closed startup. It will refuse to start if any of these are missing:

- `ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true`
- `ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=true`  
- `ZEROBOOT_KEYRING_PATH` (must be set and file must exist)
- `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION`
- `ZEROBOOT_ALLOWED_FC_BINARY_SHA256`
- `ZEROBOOT_RELEASE_CHANNEL`
- `ZEROBOOT_API_KEYS_FILE` (file must exist)
- `ZEROBOOT_API_KEY_PEPPER_FILE` (file must exist)
- `logging.log_code=false` (code logging must be disabled)

## Metrics

The `/v1/metrics` endpoint exposes Prometheus metrics including:

- `zeroboot_template_quarantines` - templates quarantined at startup
- `zeroboot_manifest_verification_failures` - manifest validation failures
- `zeroboot_signature_verification_failures` - signature check failures
- `zeroboot_template_version_mismatches` - Firecracker version mismatches
- `zeroboot_restore_failures` - VM restore failures
- `zeroboot_worker_boot_failures` - worker boot failures
- `zeroboot_worker_protocol_failures` - protocol errors

## Remaining gaps

- no warm VM pool yet (experimental)
- the KVM CI lane requires a real Ubuntu 22.04 x86_64 self-hosted runner with `/dev/kvm`
- not proven for hostile public multitenancy
