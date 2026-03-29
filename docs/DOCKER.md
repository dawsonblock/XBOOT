# XBOOT Docker Deployment Guide

Phase B - Docker packaging for the zeroboot runtime.

## Overview

This guide covers deploying XBOOT using Docker. Docker is used as a **packaging wrapper**, not as the primary isolation mechanism. Firecracker remains the isolation boundary.

**Prerequisites:** Phase A (bare metal) must be complete and stable before containerizing.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Host (Ubuntu 22.04)                       │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                 Docker Container                     │   │
│  │  ┌──────────────────────────────────────────────┐    │   │
│  │  │  zeroboot binary + firecracker binary      │    │   │
│  │  │  pinned Firecracker 1.12.0                 │    │   │
│  │  └──────────────────────────────────────────────┘    │   │
│  │                                                       │   │
│  │  ┌──────────────┐  ┌──────────────┐                 │   │
│  │  │  Templates   │  │   Secrets    │  (read-only)   │   │
│  │  │  (mounted)   │  │   (mounted)  │                 │   │
│  │  └──────────────┘  └──────────────┘                 │   │
│  │                                                       │   │
│  │  ┌──────────────┐                                   │   │
│  │  │  State       │  (writable - logs, runtime)      │   │
│  │  └──────────────┘                                   │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                              │
│  /dev/kvm ────────┐                                         │
│  cgroup v2        └──── exposed to container (privileged) │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

### 1. Build Templates (if not already built)

```bash
# Build everything including templates
make build
make guest-python
make image-python
make template-python
make guest-node
make image-node
make template-node
```

### 2. Build Docker Image

```bash
make docker-build
```

Or manually:

```bash
docker build -f deploy/docker/Dockerfile.runtime -t xboot-runtime:latest .
```

### 3. Run with Docker Compose (Recommended)

```bash
# First time setup - creates .env and secrets
make docker-compose-up

# View logs
make docker-compose-logs

# Stop
make docker-compose-down
```

### 4. Run Smoke Tests

```bash
# Wait for service to be ready (~10 seconds)
sleep 10

# Run smoke tests
make docker-smoke
```

## Manual Docker Run

For testing without Docker Compose:

```bash
# Create secrets directory
mkdir -p deploy/docker/secrets
python3 scripts/make_api_keys.py --count 1 --out deploy/docker/secrets/api_keys.json
openssl rand -hex 32 > deploy/docker/secrets/pepper

# Run container
make docker-run
```

Or with full control:

```bash
docker run \
    --device /dev/kvm \
    --privileged \
    --cgroupns=host \
    -p 8080:8080 \
    -v $(pwd)/target/release:/var/lib/zeroboot/current/bin:ro \
    -v $(pwd)/work/python:/var/lib/zeroboot/current/templates/python:ro \
    -v $(pwd)/work/node:/var/lib/zeroboot/current/templates/node:ro \
    -v $(pwd)/deploy/docker/secrets:/etc/zeroboot:ro \
    -v $(pwd)/deploy/docker/state:/var/lib/zeroboot \
    xboot-runtime:latest
```

## Configuration

### Environment Variables

Copy `.env.example` to `.env` and customize:

```bash
cp deploy/docker/.env.example deploy/docker/.env
```

Key settings:

| Variable | Default | Description |
|----------|---------|-------------|
| `XBOOT_PORT` | 8080 | Host port to bind |
| `ZEROBOOT_AUTH_MODE` | prod | Auth mode (prod/dev) |
| `ZEROBOOT_LOG_CODE` | false | Log code in requests |
| `ZEROBOOT_REQUIRE_TEMPLATE_HASHES` | true | Require template SHA256 |
| `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION` | 1.12.0 | Pinned FC version |
| `VERIFY_ON_STARTUP` | true | Run verify-startup on boot |

### Secrets Setup

The container expects secrets in `/etc/zeroboot/`:

```
deploy/docker/secrets/
├── api_keys.json      # API keys (hashed in prod mode)
├── pepper             # Random 32-byte hex secret
└── keyring.json       # (optional) For template signing
```

Generate secrets:

```bash
mkdir -p deploy/docker/secrets
python3 scripts/make_api_keys.py --count 1 --out deploy/docker/secrets/api_keys.json
openssl rand -hex 32 > deploy/docker/secrets/pepper
```

## Volume Mounts

| Container Path | Purpose | Mode |
|---------------|---------|------|
| `/var/lib/zeroboot/current/bin` | zeroboot binary | read-only |
| `/var/lib/zeroboot/current/templates/python` | Python template | read-only |
| `/var/lib/zeroboot/current/templates/node` | Node.js template | read-only |
| `/etc/zeroboot` | Secrets/config | read-only |
| `/var/lib/zeroboot` | Runtime state/logs | read-write |

## Privileges and Security

**Required privileges:**

- `--privileged`: Required for KVM and cgroup management
- `--device /dev/kvm`: KVM device access
- `--cgroupns=host`: cgroup v2 support

**Security notes:**

- Templates are mounted read-only
- Secrets are mounted read-only
- Only `/var/lib/zeroboot` is writable (logs, state)
- Firecracker remains the actual isolation boundary
- Docker is packaging, not isolation

## Health Checks

The container includes a built-in health check:

```dockerfile
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/live || exit 1
```

Check container health:

```bash
docker ps
# Look for (healthy) in STATUS column
```

## Troubleshooting

### Container fails to start

Check logs:

```bash
docker compose -f deploy/docker/docker-compose.yml logs
```

Common issues:

1. **Templates not built**: Run `make template-python template-node`
2. **Secrets missing**: Run the secrets setup commands
3. **KVM not available**: Check `ls -la /dev/kvm` on host
4. **verify-startup fails**: Check that Phase A host path works first

### verify-startup fails in container

This is the main gate. If verify-startup fails:

1. Test host path first: `./scripts/check_kvm_host.sh`
2. Run host verify-startup: `make verify`
3. Only proceed to Docker after host path is stable

### Performance issues

Docker adds minimal overhead. If performance differs from host:

- Check cgroup limits in docker-compose.yml
- Verify /dev/kvm passthrough is working
- Check host resource availability

## Acceptance Criteria

Docker Phase B is complete when:

- [x] `make docker-build` succeeds
- [x] `make docker-compose-up` starts container successfully
- [x] `/live` returns 200 OK
- [x] `/ready` returns 200 OK
- [x] Python exec works repeatedly (100+ iterations)
- [x] Node.js exec works repeatedly (100+ iterations)
- [x] Same release tree works outside Docker (Phase A verification)
- [x] Container restart works cleanly
- [x] No intermittent guest protocol failures

## Next Steps

After Docker packaging is proven:

1. **Do NOT proceed to Kubernetes** until Docker is "boring" (reliable)
2. Run extended soak tests: `./scripts/repeat_smoke.sh <api_key> http://127.0.0.1:8080 500`
3. Verify rollback works by testing with a bad template
4. Document any host-specific tuning needed

## Files Reference

| File | Purpose |
|------|---------|
| `deploy/docker/Dockerfile.runtime` | Runtime container image |
| `deploy/docker/docker-entrypoint.sh` | Container entrypoint script |
| `deploy/docker/docker-compose.yml` | Compose configuration |
| `deploy/docker/.env.example` | Environment template |
| `Makefile` | Docker build/run targets |
