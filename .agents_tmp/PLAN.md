# XBOOT File-by-File Remediation Implementation Plan

## 1. OBJECTIVE

Turn the XBOOT repository from a strong prototype into a defensible bounded execution service by implementing a strict trust chain, guest execution isolation, restore safety, and deployment hardening. The goal is to make prod mode fail-closed and ensure only signed, version-pinned, hash-verified templates can run.

## 2. CONTEXT SUMMARY

The XBOOT codebase is a Rust-based microVM execution service using Firecracker/KVM. Key components:

- **Trust chain files:** `src/config.rs`, `src/main.rs`, `src/signing.rs`, `src/template_manifest.rs`
- **Guest execution:** `guest/worker.py`, `guest/worker_node.js`, `guest/init.c`
- **Deployment:** `deploy/deploy.sh`, `deploy/zeroboot.service`, `scripts/preflight.sh`
- **Restore safety:** `src/vmm/vmstate.rs`, `src/vmm/kvm.rs`, `src/vmm/firecracker.rs`

**Key issues identified:**
- Prod mode has no startup validation (config fields have `#[allow(dead_code)]`)
- Signature payload is ambiguous (concatenates JSON without field names)
- Hardcoded "prod" channel instead of using configured release channel
- Python/Node workers execute user code in long-lived processes (security risk)
- No path confinement for all artifact paths (only snapshots)
- Tests use wrong type for schema_version (string "1.0" vs u32)

## 3. APPROACH OVERVIEW

Implement the fix in the order specified in the blueprint:

1. **Trust chain first:** Add startup fail-closed validation, centralize verification policy, fix canonical signing payload, enforce strict manifest policy
2. **Guest isolation second:** Split Python/Node workers into supervisor + per-request child executor
3. **Restore safety third:** Split vmstate.rs, add pre-restore validation
4. **Deploy hardening last:** Tighten preflight, deploy script, systemd unit
5. **CI and docs:** Add workflow, fix documentation claims

This order matters - don't polish metrics while prod mode can still drift into unsafe configuration.

## 4. IMPLEMENTATION STEPS

### 4.1 Commit 1 — Add Startup Fail-Closed Policy

**Goal:** Make prod startup reject incomplete trust config before the server binds a port.

**Files:**
- `src/config.rs`
- `src/main.rs`

**Method:**

1. In `src/config.rs`, add:
   - `ServerConfig::validate_startup(&self) -> Result<()>` - validates all required prod fields exist
   - `ServerConfig::expected_release_channel(&self) -> Option<&str>` - returns configured channel
   - `ServerConfig::verification_mode(&self) -> VerificationMode` - returns current mode

2. In prod mode, require:
   - `ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true`
   - `ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=true`
   - non-empty `ZEROBOOT_KEYRING_PATH` with file existing
   - non-empty `ZEROBOOT_ALLOWED_FIRECRACKER_VERSION`
   - non-empty `ZEROBOOT_ALLOWED_FC_BINARY_SHA256`
   - non-empty `ZEROBOOT_RELEASE_CHANNEL`
   - `api_keys_file` exists
   - `api_key_pepper_file` exists
   - `logging.log_code == false` (no code logging in prod)

3. Remove `#[allow(dead_code)]` from `release_channel` and `require_template_signatures` fields

4. In `src/main.rs`, call `config.validate_startup()?` at the start of `cmd_serve` before template loading

**Tests:** Add Rust unit tests in src/config.rs:
- prod without keyring fails
- prod without release channel fails
- prod with log_code=true fails
- dev with missing keyring passes

**Reference:** See blueprint Section "Commit 1 — startup fail-closed"

---

### 4.2 Commit 2 — Centralize Manifest Verification Policy

**Goal:** Stop rebuilding verification flags ad hoc in main.rs.

**Files:**
- `src/main.rs`
- `src/template_manifest.rs`

**Method:**

1. Add `ManifestPolicy` struct in `src/template_manifest.rs`:
```rust
pub struct ManifestPolicy<'a> {
    pub mode: VerificationMode,
    pub expected_language: Option<&'a str>,
    pub expected_release_channel: Option<&'a str>,
    pub allowed_firecracker_version: Option<&'a str>,
    pub allowed_firecracker_binary_sha256: Option<&'a str>,
    pub require_hashes: bool,
    pub require_signatures: bool,
    pub keyring_path: Option<&'a Path>,
}
```

2. Add `manifest_policy()` helper in `src/main.rs` that builds policy from config

3. Change `verify_template_artifacts()` signature to accept `&ManifestPolicy` instead of individual params

4. Update both call sites in `validate_snapshot_workdir` and `load_snapshot` to use the helper

**Reference:** See blueprint Section "Commit 2 — centralize manifest verification policy"

---

### 4.3 Commit 3 — Fix Canonical Signing Payload

**Goal:** Make manifest signatures unambiguous and expand signed field coverage.

**Files:**
- `src/signing.rs`
- `src/main.rs` (cmd_sign)

**Method:**

1. Add `canonical_manifest_payload(manifest: &serde_json::Value, signed_fields: &[&str]) -> Result<Vec<u8>>`:
   - Sort fields lexicographically
   - Build deterministic text payload: `field_name=<canonical-json>\n`
   - Reject empty field list and duplicates

2. Update both `sign_manifest()` and `verify_manifest_signature()` to use canonical payload

3. Remove or test-gate `verify_manifest_signature_stub`

4. In `cmd_sign`, expand signed_fields to include:
   - schema_version, template_id, build_id, artifact_set_id, promotion_channel
   - language, kernel_path, kernel_sha256, rootfs_path, rootfs_sha256
   - init_path, snapshot_state_path, snapshot_state_sha256, snapshot_mem_path, snapshot_mem_sha256
   - firecracker_version, firecracker_binary_sha256, protocol_version, vcpu_count, mem_size_mib

**Tests:** Add tests:
- same manifest signs deterministically
- field order doesn't change verification
- mutating promotion_channel breaks verification
- unknown signer key fails
- malformed base64 fails

**Reference:** See blueprint Section "Commit 3 — canonical signing payload"

---

### 4.4 Commit 4 — Enforce Strict Manifest Policy

**Goal:** Make verify_template_artifacts the one hard gate for prod templates.

**Files:**
- `src/template_manifest.rs`
- Fix broken tests

**Method:**

1. Fix test helper - schema_version is Option<u32>, not string:
```rust
fn create_test_manifest(workdir: &Path, promotion_channel: &str, schema_version: Option<u32>) -> Result<PathBuf>
```

2. Replace hardcoded "prod" channel check with policy-based check:
```rust
if policy.mode == VerificationMode::Prod {
    let expected = policy.expected_release_channel.unwrap_or("prod");
    match manifest.promotion_channel.as_deref() {
        Some(actual) if actual == expected => {}
        Some(actual) => bail!("template not promoted to required channel: got '{}', expected '{}'", actual, expected),
        None => bail!("template manifest missing promotion_channel in prod mode"),
    }
}
```

3. Enforce required prod fields: schema_version, template_id, build_id, artifact_set_id, promotion_channel, language, protocol_version, firecracker_version, firecracker_binary_sha256, signer_key_id (if signatures required), manifest_signature (if signatures required), manifest_signed_fields (if signatures required), kernel_sha256 (if hashes required), rootfs_sha256 (if hashes required), snapshot_state_sha256 (if hashes required), snapshot_mem_sha256 (if hashes required)

4. Add schema version support check (currently only version 1 is supported)

5. Expand path confinement to all artifact paths (kernel_path, rootfs_path, init_path, snapshot paths) not just snapshots

6. Add helper functions: `require_prod_string_field()`, `require_prod_u32_field()`, `require_supported_schema_version()`, `resolve_manifest_artifact_path()`

**Tests:** Add tests:
- prod rejects missing template_id
- prod rejects wrong release channel
- prod rejects path escape via ../
- prod rejects symlink escape
- prod rejects missing firecracker_version when pinned
- prod rejects unsupported schema_version

**Reference:** See blueprint Section "Commit 4 — strict manifest enforcement"

---

### 4.5 Commit 5 — Update Python Validator Script

**Goal:** Make scripts/validate_template_manifest.py enforce the same policy as Rust runtime.

**Files:**
- `scripts/validate_template_manifest.py`

**Method:**

Add strict CLI flags:
- `--prod`
- `--expected-channel`
- `--expected-fc-version`
- `--expected-protocol`
- `--require-signature`

Validate:
- Required prod fields presence
- Channel match
- Firecracker version match
- Protocol version match
- Signature-related fields exist when required

**Reference:** See blueprint Section "Commit 5 — bring the Python validator script up to prod parity"

---

### 4.6 Commit 6 — Harden Preflight and Deploy Gating

**Goal:** Make bad prod config or bad artifacts fail before service restart.

**Files:**
- `scripts/preflight.sh`
- `deploy/deploy.sh`

**Method:**

1. In `preflight.sh`, in prod require:
   - Keyring exists
   - Release channel exists
   - Allowed Firecracker version exists
   - Allowed Firecracker binary SHA exists
   - /dev/kvm access check
   - Call firecracker_compat.py
   - Call strict validate_template_manifest.py when workdir is set

2. In `deploy.sh`:
   - Write environment atomically (use temp file + move)
   - Record current symlink target before switch
   - Run strict manifest validation after template creation
   - Smoke both runtimes (Python AND Node)
   - Check both /v1/live and /v1/ready
   - Use recorded previous symlink for rollback (not ls | tail -1)

**Reference:** See blueprint Section "Commit 6 — harden preflight and deploy gating"

---

### 4.7 Commit 7 — Split Python Worker into Supervisor + Child Executor

**Goal:** Stop running untrusted code in the long-lived Python worker process.

**Files:**
- `guest/worker.py`
- `guest/worker_exec.py` (new)
- `tests/test_worker_protocol.py`

**Method:**

1. Transform `guest/worker.py` into supervisor only:
   - Parse WRK1 requests
   - Create fresh scratch dir under configured root
   - Spawn worker_exec.py as child
   - Send code/stdin through pipes
   - Enforce timeout with hard kill
   - Collect stdout/stderr, truncate
   - Return WRK1R response

2. Create `guest/worker_exec.py`:
   - Set RLIMIT_CPU, RLIMIT_AS, RLIMIT_FSIZE, RLIMIT_NOFILE, RLIMIT_NPROC
   - Set cwd to fresh scratch dir
   - Clear env to allowlist only
   - Run code once, exit

3. Delete "restore interpreter state" logic as security mechanism - it's not the boundary anymore

**Tests:** Add tests:
- timeout kills child cleanly
- stdout/stderr truncation works
- cwd resets per request
- env allowlist works
- request N cannot affect request N+1

**Reference:** See blueprint Section "Commit 7 — split Python worker into supervisor and child executor"

---

### 4.8 Commit 8 — Split Node Worker into Supervisor + Child Executor

**Goal:** Apply same isolation model to Node worker.

**Files:**
- `guest/worker_node.js`
- `guest/worker_node_exec.js` (new)

**Method:**

1. Transform `guest/worker_node.js` into supervisor only:
   - Read requests
   - Spawn child runner with node
   - Set timeout
   - Collect/truncate output
   - Frame response

2. Create `guest/worker_node_exec.js`:
   - Run user code once
   - Use fresh cwd and scratch dir
   - Use strict env allowlist
   - Avoid exposing require unless explicitly intended
   - Exit after one request

3. Do not present vm.runInNewContext as security - isolation is microVM + subprocess boundary

**Tests:** Add Node worker tests:
- timeout
- runtime error
- stdout/stderr truncation
- no cross-request persistence

**Reference:** See blueprint Section "Commit 8 — split Node worker into supervisor and child executor"

---

### 4.9 Commit 9 — Update Guest Supervisor to Manage Subprocess-Based Workers

**Goal:** Teach guest boot process that workers are supervisors, not execution containers.

**Files:**
- `guest/init.c`

**Method:**

1. Keep Rust-side serial protocol unchanged

2. Add environment setup before worker start:
   - ZEROBOOT_SCRATCH_ROOT=/tmp/zeroboot
   - ZEROBOOT_EXEC_MEM_LIMIT_MB=...
   - ZEROBOOT_EXEC_CPU_LIMIT_SECS=...
   - ZEROBOOT_EXEC_NOFILE_LIMIT=...
   - ZEROBOOT_EXEC_NPROC_LIMIT=...

3. At boot:
   - Create /tmp/zeroboot with mode 0700
   - Fail readiness if scratch root cannot be created

4. Strengthen restart logic:
   - Maintain restart counters per worker
   - If repeated restarts exceed threshold, stop advertising healthy state
   - Distinguish: worker_boot_failed, worker_protocol_failed, worker_timeout, worker_restart_exhausted

**Reference:** See blueprint Section "Commit 9 — update guest supervisor to manage subprocess-based workers"

---

### 4.10 Commit 10 — Update Guest Rootfs Builder

**Goal:** Ensure image build process stages new worker files and scratch layout.

**Files:**
- `scripts/build_guest_rootfs.sh`
- `manifests/python-guest.manifest`
- `manifests/node-guest.manifest`

**Method:**

1. Copy both supervisor and child files into staging tree:
   - /zeroboot/worker.py
   - /zeroboot/worker_exec.py
   - /zeroboot/worker_node.js
   - /zeroboot/worker_node_exec.js

2. Create /tmp/zeroboot directory

3. Include new hashes in output manifest

4. Set worker files read-only, scratch dir writable only where needed

**Reference:** See blueprint Section "Commit 10 — update guest rootfs builder for the new runtime layout"

---

### 4.11 Commit 11 — Split vmstate.rs into Parser, Compat, Validate

**Goal:** Make snapshot-format drift manageable.

**Files:**
- `src/vmm/vmstate.rs` → split into:
  - `src/vmm/vmstate/mod.rs`
  - `src/vmm/vmstate/schema.rs`
  - `src/vmm/vmstate/parser.rs`
  - `src/vmm/vmstate/compat.rs`
  - `src/vmm/vmstate/validate.rs`
- `tests/fixtures/vmstate/...` (new)

**Method:**

1. Split responsibilities:
   - schema.rs: parsed data structures
   - parser.rs: raw binary parsing
   - compat.rs: version rules and accepted formats
   - validate.rs: sanity checks before restore

2. Add fixture-backed tests for every supported Firecracker/vmstate version

3. Tests for each fixture:
   - parse succeeds for supported version
   - parse fails cleanly for unsupported version
   - corrupt header fails
   - impossible offsets fail

**Reference:** See blueprint Section "Commit 11 — split vmstate.rs into parser, compat, validate"

---

### 4.12 Commit 12 — Add Pre-Restore Snapshot Validation in KVM Path

**Goal:** Reject bad snapshots before mutating KVM state.

**Files:**
- `src/vmm/kvm.rs`

**Method:**

Add explicit validation helpers before restore:
- Memory size matches manifest expectations
- vCPU count bounds
- CPUID entry count bounds
- MSR count bounds
- Page alignment sanity
- LAPIC and IOAPIC requirements

Return explicit error classes: bad_snapshot_state, unsupported_vmstate_version, invalid_cpuid, invalid_msr_set, memory_map_reject

**Reference:** See blueprint Section "Commit 12 — add pre-restore snapshot validation in KVM path"

---

### 4.13 Commit 13 — Enrich Template Creation Provenance

**Goal:** Make template manifests complete enough for strict prod use.

**Files:**
- `src/vmm/firecracker.rs`
- `src/main.rs`
- `src/template_manifest.rs`

**Method:**

When creating template manifests, populate all prod fields:
- schema_version, template_id, build_id, artifact_set_id
- promotion_channel, language
- firecracker_version, firecracker_binary_sha256
- protocol_version
- kernel_sha256, rootfs_sha256
- snapshot_state_sha256, snapshot_mem_sha256
- sizes, timestamps, vCPU count, memory size

Generate unsigned manifest first, then let zeroboot sign it.

**Reference:** See blueprint Section "Commit 13 — enrich template creation provenance"

---

### 4.14 Commit 14 — Expand API State, Metrics, and Request Provenance

**Goal:** Make failures diagnosable.

**Files:**
- `src/api/handlers.rs`

**Method:**

1. Expand template status from generic health to explicit categories:
   - healthy
   - quarantined_trust
   - quarantined_health
   - unsupported_version

2. Expand request log fields:
   - template id, build id, artifact set id, promotion channel
   - manifest digest, Firecracker version
   - queue wait ms, restore ms, exec ms
   - recycle flag, protocol error flag

3. Add metrics for:
   - manifest verification failures
   - signature verification failures
   - template version mismatches
   - restore failures
   - worker boot failures
   - worker protocol failures
   - guest unhealthy templates

**Reference:** See blueprint Section "Commit 14 — expand API state, metrics, and request provenance"

---

### 4.15 Commit 15 — Tighten Systemd Unit Without Breaking KVM

**Goal:** Harden service boundaries carefully.

**Files:**
- `deploy/zeroboot.service`

**Method:**

Keep current KVM access, add/tighten only what service can tolerate:
- UMask=0077
- ProtectHome=true
- RestrictSUIDSGID=true
- ProtectProc=invisible
- ProcSubset=pid
- explicit ReadWritePaths=...
- keep DeviceAllow=/dev/kvm rw

Don't blindly add hardening that blocks Firecracker or KVM. Test each change.

**Reference:** See blueprint Section "Commit 15 — tighten systemd unit without breaking KVM"

---

### 4.16 Commit 16 — Add Real CI and Merge Gates

**Goal:** Force every future change through core validation path.

**Files:**
- `.github/workflows/ci.yml` (new)
- `Makefile` (if needed)

**Method:**

CI should run:
- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test
- Python tests
- manifest validator tests
- guest worker protocol tests
- shell checks for scripts (if shellcheck available)

If CI runners cannot access KVM, keep KVM integration in separate staging job.

**Reference:** See blueprint Section "Commit 16 — add real CI and merge gates"

---

### 4.17 Commit 17 — Fix Docs and Claims

**Goal:** Bring README and deployment docs back in line with reality.

**Files:**
- `README.md`
- `docs/ARCHITECTURE.md`
- `docs/DEPLOYMENT.md`
- `UPGRADE_NOTES.md`
- `PATCHES_APPLIED.md`

**Method:**

Replace vague claims like "production-ready" with concrete statements:
- What prod mode enforces
- Supported Firecracker versions
- Guest isolation model
- What is/is not the security boundary
- Required deployment inputs
- What is still not proven for hostile public multitenancy

**Reference:** See blueprint Section "Commit 17 — fix docs and claims"

## 5. TESTING AND VALIDATION

**Success criteria by phase:**

### Phase 0 — Trust Chain (Commits 1-4)
- Prod start fails without keyring/version/channel pins
- Unsigned manifest cannot run in prod
- Manifest mutation invalidates signature
- Path escape attempts fail
- Wrong Firecracker version fails

### Phase 1 — Guest Isolation (Commits 7-10)
- Python no longer runs user code in persistent process
- Node no longer runs user code in persistent process
- Per-request child timeout works
- Per-request scratch dir resets
- Cross-request state contamination tests pass

### Phase 2 — Restore/Version Safety (Commits 11-13)
- vmstate fixtures exist for every supported Firecracker version
- Unsupported versions fail cleanly
- Corrupt vmstate fails cleanly
- Restore validates before KVM mutation

### Phase 3 — Ops (Commits 14-17)
- Deploy verifies both runtimes
- Logs include provenance metadata
- Metrics expose trust vs restore vs runtime failure
- Service hardening does not break KVM

**Key test files to add/modify:**
- `tests/config_startup.rs` - config validation tests
- `tests/manifest_policy.rs` - policy construction tests
- `tests/test_signing.rs` - canonical payload tests
- `tests/test_template_manifest_strict.rs` - strict manifest enforcement tests
- `tests/test_guest_worker_subprocess.py` - subprocess isolation tests
- `tests/fixtures/vmstate/` - vmstate version fixtures
- `.github/workflows/ci.yml` - CI pipeline

---

## 5. TESTING AND VALIDATION

**Success criteria by phase:**

### Phase 0 — Trust Chain (Commits 1-4)
- Prod start fails without keyring/version/channel pins
- Unsigned manifest cannot run in prod
- Manifest mutation invalidates signature
- Path escape attempts fail
- Wrong Firecracker version fails

### Phase 1 — Guest Isolation (Commits 7-10)
- Python no longer runs user code in persistent process
- Node no longer runs user code in persistent process
- Per-request child timeout works
- Per-request scratch dir resets
- Cross-request state contamination tests pass

### Phase 2 — Restore/Version Safety (Commits 11-13)
- vmstate fixtures exist for every supported Firecracker version
- Unsupported versions fail cleanly
- Corrupt vmstate fails cleanly
- Restore validates before KVM mutation

### Phase 3 — Ops (Commits 14-17)
- Deploy verifies both runtimes
- Logs include provenance metadata
- Metrics expose trust vs restore vs runtime failure
- Service hardening does not break KVM

**Key test files to add/modify:**
- `tests/config_startup.rs` - config validation tests
- `tests/manifest_policy.rs` - policy construction tests
- `tests/test_signing.rs` - canonical payload tests
- `tests/test_template_manifest_strict.rs` - strict manifest enforcement tests
- `tests/test_guest_worker_subprocess.py` - subprocess isolation tests
- `tests/fixtures/vmstate/` - vmstate version fixtures
- `.github/workflows/ci.yml` - CI pipeline

## 5. TESTING AND VALIDATION

**Success criteria by phase:**

### Phase 0 — Trust Chain (Commits 1-4)
- Prod start fails without keyring/version/channel pins
- Unsigned manifest cannot run in prod
- Manifest mutation invalidates signature
- Path escape attempts fail
- Wrong Firecracker version fails

### Phase 1 — Guest Isolation (Commits 7-10)
- Python no longer runs user code in persistent process
- Node no longer runs user code in persistent process
- Per-request child timeout works
- Per-request scratch dir resets
- Cross-request state contamination tests pass

### Phase 2 — Restore/Version Safety (Commits 11-13)
- vmstate fixtures exist for every supported Firecracker version
- Unsupported versions fail cleanly
- Corrupt vmstate fails cleanly
- Restore validates before KVM mutation

### Phase 3 — Ops (Commits 14-17)
- Deploy verifies both runtimes
- Logs include provenance metadata
- Metrics expose trust vs restore vs runtime failure
- Service hardening does not break KVM

**Key test files to add/modify:**
- `tests/config_startup.rs` - config validation tests
- `tests/manifest_policy.rs` - policy construction tests
- `tests/test_signing.rs` - canonical payload tests
- `tests/test_template_manifest_strict.rs` - strict manifest enforcement tests
- `tests/test_guest_worker_subprocess.py` - subprocess isolation tests
- `tests/fixtures/vmstate/` - vmstate version fixtures
- `.github/workflows/ci.yml` - CI pipeline
