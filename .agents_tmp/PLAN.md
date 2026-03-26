# XBOOT Production-Readiness Repair Plan

## Executive Summary

This plan addresses the concrete contradictions blocking XBOOT from being a trustworthy production system. The repo has a solid core architecture but suffers from implementation inconsistencies that break critical paths.

**Key Principle:** Fix the lies first, then add features.

## Target State

When this plan is done, XBOOT should be able to claim, truthfully:

- clean Rust build and test pass
- one auth path, not two
- real signature verification, not a TODO
- self-consistent startup and deployment flow
- explicit separation between:
  - core sandbox runtime
  - artifact build pipeline
  - deployment tooling
  - future warm-pool work

---

# Priority Lanes

- **P0** — make the repo internally true (mandatory before anything else)
- **P1** — make production claims honest (security and deployment become real)
- **P2** — add missing operator value (only after P0 and P1 are done)

---

# P0 — Make the Repo Internally True

## P0.1 Fix Source-Level Rust Errors

**Scope:** Remove obvious compile blockers and API inconsistencies.

**Files:**
- `src/template_manifest.rs`
- `src/signing.rs`
- `src/main.rs`
- `src/api/handlers.rs`
- `Cargo.toml`

**Work:**

1. Fix the bad variable reference in manifest verification:
   - **Current bug:** `mem_meta.display()` used inside the metadata call that defines `mem_meta`
   - **Fix:** Change to `mem_path.display()`

2. Fix `src/signing.rs` so it actually compiles:
   - Missing `anyhow::Context` import
   - Wrong return type in `generate_key_pair()`
   - Possible dead or mismatched functions
   - Ensure base64 and ring usage are coherent

3. Run and fix:
   ```bash
   cargo check
   cargo test --locked
   ```

4. Remove dead config or wire it properly. Do not leave "future" fields looking live.

**Done when:**
- `cargo check` passes
- `cargo test --locked` passes
- No compile-time warnings for obviously broken dead code in security-critical modules

---

## P0.2 Unify Auth Into One Real Path

**Scope:** Remove the split-brain auth design.

**Files:**
- `src/auth.rs`
- `src/main.rs`
- `src/api/handlers.rs`
- `scripts/make_api_keys.py`
- `README.md`
- `docs/DEPLOYMENT.md`

**Work:**

1. Replace `Vec<String>` plaintext API keys with a verifier object.

   **Current bad path:**
   - `main.rs` loads raw strings
   - `handlers.rs` uses `contains(&key)`

   **Target path:**
   - `main.rs` loads:
     - Hashed key records from JSON
     - Pepper from `ZEROBOOT_API_KEY_PEPPER_FILE`
   - App state stores: `Option<ApiKeyVerifier>`
   - Request handler calls verifier

2. Change auth behavior:

   **Dev mode:**
   - If no keys configured, allow anonymous

   **Prod mode:**
   - Require verifier to load successfully
   - Reject startup if pepper missing
   - Reject startup if no active key records

3. Make constant-time comparison real:
   - Right now it computes a hash and compares strings normally
   - Use a constant-time byte comparison

4. Replace `scripts/make_api_keys.py`:
   - Generate one printable token for the operator
   - Generate one stored hashed record JSON for the server
   - Do not write raw bearer tokens into the server-side JSON

**Done when:**
- Plaintext API key list is gone from runtime
- One bearer verifier path exists
- Prod mode fails hard on missing pepper or invalid key store
- Docs no longer claim hashed auth while running plaintext auth

---

## P0.3 Make Signature Enforcement Real or Remove the Claim

**Scope:** Stop pretending signatures are enforced when they are not.

**Files:**
- `src/signing.rs`
- `src/template_manifest.rs`
- `src/config.rs`
- `README.md`
- `docs/ARCHITECTURE.md`
- `docs/DEPLOYMENT.md`

**Work:**

**Two acceptable choices - pick one and be strict:**

**Option A — Implement real signing now:**
- Implement key loading by `signer_key_id`
- Implement manifest canonicalization
- Implement detached signature verification
- Failure on invalid signature in prod when `require_template_signatures=true`
- Wire `verify_template_artifacts()` to call it

**Option B — Remove the production claim:**
- If you do not finish signing in this pass:
  - Remove "signed templates" from README feature table
  - Remove signature enforcement claims from architecture docs
  - Leave the manifest fields as optional metadata only

**Recommended choice:** Option A if this repo is supposed to be deployable. Otherwise the trust chain stays fake.

**Done when:**
- Either: prod signature enforcement works end to end
- Or: no documentation claims signed template enforcement

---

## P0.4 Make Startup/Config Behavior Consistent

**Scope:** Config must drive behavior. Right now some fields exist without strong effect.

**Files:**
- `src/config.rs`
- `src/main.rs`
- `src/template_manifest.rs`

**Work:**

1. Wire `require_template_signatures` from config directly into verification logic:
   - Do not use ad hoc `std::env::var(...).is_ok()` checks inside verifier code

2. Decide what `release_channel` means:
   - Right now the code checks `promotion_channel == "prod"` in prod mode, but config also has `release_channel`
   - Pick one model:
     - Model A: config requires specific channel match
     - Model B: prod mode hardcodes prod
   - Do not keep both half-alive

3. Fail startup on invalid config combinations:
   - Examples:
     - prod mode + no key verifier
     - prod mode + signatures required + no verifier keys
     - allowed Firecracker hash configured but missing in manifest

**Done when:**
- Config fields each have an observable effect or are removed
- No security behavior depends on scattered env reads outside config load

---

# P1 — Make Deployment and Artifact Flow Honest

## P1.1 Separate Code Repo from External Artifact Pipeline

**Scope:** The repo is not self-contained. That is acceptable. Lying about it is not.

**Files:**
- `README.md`
- `docs/DEPLOYMENT.md`
- `docs/ARCHITECTURE.md`
- `Makefile`
- `scripts/build_guest_rootfs.sh`
- `scripts/build_rootfs_image.sh`
- `manifests/*.manifest`
- `manifests/*.lock.json`

**Work:**

1. Rewrite Quick Start into two flows:

   **Flow A — Code-only validation:**
   - For contributors without KVM/rootfs artifacts:
   ```bash
   cargo test --locked
   python -m unittest discover -s tests -v
   ```

   **Flow B — Full system bring-up:**
   - Requires:
     - pinned Firecracker binary
     - kernel artifact
     - base rootfs
     - KVM-capable host

2. Make artifact prerequisites explicit in README top section (not buried later)

3. Make the Makefile fail with useful errors:
   - Tell operator which artifact is missing
   - Point to the manifest/build script that produces it

4. Keep manifest placeholders, but document them as operator-supplied inputs, not repo defects

**Done when:**
- A clean checkout user understands what can be tested locally vs what needs infra
- Quick start is not pretending guest assets are already available

---

## P1.2 Fix Deploy Path to Match Service Path

**Scope:** Deployment needs to be coherent and reproducible.

**Files:**
- `deploy/deploy.sh`
- `deploy/zeroboot.service`
- `docs/DEPLOYMENT.md`

**Work:**

1. Stop patching the systemd unit with sed after upload:
   - That is brittle

2. Make the service file use the real release layout from the start:
   - Better: service always points at `/var/lib/zeroboot/current/bin/zeroboot`
   - No mutation of service text during deploy

3. Add explicit environment handling for:
   - Pepper file
   - Auth mode
   - Template signature requirement
   - Firecracker version/hash locks

4. Verify that deploy smoke uses the same auth/config assumptions as production startup

5. Add post-switch rollback proof:
   - Ready
   - Health
   - One real `/v1/exec` request

**Done when:**
- Deploy script can create a release, switch symlink, restart service, verify health, and rollback without editing service text in place

---

## P1.3 Add Real Rust Integration Tests for Auth and Template Verification

**Scope:** Python tests are not enough for the critical Rust paths.

**Files:**
- `src/auth.rs`
- `src/template_manifest.rs`
- Tests under `tests/*.rs`

**Work:**

Add Rust tests for:

**Auth verifier:**
- Valid token accepted
- Wrong secret rejected
- Disabled key rejected
- Malformed token rejected
- Prod startup fails without pepper

**Template verifier:**
- Path escape rejected in prod
- Missing schema rejected in prod
- Wrong promotion channel rejected in prod
- Protocol mismatch rejected
- Firecracker hash mismatch rejected
- Signature-required mode fails when unsigned

**Startup config tests:**
- Prod mode + empty verifier -> fail
- Dev mode + missing keys -> allowed

**Done when:**
- Core trust path is covered by Rust tests, not just helper Python tests

---

# P2 — Add Missing Operator Value

## P2.1 Either Implement Warm Pool or Kill the Fiction

**Scope:** Right now the autoscaler script targets routes and metric formats that do not exist.

**Files:**
- `scripts/warm_pool_scaler.py`
- `src/api/handlers.rs`
- New files under `src/pool/` or `src/vmm/pool.rs`
- Docs

**Work:**

**Two valid choices:**

**Option A — Remove it from live docs and mark experimental:**
- Keep script under `experimental/`
- Document that no server support exists yet

**Option B — Implement it fully:**
- Need:
  - Idle VM pool manager
  - Metrics surfaced as machine-readable JSON or a real admin stats endpoint
  - Admin scale route
  - Pool health and eviction logic
  - Borrow/return lifecycle
  - Pool capacity metrics

**Recommendation:** Do not build warm pool until auth and trust chain are fixed. It is not the bottleneck right now.

**Done when:**
- Either: no operator could mistake warm-pool support as current
- Or: end-to-end pool scaling works against real routes

---

## P2.2 Add End-to-End KVM Smoke Script Outside CI

**Scope:** You need one command that proves the system is real on a KVM host.

**Files:**
- New `scripts/smoke_kvm.sh`
- Maybe `verify.sh`
- Docs

**Work:**

Script should do:
1. Validate host prerequisites
2. Validate Firecracker version
3. Build or verify guest image
4. Create template
5. Run test-exec
6. Start API
7. Hit:
   - `/live`
   - `/ready`
   - `/health`
   - `/v1/exec`
8. Stop service
9. Return non-zero on any failure

**Done when:**
- One KVM-capable machine can prove the advertised runtime path with one script

---

## P2.3 Tighten Operator-Facing Docs

**Scope:** After the code is true, make the docs match it exactly.

**Files:**
- `README.md`
- `docs/API.md`
- `docs/ARCHITECTURE.md`
- `docs/DEPLOYMENT.md`
- `UPGRADE_NOTES.md`

**Work:**

Rewrite docs around three truth levels:

**Stable:**
- Snapshot restore
- Guest protocol
- API execution flow
- Manifest/hash verification

**Partial:**
- Deploy rollback
- Artifact reproducibility
- KVM self-hosted CI lane

**Experimental:**
- Warm pool
- Template signing (if still incomplete)
- Multi-language expansion beyond current workers

**Done when:**
- No feature table includes capabilities the runtime does not actually enforce

---

# Exact Implementation Order

This is the order to execute without deviating:

## Pass 1
1. Fix `template_manifest.rs` (mem_meta bug)
2. Fix `signing.rs` (compile errors)
3. Get `cargo check` green
4. Get `cargo test --locked` green

## Pass 2
5. Replace plaintext auth with verifier-based auth
6. Replace `make_api_keys.py`
7. Update app state and handler auth path
8. Add auth tests

## Pass 3
9. Implement real manifest signature verification OR remove the claim completely
10. Wire config-driven enforcement
11. Add manifest verification tests

## Pass 4
12. Fix deploy/service layout
13. Clean README quick start and deployment docs
14. Separate "code validation" from "full KVM bring-up"

## Pass 5
15. Decide fate of warm pool:
    - Implement properly
    - OR demote to experimental/remove from claims

## Pass 6
16. Add one real host-side smoke script
17. Then rerun docs and hardening review

---

# Refactor Map by File

| File | Changes |
|------|---------|
| `src/main.rs` | `load_api_keys` → `load_api_verifier`; startup config validation; fail hard on broken prod auth; cleaner template validation wiring |
| `src/api/handlers.rs` | `AppState.api_keys: Vec<String>` → verifier object; `check_auth()` calls verifier; avoid returning raw key as tenant identity; return key ID/prefix instead |
| `src/auth.rs` | constant-time comparison; clearer record loading errors; expose non-sensitive identity for rate limiting and logging |
| `src/template_manifest.rs` | compile bug fix; config-driven signature requirement; real signature verification call; optional release channel enforcement cleanup |
| `src/signing.rs` | compile fixes; canonical signing input; key loading and verify API usable from manifest verifier |
| `scripts/make_api_keys.py` | generate printable token + hashed record bundle; never store raw token in server JSON |
| `deploy/deploy.sh` | remove service-file mutation; make rollback health check stronger; use current-symlink model cleanly |
| `deploy/zeroboot.service` | point to stable current path; include pepper/config env assumptions explicitly |
| `README.md` | stop saying production-ready until P1 is done; split quick start by environment reality |

---

# Exit Criteria

**Production-ready status:**
- [x] `cargo test --locked` passes
- [x] Auth uses only hashed verifier path
- [x] Prod startup requires pepper + valid active key records
- [x] Manifest signature verification is either real or not claimed
- [x] Deploy flow matches service layout without service mutation hacks
- [ ] KVM smoke passes on one real host (requires KVM hardware)
- [x] README feature table matches actual enforcement

---

# What NOT to Do

**Do not spend time on these before P0/P1:**

- Warm VM pool
- More guest languages
- Dashboard polish
- SDK expansion
- Fancy release metadata
- Autoscaling
- Operator UI

Those are downstream. The repo first needs one truthful spine.
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
