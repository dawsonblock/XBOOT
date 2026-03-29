<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-light.svg">
    <img alt="Zeroboot" src="assets/logo-light.svg" width="500">
  </picture>
</p>

<p align="center">
  <strong>Controlled-internal snapshot-forked KVM sandboxes with structured guest protocol</strong>
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

XBOOT is a VM sandbox runtime for **controlled internal use** on **Ubuntu 22.04 x86_64 with KVM**, pinned to **Firecracker 1.12.0**. The first hardened release is **offline-only** and combines:

- **Snapshot-based KVM restore** with copy-on-write memory mapping
- **Framed host↔guest protocol** with length-prefixed frames and FNV-1a checksums
- **Per-request guest workers** (Python & Node.js) with supervisor/child subprocess isolation
- **Pinned internal hardening** including hashed API keys, template signing, fail-closed startup, and systemd confinement
- **Versioned deployments** with automatic rollback

Current status: a strong internal sandbox base with real trust controls, but KVM end-to-end proof on the pinned host matrix remains a release gate.

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
- Per-request scratch filesystem area with no persistent on-disk state between requests
- Memory isolation between executions
- Automatic cleanup on timeout or error

### Key Features

| Feature | Description |
|---------|-------------|
| Fast Fork | Snapshot-based VM instantiation via KVM restore + CoW |
| Hardened First Pass | Signed templates, hashed auth, strict verification modes, fail-closed startup |
| Versioned Deployments | Immutable releases with rollback on failure |
| Observability | Prometheus metrics, structured logging, health probes |
| Security | Systemd sandboxing, resource limits, path confinement |

### Pinned Artifact Matrix

The repo-owned first-pass matrix is:

- Firecracker `1.12.0` x86_64 release binary
- guest kernel `vmlinux-5.10.223`
- base Ubuntu 22.04 ext4 rootfs from the official Firecracker CI bucket
- Python `3.10.12` from that Ubuntu base rootfs
- Node.js `20.20.2` installed from the official Node.js Linux x64 tarball

This is explicit because upstream no longer publishes an Ubuntu 22.04 artifact set under the Firecracker `v1.12` CI prefix.

---

## Quick Start

### Deployment Options

XBOOT supports three deployment phases:

| Phase | Method | Use Case | Prerequisites |
|-------|--------|----------|---------------|
| **A** | [Bare Metal/Systemd](./docs/DEPLOYMENT.md) | Production on dedicated hosts | Ubuntu 22.04 + KVM + Firecracker 1.12.0 |
| **B** | [Docker](./docs/DOCKER.md) | Development, testing, portability | Same as Phase A + Docker |
| **C** | [Kubernetes](./docs/KUBERNETES.md) | Fleet deployment, orchestration | Same as Phase A + K8s cluster |

**Important**: Phase A (bare metal) must be stable before containerizing. The Docker and Kubernetes phases are packaging layers, not replacements for KVM isolation.

### Quick Deploy (Bare Metal)

```bash
# 1. Check host readiness
./scripts/check_kvm_host.sh

# 2. Fetch pinned artifacts
bash scripts/fetch_official_artifacts.sh /var/lib/zeroboot/artifacts

# 3. Build and create templates
make build
make guest-python && make image-python && make template-python
make guest-node && make image-node && make template-node

# 4. Assemble release tree
./scripts/build_release_tree.sh

# 5. Verify startup
/var/lib/zeroboot/current/bin/zeroboot verify-startup \
    "python:/var/lib/zeroboot/current/templates/python,node:/var/lib/zeroboot/current/templates/node" \
    --release-root /var/lib/zeroboot/current

# 6. Run smoke tests
./scripts/smoke_exec.sh <api-key> http://127.0.0.1:8080
./scripts/repeat_smoke.sh <api-key> http://127.0.0.1:8080 100

# 7. Install systemd service
sudo cp deploy/zeroboot.service /etc/systemd/system/
sudo systemctl enable --now zeroboot
```

### Docker Quick Start

```bash
# Build and run with Docker Compose
make docker-compose-up

# Run smoke tests
make docker-smoke
```

See [docs/DOCKER.md](./docs/DOCKER.md) for full Docker deployment guide.

### Kubernetes Quick Start

```bash
# Label and taint KVM nodes
kubectl label node <node> sandbox.kvm=true
kubectl taint node <node> sandbox.kvm=true:NoSchedule

# Deploy with Kustomize
kubectl apply -k deploy/k8s/

# Port forward and test
kubectl port-forward -n xboot svc/xboot 8080:8080
./scripts/smoke_exec.sh <api-key> http://127.0.0.1:8080
```

See [docs/KUBERNETES.md](./docs/KUBERNETES.md) for full Kubernetes deployment guide.

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
└── Runtime
    ├── Snapshot restore / fork path
    ├── Health and readiness surfaces
    └── No server-side warm pool in this pass
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
| ZEROBOOT_API_KEYS_FILE | api_keys.json | Path to hashed API key records |
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
# 2. Verifies staged templates through `verify-startup`
# 3. Runs smoke test before switching
# 4. Atomically switches `current`
# 5. Rolls back on health check failure
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
│       └── handlers.rs      # HTTP request handlers
├── guest/
│   ├── init.c               # Guest init and worker launcher
│   ├── worker_supervisor.py  # Python supervisor
│   ├── worker_child.py       # Python child executor
│   ├── worker_supervisor.js  # Node supervisor
│   └── worker_child.js       # Node child executor
├── deploy/
│   ├── deploy.sh            # Versioned deployment script
│   ├── zeroboot.service     # Systemd unit
│   ├── docker/              # Docker packaging (Phase B)
│   │   ├── Dockerfile.runtime
│   │   ├── docker-compose.yml
│   │   ├── docker-entrypoint.sh
│   │   └── .env.example
│   └── k8s/                 # Kubernetes manifests (Phase C)
│       ├── namespace.yaml
│       ├── deployment.yaml
│       ├── daemonset.yaml
│       ├── service.yaml
│       ├── networkpolicy.yaml
│       ├── pvc-release.yaml
│       ├── pvc-state.yaml
│       ├── secret-example.yaml
│       └── kustomization.yaml
├── scripts/
│   ├── build_guest_rootfs.sh
│   ├── build_rootfs_image.sh
│   ├── build_release_tree.sh  # Assemble release directory
│   ├── check_kvm_host.sh      # Host readiness check
│   ├── fetch_official_artifacts.sh
│   ├── make_api_keys.py
│   ├── smoke_exec.sh          # Basic smoke test
│   └── repeat_smoke.sh        # Soak test for drift detection
├── docs/
│   ├── DEPLOYMENT.md          # Phase A: Bare metal
│   ├── DOCKER.md              # Phase B: Docker
│   ├── KUBERNETES.md          # Phase C: Kubernetes
│   ├── API.md
│   ├── ARCHITECTURE.md
│   └── COMPATIBILITY.md
├── .github/
│   └── workflows/
│       └── ci.yml             # CI with KVM smoke tests
└── tests/
```

---

## Testing

### CI Pipeline

- **sanity**: `pytest`, `cargo test --locked`, `cargo fmt --check`, and the current clippy gate the repo can honestly pass now
- **artifact-verify**: Validates only real checked-in template manifests, not lockfiles
- **kvm-integration**: Real KVM on a self-hosted Ubuntu 22.04 runner, using the pinned Ubuntu base rootfs and promoted manifests

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
- [x] Pinned artifact matrix and promoted-template workflow
- [ ] Self-hosted KVM lane as a required release gate
- [ ] Warm pool autoscaling (experimental, no server-side pool yet)

---

## License

Apache License 2.0 - see LICENSE file.
