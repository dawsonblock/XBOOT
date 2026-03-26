<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
    <img alt="Zeroboot" src="assets/logo-light.svg" width="500">
  </picture>
</p>

<p align="center">
  <strong>Snapshot-forked KVM sandboxes with a structured guest runtime protocol</strong>
</p>

## What this repo is now

This branch is no longer presented as a finished public sandbox. It is a hardened prototype with four real pieces:

- snapshot-based KVM restore with CoW memory mapping
- a framed host↔guest execution protocol with length and checksum validation
- a guest supervisor that can front persistent Python and Node workers
- an API layer with production/dev auth modes, hard request limits, trusted-proxy handling, and metadata-only logging by default

## What is still missing

This repo still does **not** include:

- pinned kernel and rootfs artifacts
- a fully reproducible guest image builder
- verified multi-version Firecracker compatibility fixtures
- a proven production scheduler or warm-pool manager
- CI that exercises live Firecracker execution

Treat it as a serious implementation base, not a finished managed service.

## Protocol changes

The old marker-based `ZEROBOOT_DONE` path is no longer the primary contract.

Host → guest now sends a framed request:

```text
ZB1 <request_id_len> <language> <timeout_ms> <code_hex_len> <stdin_hex_len> <checksum>\n
<body>
```

Guest → host returns a framed response:

```text
ZB1R <request_id_len> <exit_code> <error_type> <stdout_hex_len> <stderr_hex_len> <flags> <checksum>\n
<body>
```

The guest supervisor then forwards the request to a persistent worker process using a raw-length framed pipe protocol.

## Honest benchmark labels

There are now three benchmark categories to keep claims separate:

- pure CoW mapping cost
- KVM restore cost
- fork + framed Python request cost

Do not quote framed guest execution numbers as generic language runtime numbers unless the matching guest image actually contains the advertised runtime.

## Build flow

```bash
make build
make guest-python PY_ROOTFS_TEMPLATE=/path/to/base-rootfs-tree
make image-python
make template-python
```

`make guest-python` and `make guest-node` build deterministic staging trees under `build/staging/...`.
`make image-python` and `make image-node` convert those staging trees into ext4 images with `mkfs.ext4 -d`.

## API

See [docs/API.md](docs/API.md).

## Deployment notes

- `ZEROBOOT_AUTH_MODE=prod` now refuses startup without API keys.
- forwarded headers are ignored unless the connecting peer is explicitly trusted.
- request logs default to metadata only and write to `/var/lib/zeroboot/requests.jsonl`.
- health probes now expose `/live` plus cached readiness on `/ready` to avoid executing guest code on every probe.
- `deploy/grafana-dashboard.json` now targets the metrics this repo actually emits and uses a portable Prometheus datasource input instead of a hard-coded cloud UID.
- `/v1/metrics` now includes process RSS plus execution-slot capacity gauges for capacity planning.

## Repo layout

- `src/vmm/` — KVM restore, Firecracker template management, vmstate parsing
- `src/protocol.rs` — framed request/response protocol
- `src/config.rs` — server configuration, auth mode, trusted proxies, limits
- `guest/` — guest supervisor and worker scripts
- `manifests/` — pinned dependency placeholders and artifact manifests
- `scripts/` — rootfs/template build scaffolding

## Status

Use this as an upgradeable sandbox core. Do not market it as a complete public execution service until the missing image pipeline, test matrix, and live integration coverage exist.


## Additional hardening in this archive

- `verify.sh` now checks structured stdout/stderr against the live work directories used by the Makefile.
- `scripts/build_guest_rootfs.sh` now builds a deterministic staging tree and hash manifest from caller-supplied artifacts.
- `scripts/build_rootfs_image.sh` builds ext4 images from those staging trees without requiring a mounted loop device.
- `scripts/preflight.sh` checks `/dev/kvm` and artifact presence before deployment.
- `scripts/make_api_keys.py` generates API key files for prod mode.
- guest workers now request recycle after risky executions instead of persisting indefinitely.
- `deploy/deploy.sh` now assumes `/init`, prod auth, and the new artifact paths.


Additional runtime controls:
- `ZEROBOOT_BIND_ADDR` selects the listen address.
- `ZEROBOOT_QUEUE_WAIT_TIMEOUT_MS` caps how long API requests wait for execution capacity before returning `429`.
- `template.manifest.json` is written during template creation and can be verified with `scripts/validate_template_manifest.py`.
