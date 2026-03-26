# 1. OBJECTIVE

Transform XBOOT from a promising prototype into a production-ready private deployment by enforcing these five properties:
1. Only promoted templates can serve traffic in prod
2. Firecracker boot/restore is validated in CI on real KVM
3. Host↔guest I/O is parsed deterministically, not heuristically
4. API keys stop living as plaintext secrets on disk
5. Deployments become versioned releases with rollback

**Core principle: Do not start with warm pools.** Fix trust and correctness first - pooling multiplies risk if the boot path and artifact chain are still loose.

# 2. CONTEXT SUMMARY

**Current faults in the code:**

- `src/main.rs`: `load_snapshot()` validates against template.manifest.json, then ignores it and hardcodes `"{workdir}/snapshot/mem"` and `"{workdir}/snapshot/vmstate"`. This defeats part of the manifest model. `cmd_serve()` quarantines bad templates but still starts in prod with partial activation - wrong for prod.

- `src/template_manifest.rs`: Verifies hashes and protocol version, but does NOT verify: provenance, promotion state, signer identity, Firecracker binary hash, path confinement under the template root. `resolve_path()` accepts absolute paths - too permissive in prod.

- `src/vmm/firecracker.rs`: Three concrete problems:
  - `wait_for_guest_ready()` rebuilds BufReader inside the polling loop (brittle)
  - `api_request()` treats any response containing "200" or "204" as success (naive)
  - `read_stderr_tail()` tries to drain entire stderr at timeout time (can block)

- `src/vmm/vmstate.rs`: Parser uses offset/anchor heuristics - survivable only if version pinning is strict. Right now parser is more flexible than artifact policy, which is backward.

- `src/api/handlers.rs` and `scripts/make_api_keys.py`: Auth is plain-text string equality against raw bearer tokens. If that file leaks, keys are live.

- `.github/workflows/ci.yml`: CI proves almost nothing about real system boundary - does NOT prove Firecracker boot, snapshot creation, restore, or exec.

# 3. APPROACH OVERVIEW

**Execution order (exactly):**
1. src/template_manifest.rs + new src/signing.rs
2. src/main.rs
3. src/vmm/firecracker.rs
4. src/vmm/vmstate.rs
5. .github/workflows/ci.yml
6. src/auth.rs (new)
7. src/config.rs
8. src/api/handlers.rs
9. scripts/make_api_keys.py
10. deploy/deploy.sh
11. deploy/zeroboot.service
12. src/protocol.rs
13. guest/init.c
14. guest/worker.py
15. guest/worker_node.js
16. src/vmm/pool.rs (new)

**P0 - Fix trust and correctness first:**
- Extended manifest with trust fields
- Strict verification mode (reject unsigned prod manifests, path escaping)
- Tighten Firecracker transport (parse HTTP properly, persistent reader)
- Pin snapshot format strictly
- Add KVM smoke test to CI

**P1 - Harden auth, admission, deployment:**
- Replace plaintext API keys with hashed verifier records
- Versioned deployments with rollback
- Enhanced systemd confinement

**P2 - Protocol and guest cleanup:**
- Protocol version in handshake
- setrlimit() in guest init
- Worker recycle logging

**P3 - Pooling (only after P0-P2):**
- VM pooling with health probes

# 4. IMPLEMENTATION STEPS

## Patch 1: Make manifest authoritative (src/template_manifest.rs + src/main.rs)

**Add trust fields to TemplateManifest:**
- schema_version: u32
- template_id: String
- build_id: String
- artifact_set_id: String
- promotion_channel: String (dev | staging | prod)
- signer_key_id: Option<String>
- manifest_signature: Option<String>
- manifest_signed_fields: Option<Vec<String>>
- built_from_git_rev: Option<String>
- build_host: Option<String>
- firecracker_binary_sha256: Option<String>

**Add VerificationMode enum:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    Dev,
    Prod,
}
```

**Add resolve_path_confined():**
```rust
pub fn resolve_path_confined(workdir: &Path, raw: &str) -> Result<PathBuf> {
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workdir.join(raw)
    };

    let canon_workdir = workdir.canonicalize()
        .with_context(|| format!("canonicalize workdir {}", workdir.display()))?;
    let canon_joined = joined.canonicalize()
        .with_context(|| format!("canonicalize template path {}", joined.display()))?;

    if !canon_joined.starts_with(&canon_workdir) {
        bail!(
            "template artifact path escapes workdir: {} not under {}",
            canon_joined.display(),
            canon_workdir.display()
        );
    }

    Ok(canon_joined)
}
```

**Change verify_template_artifacts() signature:**
```rust
pub fn verify_template_artifacts(
    workdir: &Path,
    expected_language: Option<&str>,
    allowed_firecracker_version: Option<&str>,
    allowed_firecracker_binary_sha256: Option<&str>,
    mode: VerificationMode,
) -> Result<TemplateManifest>
```

**In prod mode, reject:**
- Missing schema_version
- Missing promotion_channel or promotion_channel != "prod"
- Missing signature when ZEROBOOT_AUTH_MODE=prod
- Mismatched Firecracker binary hash
- Escaping or absolute paths
- Mismatched protocol version
- Mismatched Firecracker version

**In src/main.rs, fix load_snapshot():**
- Use manifest.snapshot_mem_path and manifest.snapshot_state_path
- Call resolve_path_confined() for each path
- Stop hardcoding "{workdir}/snapshot/mem" and "{workdir}/snapshot/vmstate"

**In cmd_serve(), split into discover/verify/activate:**
- In prod mode, fail hard if any template is quarantined
- Two states only: all activated OR startup failure
- Not "serve with half the languages silently quarantined"

**Create new module: src/signing.rs** for signature verification
**Add new error variants** so failures are machine-readable

---

## Patch 2: Tighten Firecracker transport (src/vmm/firecracker.rs)

**A. Replace looping BufReader in wait_for_guest_ready():**

Add GuestReady struct:
```rust
#[derive(Debug, Clone)]
pub struct GuestReady {
    pub protocol_version: String,
    pub worker_python: bool,
    pub worker_node: bool,
}
```

Parse explicit readiness frame:
```
ZEROBOOT_READY proto=ZB1 worker_python=1 worker_node=1
```

Parse function:
```rust
fn parse_guest_ready_line(line: &str) -> Result<GuestReady> {
    let mut proto = None;
    let mut worker_python = false;
    let mut worker_node = false;

    let mut parts = line.split_ascii_whitespace();
    let prefix = parts.next().unwrap_or_default();
    if prefix != GUEST_READY_PREFIX {
        bail!("not a ready line");
    }

    for part in parts {
        if let Some(v) = part.strip_prefix("proto=") {
            proto = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("worker_python=") {
            worker_python = v == "1";
        } else if let Some(v) = part.strip_prefix("worker_node=") {
            worker_node = v == "1";
        }
    }

    let proto = proto.ok_or_else(|| anyhow::anyhow!("ready line missing proto"))?;
    Ok(GuestReady { protocol_version: proto, worker_python, worker_node })
}
```

Use one persistent reader:
```rust
pub fn wait_for_guest_ready(&mut self, timeout: Duration) -> Result<GuestReady> {
    let start = Instant::now();
    let stdout = self.process.stdout.take().context("Firecracker stdout pipe unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    loop {
        if let Ok(Some(status)) = self.process.try_wait() {
            bail!("Firecracker exited before guest became ready: {}", status);
        }
        if start.elapsed() > timeout {
            bail!("guest readiness handshake timed out after {:?}", timeout);
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(_) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    eprintln!("guest: {}", trimmed);
                }
                if trimmed.starts_with(GUEST_READY_PREFIX) {
                    let ready = parse_guest_ready_line(trimmed)?;
                    if ready.protocol_version != protocol::PROTOCOL_VERSION {
                        bail!(
                            "guest protocol mismatch: expected {}, got {}",
                            protocol::PROTOCOL_VERSION,
                            ready.protocol_version
                        );
                    }
                    return Ok(ready);
                }
            }
            Err(e) => {
                if start.elapsed() > timeout {
                    bail!("guest readiness read failed: {}", e);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
```

**B. Parse HTTP properly in api_request():**

Replace naive `resp.contains("200")` with proper parsing:
```rust
fn parse_http_status(resp: &str) -> Result<u16> {
    let mut lines = resp.lines();
    let status_line = lines.next().ok_or_else(|| anyhow::anyhow!("empty HTTP response"))?;
    let mut parts = status_line.split_ascii_whitespace();
    let http = parts.next().unwrap_or_default();
    let status = parts.next().unwrap_or_default();
    if !http.starts_with("HTTP/") {
        bail!("malformed HTTP status line: {}", status_line);
    }
    let code: u16 = status.parse().with_context(|| format!("invalid HTTP status {}", status))?;
    Ok(code)
}

let code = parse_http_status(&resp)?;
if code != 200 && code != 204 {
    bail!("Firecracker API error on {} {}: {}", method, path, resp);
}
```

**C. Add metadata capture in create_template_snapshot():**
- Capture Firecracker binary hash
- Persist host kernel release
- Persist template creation timestamp
- Persist protocol handshake details observed from guest

**D. Distinguish error modes:**
- socket not ready
- VM exited
- guest never signaled readiness
- Firecracker API returned malformed response
- guest signaled wrong protocol version

---

## Patch 3: Stop pretending vmstate compatibility is dynamic (src/vmm/vmstate.rs)

**Add compatibility gate:**
```rust
pub const SUPPORTED_FIRECRACKER_VERSION: &str = "Firecracker v1.12.0";
```

**Enforce version during manifest verification** - only one pinned format supported.

**Add test scaffolding:**
- tests/vmstate_known_good.rs
- tests/vmstate_rejects_wrong_version.rs
- tests/vmstate_rejects_truncated.rs

**Done when:** Snapshot created with wrong Firecracker build fails before KVM restore is attempted.

---

## Patch 4: CI must prove the real boundary (.github/workflows/ci.yml)

**Keep current sanity job, add kvm-smoke job:**
```yaml
kvm-smoke:
  runs-on: [self-hosted, linux, x64, kvm]
  needs: sanity
  steps:
    - uses: actions/checkout@v4
    - name: Install Rust
      uses: dtolnay/rust-toolchain@stable
    - name: Build zeroboot
      run: cargo build --locked --release
    - name: Verify Firecracker exists
      run: |
        command -v firecracker
        firecracker --version
    - name: Build guest assets
      run: |
        bash scripts/build_guest_rootfs.sh
        bash scripts/build_rootfs_image.sh
    - name: Create Python template
      run: |
        mkdir -p /tmp/zb-python
        ./target/release/zeroboot template guest/vmlinux-fc guest/rootfs-python.ext4 /tmp/zb-python 20 /init 512
    - name: Smoke exec Python
      run: |
        ./target/release/zeroboot test-exec /tmp/zb-python python "print(1+1)"
    - name: Start API
      run: |
        ./target/release/zeroboot serve python:/tmp/zb-python 8080 > /tmp/zeroboot.log 2>&1 &
        echo $! > /tmp/zeroboot.pid
        sleep 3
        curl -fsS http://127.0.0.1:8080/ready
        curl -fsS http://127.0.0.1:8080/health
        curl -fsS -X POST http://127.0.0.1:8080/v1/exec \
          -H 'content-type: application/json' \
          -d '{"language":"python","code":"print(40+2)","timeout_seconds":5}'
    - name: Stop API
      if: always()
      run: |
        kill "$(cat /tmp/zeroboot.pid)" || true
```

**Add artifact-verify job:**
- Validate manifest schema
- Validate hashes
- Validate signatures
- Refuse unsigned prod templates

**Done when:** Pull request cannot merge unless real Firecracker snapshot/restore round-trip passed on KVM hardware.

---

## Patch 5: Secure API key storage (src/auth.rs + src/api/handlers.rs + scripts/make_api_keys.py)

**Create src/auth.rs:**
```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub prefix: String,
    pub hash: String,
    pub created_at: u64,
    pub disabled_at: Option<u64>,
    pub label: Option<String>,
}
```

**Server authentication flow:**
- Extract bearer token
- Split prefix.secret
- Lookup by prefix
- Compute HMAC-SHA256(server_pepper, token)
- Constant-time compare to stored hash
- Reject disabled keys

**Update src/api/handlers.rs:**
- Replace `api_keys: Vec<String>` with `api_keys: Arc<HashMap<String, ApiKeyRecord>>`
- Replace `.contains(&key)` with verifier lookup

**Update scripts/make_api_keys.py:**
- Emit two outputs:
  - Client handoff file (full secrets)
  - Server verifier file (hashed only)
- Example server record:
```json
[
  {
    "id": "key_01",
    "prefix": "zb_live_abcd",
    "hash": "...",
    "created_at": 1770000000000,
    "disabled_at": null,
    "label": "prod-default"
  }
]
```

---

## Patch 6: Config expansion (src/config.rs)

Add config options:
- ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES
- ZEROBOOT_ALLOWED_FC_BINARY_SHA256
- ZEROBOOT_RELEASE_CHANNEL
- ZEROBOOT_POOL_MIN_PER_LANG
- ZEROBOOT_POOL_MAX_PER_LANG
- ZEROBOOT_API_KEY_PEPPER_FILE

---

## Patch 7: Versioned deployments (deploy/deploy.sh)

**Target model:**
- Create immutable release dir: /var/lib/zeroboot/releases/<release_id>/
- Upload binary and artifacts there
- Verify manifests/signatures there
- Generate templates there
- Run smoke exec there
- Switch symlink: current -> releases/<release_id>
- Restart service
- Verify health
- Rollback symlink on failure

**Done when:** Failed deploy does not destroy last working release.

---

## Patch 8: Tighten systemd confinement (deploy/zeroboot.service)

Add or tighten:
- DeviceAllow=/dev/kvm rw
- ProtectKernelTunables=true
- ProtectKernelModules=true
- ProtectControlGroups=true
- RestrictNamespaces=true
- RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
- LockPersonality=true
- SystemCallArchitectures=native
- NoNewPrivileges=true
- PrivateTmp=true

Be careful with anything that breaks Firecracker or KVM access.

---

## Patch 9: Protocol and guest improvements

**src/protocol.rs + guest/init.c:**
- Explicit max header sizes
- Explicit protocol version in every request/response header
- Guest ready handshake includes protocol version
- Reject mismatched protocol before execution
- Test truncated frames, checksum errors, length overflows, bad UTF-8 ids

**guest/init.c:**
- Add setrlimit() for RLIMIT_NOFILE, RLIMIT_NPROC, RLIMIT_FSIZE, RLIMIT_CORE
- Clean worker restart backoff
- Better cleanup on partial worker read failure
- Explicit worker health line before snapshot readiness

Target output:
```c
printf("ZEROBOOT_READY proto=%s worker_python=%d worker_node=%d\n", "ZB1", py_ready, node_ready);
fflush(stdout);
```

**guest/worker.py:**
- Worker recycle reason logging
- Clearer protocol failure vs runtime failure separation
- Hard disable dangerous inherited env vars

**guest/worker_node.js:**
- Parity with Python recycle reasons
- Explicit reset of stateful globals between runs

---

## Patch 10: VM Pooling (src/vmm/pool.rs) - Only after P0-P2

**Pool design per language:**
- min_idle, max_idle
- Background refill
- Borrow timeout
- Health probe before return to pool
- Discard on protocol mismatch, timeout, recycle request, transport error

**Important:** Pool ready-to-serve restored VMs, not templates.

**Metrics:**
- pool depth
- lease latency
- warm hits / cold fallbacks
- refill failures
- discarded warmed VMs

---

# 5. TESTING AND VALIDATION

## P0 Validation (Trust and Correctness)

**Done when:**
- Copied snapshot from random directory cannot be loaded in prod unless signed and marked promoted
- Prod startup has two modes only: all templates activated OR startup failure
- Can distinguish socket not ready / VM exited / guest never ready / malformed response / protocol mismatch
- CI fails unless real Firecracker snapshot/restore round-trip passed on KVM hardware

**Test Scenarios:**
1. Attempt to load unsigned manifest in prod → must reject
2. Attempt to load manifest with promotion_channel=dev in prod → must reject
3. Attempt to load manifest with escaping paths → must reject
4. Verify manifest with wrong Firecracker binary hash → must reject
5. Firecracker API returns malformed response → proper parse error
6. Guest signals wrong protocol version → proper rejection

## P1 Validation (Auth and Deployment)

**Done when:**
- Stolen api_keys.json cannot be used to authenticate (contains no reusable tokens)
- Failed deploy does not destroy last working release
- Rollback restores previous version correctly

## P2 Validation (Protocol and Guest)

**Done when:**
- Truncated frames rejected
- Checksum errors detected
- Protocol mismatch caught before execution
- Guest init applies setrlimit() restrictions
- Worker recycle reasons logged correctly

## P3 Validation (Pooling)

**Done when:**
- Pool maintains min_idle VMs
- Health probe runs before return to pool
- Protocol mismatch triggers VM discard
- Metrics accurately reflect pool state

---

## Execution Order (exactly as specified)

1. src/template_manifest.rs + new src/signing.rs
2. src/main.rs
3. src/vmm/firecracker.rs
4. src/vmm/vmstate.rs
5. .github/workflows/ci.yml
6. src/auth.rs (new)
7. src/config.rs
8. src/api/handlers.rs
9. scripts/make_api_keys.py
10. deploy/deploy.sh
11. deploy/zeroboot.service
12. src/protocol.rs
13. guest/init.c
14. guest/worker.py
15. guest/worker_node.js
16. src/vmm/pool.rs (new)

---

## Final Milestone

The next honest milestone is:

**signed private deployment with live KVM CI, deterministic startup validation, hashed auth records, and versioned rollback-safe releases.**

Once that exists, pooling becomes worth doing.

---

# 6. MISSING INFRASTRUCTURE - ADD TO PLAN

The repo still does not include these critical infrastructure components:

## 6.1 Pinned Kernel and Rootfs Artifacts

**Problem:** No canonical source for kernel and rootfs - templates may use arbitrary versions.

**Solution:**

A. **Create artifact manifest for host-side binaries:**
```
artifacts/
  kernels/
    vmlinux-fc-5.10.0-amd-virt # AWS Linux kernel for Firecracker
      sha256: <hash>
      version: 5.10.0
      arch: x86_64
      built_from: https://github.com/awslabs/linux-aws/commit/xxx
  rootfs/
    rootfs-python.ext4
      sha256: <hash>
      size: <size>
      python_version: 3.x.x
      worker_version: <commit>
```

B. **Add config for pinned versions:**
```rust
pub struct PinnedArtifacts {
    pub kernel_version: String,
    pub kernel_sha256: String,
    pub rootfs_python_sha256: String,
    pub rootfs_node_sha256: String,
}
```

C. **Verify artifacts against pinned hashes at template build time**

---

## 6.2 Fully Reproducible Guest Image Builder

**Problem:** Guest images built ad-hoc - not reproducible, not pinned.

**Solution:**

A. **Create `scripts/build_guest_image.sh`** with pinned inputs:
```bash
#!/bin/bash
set -euo pipefail

PYTHON_VERSION="3.11.5"
PYTHON_BUILD_DATE="20231113"
NODE_VERSION="20.10.0"

# Use deterministic build with pinned dates
docker run --rm \
  -e PYTHON_BUILD_DATE="$PYTHON_BUILD_DATE" \
  -e TZ=UTC \
  python:${PYTHON_VERSION}-bookworm \
  /bin/sh -c 'python get-pip.py...'

# Or: Use pre-built base with verified hash
```

B. **Define guest image specification:**
```
guest/
  python/
    dockerfile: guest/python/Dockerfile
    base_image: python:3.11.5-slim
    packages:
      - pip==23.3.2
      - wheel==0.42.0
    worker_commit: <git-sha>
    built_at: <timestamp>
```

C. **Store build provenance in manifest:**
```rust
pub struct GuestImageBuild {
    pub base_image: String,
    pub base_image_sha256: String,
    pub packages: Vec<String>,
    pub worker_git_rev: String,
    pub build_timestamp_unix: u64,
    pub dockerfile_hash: String,
}
```

D. **Build scripts:**
- `scripts/build_guest_rootfs.sh` - builds rootfs image
- `scripts/build_rootfs_image.sh` - creates ext4 from rootfs
- `scripts/verify_guest_image.sh` - verifies reproducibility

---

## 6.3 Verified Multi-Version Firecracker Compatibility Fixtures

**Problem:** No systematic testing of Firecracker version compatibility.

**Solution:**

A. **Define supported Firecracker versions:**
```yaml
# .firecracker-versions.yaml
supported:
  - version: "1.10.0"
    status: tested
    fixtures: tests/fixtures/fc-1.10.0/
  - version: "1.11.0"
    status: tested
    fixtures: tests/fixtures/fc-1.11.0/
  - version: "1.12.0"
    status: current
    fixtures: tests/fixtures/fc-1.12.0/

minimum: "1.10.0"
```

B. **Create fixture corpus:**
```
tests/fixtures/
  fc-1.10.0/
    snapshot_valid/
      mem
      vmstate
      manifest.json
    snapshot_truncated/
      mem (10KB)
    snapshot_wrong_version/
      vmstate (from 1.9.0)
  fc-1.11.0/
    ...
  fc-1.12.0/
    ...
```

C. **Add versioned test suite:**
```rust
// tests/vmstate_fc_compat.rs
#[test]
fn test_fc_1_10_0_snapshot_parse() {
    let data = include_bytes!("../fixtures/fc-1.10.0/snapshot_valid/vmstate");
    let parsed = parse_vmstate(data).expect("should parse fc 1.10.0 snapshot");
    assert_eq!(parsed.fc_version, "1.10.0");
}

#[test]
fn test_fc_1_12_0_wrong_version_rejected() {
    let data = include_bytes!("../fixtures/fc-1.12.0/snapshot_wrong_version/vmstate");
    let result = parse_vmstate(data);
    assert!(result.is_err());
}
```

D. **CI matrix for version compatibility:**
```yaml
matrix:
  fc_version: ['1.10.0', '1.11.0', '1.12.0']
```

---

## 6.4 Expanded CI with Live Firecracker Execution

**Current gap:** kvm-smoke job exists but needs expansion.

**Solution:**

A. **Comprehensive kvm-smoke job:**
```yaml
kvm-smoke:
  runs-on: [self-hosted, linux, x64, kvm]
  needs: sanity
  steps:
    - uses: actions/checkout@v4

    - name: Download pinned artifacts
      run: |
        # Download pinned kernel/rootfs
        curl -fsSL "$ARTIFACT_URL/vmlinux-fc-5.10.0-amd-virt" -o guest/vmlinux-fc
        curl -fsSL "$ARTIFACT_URL/rootfs-python.ext4" -o guest/rootfs-python.ext4

    - name: Verify artifact hashes
      run: |
        sha256sum -c artifacts/kernels/vmlinux-fc-5.10.0-amd-virt.sha256
        sha256sum -c artifacts/rootfs/rootfs-python.ext4.sha256

    - name: Build zeroboot
      run: cargo build --locked --release

    - name: Build guest images (reproducible)
      run: |
        bash scripts/build_guest_rootfs.sh
        bash scripts/build_rootfs_image.sh
        # Verify reproducibility
        bash scripts/verify_guest_image.sh python

    - name: Create templates (Python + Node)
      run: |
        ./target/release/zeroboot template \
          guest/vmlinux-fc guest/rootfs-python.ext4 \
          /tmp/zb-python 20 /init 512
        ./target/release/zeroboot template \
          guest/vmlinux-fc guest/rootfs-node.ext4 \
          /tmp/zb-node 20 /init 512

    - name: Test exec Python
      run: |
        ./target/release/zeroboot test-exec /tmp/zb-python python "print(1+1)"
        ./target/release/zeroboot test-exec /tmp/zb-python python "import sys; print(sys.version)"

    - name: Test exec Node
      run: |
        ./target/release/zeroboot test-exec /tmp/zb-node node "-e 'console.log(1+1)'"

    - name: Test snapshot-create and restore
      run: |
        # Create new template from running VM
        ./target/release/zeroboot snapshot-create /tmp/zb-python /tmp/zb-python-snap
        # Restore from snapshot
        ./target/release/zeroboot test-exec /tmp/zb-python-snap python "print(2+2)"

    - name: Start API service
      run: |
        ./target/release/zeroboot serve python:/tmp/zb-python node:/tmp/zb-node 8080 &
        sleep 5
        echo $! > /tmp/zeroboot.pid

    - name: Test API endpoints
      run: |
        # Health checks
        curl -fsS http://127.0.0.1:8080/ready
        curl -fsS http://127.0.0.1:8080/health

        # Exec endpoints
        curl -fsS -X POST http://127.0.0.1:8080/v1/exec \
          -H 'content-type: application/json' \
          -H 'Authorization: Bearer test-key' \
          -d '{"language":"python","code":"print(40+2)","timeout_seconds":5}'

    - name: Cleanup
      if: always()
      run: kill $(cat /tmp/zeroboot.pid) || true
```

B. **Artifact-verify job (expanded):**
```yaml
artifact-verify:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Validate kernel/rootfs manifests
      run: |
        # Validate YAML schemas
        # Verify pinned hashes exist
        # Check build provenance

    - name: Validate Firecracker fixtures
      run: |
        # Check all version fixtures exist
        # Validate fixture metadata

    - name: Validate guest image specs
      run: |
        # Check dockerfile hashes
        # Verify reproducible flag
```

---

## 6.5 Production Scheduler / Warm-Pool Manager

**Problem:** No production-ready scheduler for VM lifecycle management.

**Solution:**

A. **Scheduler design:**
```rust
pub struct VmScheduler {
    // Per-language pools
    pools: HashMap<String, VmPool>,
    // Global config
    config: SchedulerConfig,
}

pub struct SchedulerConfig {
    pub min_idle_per_language: usize,
    pub max_idle_per_language: usize,
    pub borrow_timeout_seconds: u64,
    pub health_check_interval_seconds: u64,
    pub refill_batch_size: usize,
}

impl VmScheduler {
    /// Acquire a VM for the given language, possibly waiting
    pub fn borrow(&self, language: &str) -> Result< BorrowedVm>;

    /// Return a VM to the pool (after health check)
    pub fn repay(&self, vm: BorrowedVm, health_check: VmHealth);

    /// Manually request pool refill
    pub fn trigger_refill(&self, language: &str);
}
```

B. **Pool implementation:**
```rust
pub struct VmPool {
    language: String,
    config: PoolConfig,

    // Ready VMs available to borrow
    ready: Vec<PooledVm>,

    // VMs being prepared (boot + restore)
    pending: Vec<PendingVm>,

    // Metrics
    metrics: PoolMetrics,
}

#[derive(Clone)]
pub struct PooledVm {
    vm_id: String,
    snapshot_path: PathBuf,
    created_at: Instant,
    last_health_check: Instant,
    health_check_result: Option<VmHealth>,
}

pub enum VmHealth {
    Healthy,
    Unhealthy(String), // reason
    Unknown,
}
```

C. **Health check before return to pool:**
```rust
fn health_check(vm: &mut PooledVm) -> VmHealth {
    // Quick exec to verify VM is responsive
    let result = vm.exec_quick("echo alive");

    match result {
        Ok(output) if output.contains("alive") => VmHealth::Healthy,
        Ok(_) => VmHealth::Unhealthy("unexpected output".to_string()),
        Err(e) => VmHealth::Unhealthy(e.to_string()),
    }
}
```

D. **Metrics for observability:**
```rust
pub struct PoolMetrics {
    pub current_idle: usize,
    pub total_borrows: u64,
    pub warm_hits: u64,
    pub cold_forks: u64,
    pub health_check_failures: u64,
    pub refill_failures: u64,
    pub discard_on_return: u64,
    pub borrow_latency_seconds_histogram: Histogram,
}
```

E. **Integration with API:**
```rust
// In api/handlers.rs
async fn handle_exec(
    State(state): State<AppState>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>> {
    let language = &req.language;

    // Borrow from pool (waits if none available, with timeout)
    let vm = state.scheduler
        .borrow_with_timeout(language, req.timeout_seconds)
        .await
        .map_err(|e| Error::NoVmAvailable(e.to_string()))?;

    // Execute
    let result = vm.execute(&req.code, req.timeout_seconds).await;

    // Return to pool (after health check)
    let health = vm.health_check().await;
    state.scheduler.repay(vm, health);

    Ok(Json(ExecResponse::from(result)))
}
```

---

## Execution Order - Updated

1. src/template_manifest.rs + new src/signing.rs
2. src/main.rs
3. src/vmm/firecracker.rs
4. src/vmm/vmstate.rs
5. .github/workflows/ci.yml
6. **NEW: Add pinned kernel/rootfs artifact manifests**
7. **NEW: Create reproducible guest image builder**
8. **NEW: Add Firecracker version fixtures and tests**
9. Expand kvm-smoke CI job
10. src/auth.rs (new)
11. src/config.rs
12. src/api/handlers.rs
13. scripts/make_api_keys.py
14. deploy/deploy.sh
15. deploy/zeroboot.service
16. src/protocol.rs
17. guest/init.c
18. guest/worker.py
19. guest/worker_node.js
20. **NEW: Production scheduler (warm-pool manager)**

---

## Final Milestone - Updated

The complete production-readiness milestone now includes:

1. **Signed artifacts** - only promoted templates serve traffic
2. **Pinned kernel/rootfs** - canonical artifact sources with verified hashes
3. **Reproducible guest images** - build provenance tracked, images verifiable
4. **Firecracker version fixtures** - multi-version compatibility tested
5. **Live KVM CI** - every PR proves Firecracker boot/exec/snapshot works
6. **Hashed auth records** - API keys not stored as plaintext
7. **Versioned deployments** - rollback-safe releases
8. **Production scheduler** - warm-pool manager with health checks and metrics

This is a serious implementation base, not a finished managed service.
