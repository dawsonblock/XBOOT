<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
    <img alt="Zeroboot" src="assets/logo-light.svg" width="500">
  </picture>
</p>

<p align="center">
  <strong>Production-hardened snapshot-forked KVM sandboxes with structured guest protocol</strong>
</p>

<p align="center">
  <a href="https://github.com/dawsonblock/XBOOT/actions/workflows/ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/dawsonblock/XBOOT/ci.yml?branch=main&label=CI" alt="CI Status">
  </a>
  <a href="https://github.com/dawsonblock/XBOOT/releases">
    <img src="https://img.shields.io/github/v/release/dawsonblock/XBOOT?include_prereleases&label=Version" alt="Version">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/github/license/dawsonblock/XBOOT?label=License" alt="License">
  </a>
</p>

---

## What is XBOOT?

XBOOT is a **production-ready** VM sandbox system that provides sub-millisecond code execution by combining:

- **Snapshot-based KVM restore** with copy-on-write memory mapping
- **Framed host↔guest protocol** with length-prefixed frames and FNV-1a checksums
- **Per-request guest workers** (Python & Node.js) with subprocess isolation
- **Production-grade security** including hashed API keys, template signing, and systemd confinement
- **Versioned deployments** with automatic rollback

### Production Mode Requirements

In **Prod mode**, the server enforces strict security requirements:

| Requirement | Description |
|-------------|-------------|
| Template Hashes | All artifact files must have SHA256 hashes in manifest |
| Template Signatures | Manifest must be signed by a trusted key |
| Release Channel | Template must be promoted to configured channel (default: "prod") |
| Schema Version | Template must declare schema_version (only v1 supported) |
| Firecracker Version | Template must specify version (when pinned in config) |
| Path Confinement | All artifact paths must stay within workdir |
| API Key Pepper | Pepper secret must exist |
| No Code Logging | `ZEROBOOT_LOG_CODE` must be false |

**Startup Fail-Closed**: In prod mode, the server will refuse to start if:
- `ZEROBOOT_REQUIRE_TEMPLATE_HASHES` is not set to true
- `ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES` is not set to true
- `ZEROBOOT_KEYRING_PATH` is not set or file doesn't exist
- `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION` is not set
- `ZEROBOOT_ALLOWED_FC_BINARY_SHA256` is not set
- `ZEROBOOT_RELEASE_CHANNEL` is not set
- `ZEROBOOT_API_KEYS_FILE` doesn't exist
- `ZEROBOOT_API_KEY_PEPPER_FILE` doesn't exist
- `logging.log_code` is true

### Guest Isolation Model

The guest execution model provides strong isolation between requests:

1. **Supervisor Process**: A long-lived process that manages request queuing
2. **Child Executor**: For each request, a fresh child process is spawned to execute code
3. **Process Exit**: The child process exits after each request, ensuring no state bleeds

This subprocess-based model ensures:
- No persistent Python/Node.js state between requests
- Fresh filesystem namespace per request
- Memory isolation between executions
- Automatic cleanup on timeout or error

### Key Features

| Feature | Description |
|---------|-------------|
| Fast Fork | Sub-millisecond VM instantiation via KVM snapshot restore + CoW |
| Production Hardened | Signed templates, hashed auth, strict verification modes |
| Versioned Deployments | Immutable releases with rollback on failure |
| Observability | Prometheus metrics, structured logging, health probes |
| Security | Systemd sandboxing, resource limits, path confinement |

---

## Quick Start

> **Important:** This repo is the **runtime** for XBOOT. It does **not** include guest artifacts (kernel, rootfs) by default. You need to provide these or build them.

### Option A: Code-Only Testing

For contributors without KVM access or guest artifacts:

```bash
# Run unit tests
cargo test --locked
python -m unittest discover -s tests -v

# Run lints
cargo clippy
cargo fmt --check
```

### Option B: Full System Bring-Up

Prerequisites:
- **KVM-capable host** with root access (for `/dev/kvm`)
- **Firecracker binary** (download from releases, set `$ZEROBOOT_FIRECRACKER_PATH`)
- **Linux kernel** (vmlinux for Firecracker, e.g., `vmlinux-fc-5.10.0-amd-virt`)
- **Guest rootfs** (ext4 image with Python/Node runtime)

```bash
# Build the server
make build

# Build guest images (requires debootstrap/Docker)
make guest-python
make image-python
make template-python

# Run locally
./target/release/zeroboot serve python:/path/to/template 8080

# Or with test execution
./target/release/zeroboot test-exec /path/to/template python "print(1+1)"
```

### Required Environment Variables

For production deployments, set these:

| Variable | Description | Default |
|----------|-------------|---------|
| `ZEROBOOT_AUTH_MODE` | `dev` or `prod` | `dev` |
| `ZEROBOOT_API_KEY_PEPPER_FILE` | Path to pepper secret | `/etc/zeroboot/pepper` |
| `ZEROBOOT_REQUIRE_TEMPLATE_HASHES` | Enforce template hash verification | `false` in dev |
| `ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES` | Enforce template signatures | `false` in dev |
| `ZEROBOOT_KEYRING_PATH` | Path to signing keyring | none |

---

## Production Architecture

```
zeroboot serve
├── Auth Layer
│   ├── HMAC-SHA256 keys
│   ├── Pepper secret
│   └── Rate limiting
├── Verification Mode
│   ├── Dev (lenient)
│   └── Prod (strict)
├── Template Manager
│   ├── Manifest verification (schema, signatures, hashes)
│   ├── Path confinement (no escaping workdir)
│   └── Promotion channels (dev → staging → prod)
└── VM Pool (optional)
    └── Pre-warmed VMs with health checks
```

### Trust Model

Templates must be explicitly **promoted** to production:

```json
{
  "schema_version": 1,
  "template_id": "...",
  "promotion_channel": "prod",
  "manifest_signature": "...",
  "signer_key_id": "..."
}
```

In **Prod mode**, the server enforces:
- schema_version must be present
- promotion_channel must be "prod"
- manifest_signature required (when configured)
- firecracker_binary_sha256 validation
- Path confinement (no escaping workdir)
- Protocol version matching

---

## Protocol

### Request (Host → Guest)

```
ZB1 <request_id_len> <language> <timeout_ms> <code_hex_len> <stdin_hex_len> <checksum>
<body>
```

### Response (Guest → Host)

```
ZB1R <request_id_len> <exit_code> <error_type> <stdout_hex_len> <stderr_hex_len> <flags> <checksum>
<body>
```

### Guest Ready Handshake

The guest signals readiness with protocol version:

```
ZEROBOOT_READY proto=ZB1 worker_python=1 worker_node=1
```

---

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| ZEROBOOT_AUTH_MODE | dev | Authentication mode (dev or prod) |
| ZEROBOOT_API_KEYS_FILE | api_keys.json | Path to API key records |
| ZEROBOOT_API_KEY_PEPPER_FILE | /etc/zeroboot/pepper | HMAC pepper secret |
| ZEROBOOT_REQUIRE_TEMPLATE_HASHES | false | Enforce artifact hashes |
| ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES | false | Enforce manifest signatures |
| ZEROBOOT_ALLOWED_FIRECRACKER_VERSION | - | Lock Firecracker version |
| ZEROBOOT_ALLOWED_FC_BINARY_SHA256 | - | Lock Firecracker binary hash |
| ZEROBOOT_RELEASE_CHANNEL | - | Require specific release channel |
| ZEROBOOT_BIND_ADDR | 127.0.0.1 | Listen address |
| ZEROBOOT_PORT | 8080 | Listen port |
| ZEROBOOT_TRUSTED_PROXIES | - | Comma-separated IPs for forwarded headers |
| ZEROBOOT_LOG_CODE | false | Include code in request logs |
| ZEROBOOT_POOL_MIN_PER_LANG | 0 | Minimum idle VMs per language |
| ZEROBOOT_POOL_MAX_PER_LANG | 4 | Maximum idle VMs per language |

---

## Deployment

### Production Deployment with Rollback

```bash
# Deploy with versioned releases
SERVERS="prod1 prod2" ./deploy/deploy.sh

# The script:
# 1. Creates immutable release directory
# 2. Runs smoke test before switching
# 3. Atomically switches symlink
# 4. Rolls back on health check failure
```

### Systemd Service

The `deploy/zeroboot.service` includes security hardening:

```ini
[Service]
DeviceAllow=/dev/kvm rw
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictNamespaces=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
NoNewPrivileges=true
```

---

## Project Structure

```
XBOOT/
├── src/
│   ├── main.rs              # CLI and server entry
│   ├── config.rs            # Configuration parsing
│   ├── protocol.rs          # Frame encoding/decoding
│   ├── template_manifest.rs # Template verification
│   ├── auth.rs              # HMAC-SHA256 API key verification
│   ├── signing.rs           # Manifest signature verification
│   ├── vmm/
│   │   ├── firecracker.rs   # Firecracker VM management
│   │   ├── kvm.rs           # KVM snapshot restore
│   │   └── vmstate.rs       # VM state parsing
│   └── api/
│       └── handlers.rs     # HTTP request handlers
├── guest/
│   ├── init.c               # Guest supervisor (with setrlimit)
│   ├── worker.py            # Python worker
│   └── worker_node.js       # Node.js worker
├── deploy/
│   ├── deploy.sh            # Versioned deployment script
│   └── zeroboot.service     # Systemd unit
├── scripts/
│   ├── build_guest_rootfs.sh
│   ├── build_rootfs_image.sh
│   └── make_api_keys.py
├── .github/
│   └── workflows/
│       └── ci.yml           # CI with KVM smoke tests
└── tests/
```

---

## Testing

### CI Pipeline

- **sanity**: Syntax, unit tests, cargo tests
- **artifact-verify**: Manifest schema validation
- **kvm-smoke**: Real KVM on self-hosted runner

### Manual Testing

```bash
# Template creation
./target/release/zeroboot template guest/vmlinux-fc guest/rootfs-python.ext4 /tmp/template 20 /init 512

# Test execution
./target/release/zeroboot test-exec /tmp/template python "print(1+1)"

# Server with health checks
./target/release/zeroboot serve python:/tmp/template 8080
curl http://127.0.0.1:8080/ready
curl http://127.0.0.1:8080/health
```

---

## Metrics

The `/v1/metrics` endpoint exposes:

- zeroboot_requests_total - Total requests by language
- zeroboot_request_duration_seconds - Request latency histogram
- zeroboot_template_quarantines - Quarantined templates count
- zeroboot_pool_depth - Current pool size per language
- Process RSS and execution slot capacity

---

## Security

### API Key Security

API keys use HMAC-SHA256 hashing with server-side pepper:

- Client receives: "prefix.secret"
- Server stores: HMAC(pepper, "prefix:secret")

If the key file leaks, attackers cannot use the keys without the pepper.

### Guest Resource Limits

The guest init applies setrlimit():

- RLIMIT_NOFILE: 256 files
- RLIMIT_NPROC: 32 processes
- RLIMIT_FSIZE: 8MB
- RLIMIT_CORE: 0 (core dumps disabled)

---

## Roadmap

- [x] Full signature verification with trusted keyring (Ed25519 via ring crate)
- [x] Reproducible guest image builder (scripts/build_reproducible_image.py)
- [x] Multi-version Firecracker compatibility matrix (scripts/firecracker_compat.py)
- [~] Warm pool autoscaling (scripts/warm_pool_scaler.py) - experimental, no server-side pool yet

---

## License

Apache License 2.0 - see LICENSE file.