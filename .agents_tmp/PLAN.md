# XBOOT Full Implementation Plan

## Executive Summary

This plan transforms XBOOT from a strong pre-production sandbox into a system ready for internal production use. The approach prioritizes truth and correctness before feature expansion.

**Target State:**
- Buildable from a clean supported host
- Reproducible guest artifact generation
- Measurable under load
- Safe for internal multi-tenant use with trusted callers
- Ready for public hardening pass later

**Strategy:** Five phases executed in order:
1. Make it build and boot
2. Prove one real exec path  
3. Make it operationally stable
4. Make it secure enough for serious use
5. Make it scalable and consumable

---

## Phase 0 — Establish Baseline (P0)

### P0.1 Truth Reset and Support Matrix

**Goal:** Remove false claims, define exact supported environment.

**Files:**
- `README.md`
- `docs/DEPLOYMENT.md`
- `docs/ARCHITECTURE.md`
- `PATCHES_APPLIED.md`
- `UPGRADE_NOTES.md`

**Actions:**
- Remove/soften any "production-ready" claims
- State exact support matrix:
  - Linux distro/version (e.g., Ubuntu 22.04)
  - Kernel minimum version
  - KVM required
  - Rust toolchain version
  - Firecracker version
  - Python version
  - Node version
  - cgroup/systemd expectations
- Document the real runtime spine:
  ```
  API → admission → VM allocate/restore → serial protocol → 
  guest supervisor → child exec → validated response → metrics/logs
  ```

**Done when:** Docs no longer overclaim; supported environment is concrete.

---

### P0.2 Canonical Artifact Layout

**Goal:** Define single artifact directory structure all code agrees on.

**Files:**
- `docs/DEPLOYMENT.md`
- `src/config.rs`
- `src/template_manifest.rs`
- `scripts/validate_template_manifest.py`
- `manifests/*.manifest`
- `manifests/*.lock.json`

**Actions:**
Define canonical layout:
```
artifacts/
  firecracker/
  kernel/
  rootfs/
  templates/
    python/
    node/
```
Ensure config resolves paths from this layout consistently.

**Done when:** One documented layout; config and validation agree.

---

### P0.3 Strict Preflight

**Goal:** Fail before boot if host or artifacts are wrong.

**Files:**
- `scripts/preflight.sh`
- `docs/DEPLOYMENT.md`

**Actions:** Preflight must hard-fail on:
- Missing /dev/kvm
- Missing Firecracker binary
- Missing kernel/rootfs/template artifacts
- Wrong permissions/ownership
- Unsupported host kernel/systemd
- Manifest mismatch
- Missing signing policy files in production mode

**Done when:** Preflight returns nonzero for any missing hard dependency.

---

### P0.4 Host Build and Test Proof

**Goal:** Prove Rust host builds and tests work.

**Files:**
- `Cargo.toml`
- `Cargo.lock`
- `src/**/*.rs`

**Actions:**
- Ensure `cargo build --release` succeeds
- Ensure `cargo test` passes
- Fix any dependency drift or module issues

**Done when:** Release build and tests pass on clean supported host.

---

### P0.5 Deterministic Guest Artifact Build

**Goal:** Make guest image/template generation reproducible.

**Files:**
- `scripts/build_guest_rootfs.sh`
- `scripts/build_rootfs_image.sh`
- `scripts/build_reproducible_image.py`
- `manifests/*.manifest`
- `manifests/*.lock.json`

**Actions:**
- Pin tool versions where possible
- Normalize output paths
- Ensure output hashes match manifest expectations
- Document one canonical artifact build sequence

**Done when:** Clean host can rebuild rootfs/template set; hashes match.

---

### P0.6 Python-Only Golden Path

**Goal:** Get one language path fully real before expanding.

**Files:**
- `guest/worker_supervisor.py`
- `guest/worker_child.py`
- `manifests/python-guest.manifest`
- `manifests/python-build.lock.json`

**Actions:**
- Treat Python as only supported path for milestone 1
- Ensure guest supervisor and child model are clean
- Defer Node until later

**Done when:** Python template builds; golden smoke path defined.

---

### P0.7 Protocol Handshake Validation

**Goal:** Prove host↔guest protocol is strict and fail-closed.

**Files:**
- `src/protocol.rs`
- `src/vmm/serial.rs`
- `guest/worker_supervisor.py`

**Actions:** Verify:
- Ready handshake
- Request ID round-trip
- Protocol version check
- Checksum validation
- Malformed frame rejection
- Response/request size bounds

**Done when:** Host rejects malformed/mismatched frames; VM marked bad on protocol mismatch.

---

### P0.8 One Real KVM-Backed Smoke Flow

**Goal:** Prove end-to-end execution with real VM.

**Files:**
- `src/main.rs`
- `src/api/handlers.rs`
- `src/vmm/firecracker.rs`
- `src/vmm/kvm.rs`
- `src/vmm/vmstate.rs`
- `guest/*.py`
- Smoke script

**Actions:** Build canonical smoke test:
1. Run preflight
2. Start API
3. POST one Python request
4. Receive valid JSON result
5. Verify metrics changed
6. Clean shutdown

**Done when:** One KVM-backed end-to-end smoke works; documented and scriptable.

---

## Phase 1 — Make Runtime Trustworthy Under Failure (P1)

### P1.1 Explicit VM State Machine

**Goal:** Replace implicit state with hard state model.

**Files:**
- `src/vmm/vmstate.rs`
- `src/vmm/firecracker.rs`
- `src/vmm/kvm.rs`

**Actions:** Define states:
- Cold, Restoring, Ready, Busy, Draining, Corrupt, Dead

Make transitions explicit.

**Done when:** VM lifecycle uses explicit states, not loose booleans.

---

### P1.2 Quarantine and Teardown Rules

**Goal:** Never reuse suspect VM.

**Files:**
- `src/vmm/vmstate.rs`
- `src/vmm/firecracker.rs`
- `src/vmm/kvm.rs`

**Actions:** Quarantine on:
- Checksum mismatch
- Protocol mismatch
- Malformed ready handshake
- Repeated timeout
- Unexpected EOF mid-request
- Corrupt response decode

Make teardown idempotent.

**Done when:** Corrupt VMs marked unusable; teardown safe to call twice.

---

### P1.3 Guest Subprocess Hardening

**Goal:** Make per-request guest execution bounded and clean.

**Files:**
- `guest/worker_supervisor.py`
- `guest/worker_child.py`
- `guest/worker_supervisor.js`
- `guest/worker_child.js`

**Actions:** Enforce:
- Timeout kill
- Zombie cleanup
- Bounded stdout/stderr
- Bounded total response size
- Sanitized env
- Fresh per-request workdir
- Temp cleanup
- Structured child invocation (no shell)

**Done when:** One request cannot pollute next; output/runtime bounded.

---

### P1.4 Host Timeout and Abort Discipline

**Goal:** Hung guests don't wedge the API.

**Files:**
- `src/api/handlers.rs`
- `src/vmm/firecracker.rs`
- `src/vmm/kvm.rs`
- `src/vmm/serial.rs`

**Actions:**
- Host timeout cancels request cleanly
- Bad guest response doesn't produce partial success
- Slow/stalled serial path fails and quarantines appropriately

**Done when:** API returns proper timeout/error; bad guest can't hang request threads.

---

### P1.5 Critical Panic Removal

**Goal:** Remove unwrap()/expect() from hot paths.

**Files:** All `src/**/*.rs`

**Actions:** Search and replace panic-prone calls in:
- Config load, auth load, manifest load
- VM create/restore, serial I/O
- API handling, metrics init

**Done when:** No panic-based failure in core request path.

---

### P1.6 Canonical Error Taxonomy

**Goal:** Make failures understandable, map to HTTP results.

**Files:**
- `src/api/handlers.rs`
- `src/api/errors.rs` (create)
- `src/config.rs`
- `src/template_manifest.rs`
- `src/vmm/*.rs`
- `src/auth.rs`

**Actions:** Define error families:
- Config, Auth, Admission, Artifact validation
- VM allocation, Protocol, Guest execution
- Timeout, Capacity, Invariant violation

Map each to stable HTTP status and response body.

**Done when:** API errors stable and typed; logs include machine-readable error codes.

---

### P1.7 API Admission Control Before Expensive Work

**Goal:** Reject bad requests before touching VMs.

**Files:**
- `src/api/handlers.rs`
- `src/auth.rs`
- `src/config.rs`

**Actions:** Before VM allocation, reject:
- Invalid auth, unsupported language
- Bad template/channel, oversized payload
- Batch too large, impossible timeout
- Queue full

**Done when:** Bad caller input never reaches expensive restore/exec path.

---

### P1.8 Metrics and Structured Logs

**Goal:** Give operators visibility to debug and measure.

**Files:**
- `src/main.rs`
- `src/api/handlers.rs`
- `deploy/grafana-dashboard.json`

**Actions:** Emit:
- Request count by outcome/language/template
- Request latency histogram, queue wait histogram
- VM restore latency, guest exec latency
- Timeout count, protocol error count
- Auth rejection count, VM counts by state

Use structured logs with: request_id, vm_id, template_id, client_id, language, duration_ms, outcome, error_code

**Done when:** Dashboard reflects real metrics; logs are parseable.

---

## Phase 2 — Make Maintainable and Performant (P2)

### P2.1 Split Oversized Host Files

**Goal:** Break large modules into coherent pieces.

**Files:**
- `src/main.rs` → `src/bootstrap.rs`, `src/server.rs`, `src/runtime.rs`
- `src/api/handlers.rs` → `src/api/exec.rs`, `src/api/batch.rs`, `src/api/health.rs`, `src/api/metrics.rs`, `src/api/errors.rs`
- `src/template_manifest.rs` → `src/template_manifest/schema.rs`, `src/template_manifest/verify.rs`, `src/template_manifest/policy.rs`

**Done when:** Each file has one dominant responsibility; build/tests pass.

---

### P2.2 Auth/Docs Alignment

**Goal:** Documented auth matches implemented auth.

**Files:**
- `src/auth.rs`
- `docs/API.md`
- `scripts/make_api_keys.py`

**Actions:**
- If using HMAC-hashed keys with peppering, document it
- Remove stale "plain key JSON" narrative
- Define trusted proxy rules, identity derivation, rate-limit subject rules

**Done when:** API docs accurately describe auth; key generation matches runtime.

---

### P2.3 Warm Pool as First-Class Runtime

**Goal:** Turn low-latency architecture into measurable capacity system.

**Files:**
- `scripts/warm_pool_scaler.py`
- `src/vmm/*.rs`
- `src/config.rs`

**Actions:** Define:
- Minimum idle pool per template, max pool size
- Refill threshold, max requests per VM, max age
- Drain/retire policy, corruption handling
- Separate warm-hit from cold-start path

**Done when:** Runtime maintains idle VMs; warm vs cold metrics explicit.

---

### P2.4 Load and Chaos Validation

**Goal:** Measure actual behavior under stress.

**Files:** Test harness scripts, CI workflows

**Actions:** Add scenarios:
- Concurrent request load, repeated timeouts
- Malformed protocol frames, VM death mid-request
- Queue overload, warm-pool depletion

**Done when:** Reproducible latency/failure data; p50/p95/p99 warm/cold separated.

---

### P2.5 SDK Stabilization

**Goal:** Make Python/Node SDKs usable against stable v1.

**Files:**
- `sdk/python/**`
- `sdk/node/**`
- `docs/API.md`

**Actions:** Support only:
- Execute, batch execute, health/ready
- Auth config, typed errors, timeout handling

**Done when:** Each SDK runs golden smoke flow; error handling matches API v1.

---

### P2.6 Operator Runbook

**Goal:** Give operator enough to run, debug, rotate, recover.

**Files:**
- `docs/RUNBOOK.md` (create)
- `docs/DEPLOYMENT.md`

**Actions:** Document:
- Preflight, artifact install, first boot, health validation
- Key rotation, artifact rotation, draining/restarting
- Quarantining bad templates/VMs, reading dashboard/logs
- Rollback process

**Done when:** Operator who didn't write system can run it; recovery steps explicit.

---

## Phase 3 — Finish Supply Chain and Release (P3)

### P3.1 Signed Artifact Enforcement

**Goal:** Turn signing from hook into actual policy.

**Files:**
- `src/signing.rs`
- `src/template_manifest.rs`
- Manifest files, deployment docs

**Actions:** Decide and enforce:
- Signature required/optional by environment
- Accepted key formats, rejection behavior
- Revocation/rotation workflow

**Done when:** Production refuses unsigned/invalid artifacts; docs explain signing.

---

### P3.2 Promotion Pipeline

**Goal:** Artifacts move through environments with provenance.

**Files:** CI workflows, deployment scripts, docs

**Actions:** Define:
- dev → staging → production promotion
- Hash pinning, signature verification at each stage
- Provenance: source commit, build timestamp, tool versions, artifact hashes

**Done when:** Operator can answer where production template came from.

---

### P3.3 Self-Hosted KVM CI as Release Gate

**Goal:** Live validation is mandatory, not optional.

**Files:** `.github/workflows/*`

**Actions:** Make release-blocking KVM CI:
1. Build host binary
2. Build guest artifacts
3. Validate manifests
4. Create template, boot/restore VM
5. Run smoke exec, timeout exec, malformed frame rejection
6. Export metrics/log artifacts

**Done when:** No release without live KVM validation.

---

### P3.4 Security Review Pass

**Goal:** Close obvious gaps before public exposure.

**Files:** All source, guest, deployment, docs

**Actions:** Review:
- Guest env leakage, network exposure
- Path traversal, queue abuse, batch amplification
- Logging of user code/secrets
- Firecracker/jailer confinement, systemd hardening
- Rootfs immutability expectations

**Done when:** High-severity findings fixed or documented as blockers.

---

## Detailed Patch Map

### Patch Set 1 — Truth, Docs, Build Contract

#### README.md
- Remove "production-ready" claims
- Add hard support matrix
- Add "What this repo does not include" section
- Add canonical runtime path diagram

#### docs/API.md
- Fix auth section to match src/auth.rs (hashed records, not raw keys)
- Add stable error envelope examples
- Add truncation semantics for stdout/stderr

#### docs/DEPLOYMENT.md
- Replace vague deployment language with strict sequence
- Add canonical artifact layout
- Add prod-mode checklist

#### docs/ARCHITECTURE.md
- Add trust boundary section
- Add "Known current limits" section

---

### Patch Set 2 — Auth and Startup Integrity

#### src/auth.rs → Split into:
- `src/auth/mod.rs`
- `src/auth/records.rs` - ApiKeyRecord
- `src/auth/verifier.rs` - verification logic
- `src/auth/context.rs` - header extraction

Use constant-time compare; return typed auth errors.

#### scripts/make_api_keys.py
- Replace raw token arrays with hashed records + secrets file
- Add --label, --pepper-file, --records-output, --tokens-output flags

#### src/config.rs → Split into:
- `src/config/mod.rs`
- `src/config/auth.rs`
- `src/config/limits.rs`
- `src/config/logging.rs`
- `src/config/artifacts.rs`
- `src/config/pool.rs`
- `src/config/validation.rs`

Strengthen validate_startup() for prod mode.

---

### Patch Set 3 — API Surface and Admission

#### src/api/handlers.rs → Split into:
- `src/api/mod.rs`
- `src/api/types.rs`
- `src/api/errors.rs`
- `src/api/auth.rs`
- `src/api/admission.rs`
- `src/api/exec.rs`
- `src/api/batch.rs`
- `src/api/health.rs`
- `src/api/metrics.rs`
- `src/api/logging.rs`

Add typed ApiError enum; separate timing metrics; move request rejection before VM acquisition.

---

### Patch Set 4 — Runtime Bootstrap and CLI

#### src/main.rs → Split into:
- `src/main.rs` (thin - parse/dispatch only)
- `src/cli.rs`
- `src/bootstrap.rs`
- `src/server.rs`
- `src/template_cmd.rs`
- `src/test_exec_cmd.rs`
- `src/fork_bench_cmd.rs`
- `src/keygen_cmd.rs`
- `src/sign_cmd.rs`

Create single build_app_state() function.

---

### Patch Set 5 — Template Trust and Supply Chain

#### src/template_manifest.rs → Split into:
- `src/template_manifest/mod.rs`
- `src/template_manifest/schema.rs`
- `src/template_manifest/verify.rs`
- `src/template_manifest/policy.rs`
- `src/template_manifest/errors.rs`

Define one canonical JSON manifest schema; make path confinement explicit.

#### scripts/validate_template_manifest.py
- Update to validate same canonical schema
- Add --mode dev|staging|prod flag

#### src/signing.rs → Split into:
- `src/signing/mod.rs`
- `src/signing/keys.rs`
- `src/signing/verify.rs`
- `src/signing/sign.rs`

Define environment behavior; support key rotation.

---

### Patch Set 6 — VM Lifecycle

#### src/vmm/vmstate.rs
- Add explicit VmLifecycleState enum
- Add VmCorruptionReason enum

#### src/vmm/firecracker.rs → Split into:
- `src/vmm/firecracker.rs`
- `src/vmm/process.rs`
- `src/vmm/restore.rs`
- `src/vmm/guest_ready.rs`

Make teardown idempotent; emit lifecycle events.

#### src/vmm/kvm.rs → Split into:
- `src/vmm/kvm.rs`
- `src/vmm/allocator.rs`
- `src/vmm/fork.rs`
- `src/vmm/executor.rs`

Return structured ExecutionTrace.

#### src/vmm/serial.rs
- Harden framed I/O; add max frame size, request ID correlation
- Move protocol parsing to src/protocol.rs

---

### Patch Set 7 — Guest Execution

#### guest/worker_supervisor.py
- Minimal allowlisted environment
- Unique per-request scratch dir
- Hard timeout/output enforcement
- Structured child invocation (no shell)

#### guest/worker_child.py
- Keep tiny - receive, execute, emit, exit
- Explicit truncation markers

#### guest/init.c
- Audit boot-time responsibilities
- Split if doing too much

---

### Patch Set 8 — Preflight, Build, CI

#### scripts/preflight.sh
- Add --template <dir> mode
- Check prod-mode required files

#### scripts/build_guest_rootfs.sh
- Emit metadata JSON with build timestamp, source commit, versions

#### .github/workflows/ci.yml
- Fix manifest validation job
- Make KVM smoke include malformed frame test, timeout test
- Archive logs and metrics

---

### Patch Set 9 — Service Hardening

#### deploy/zeroboot.service
- Add prod env example
- Consider hardening additions that don't break KVM

#### deploy/deploy.sh
- Make deployment atomic with release layout
- Add preflight, smoke test, rollback

#### deploy/grafana-dashboard.json
- Align panels to emitted metrics only

---

## Testing Plan

### New Test Files to Create:
- `tests/test_auth_records.py` - key generation and verification
- `tests/test_api_error_mapping.py` - error status codes
- `tests/test_preflight_modes.py` - preflight strictness
- `tests/test_rate_limit_identity.py` - proxy and identity derivation
- `tests/test_manifest_policy_modes.py` - dev/staging/prod enforcement

### Extend Existing Tests:
- `tests/test_template_manifest_validator.py` - missing fields, path escape, invalid channel
- `tests/test_worker_protocol.py` - mismatched ID, oversized stdout, malformed flags
- `tests/test_guest_worker_subprocess.py` - scratch cleanup, env scrub, no state bleed

---

## Milestone Sequence

| Milestone | Includes | Result |
|-----------|----------|--------|
| A - Honest and Buildable | P0.1-P0.4 | Docs honest; host builds; artifact layout coherent; preflight catches issues |
| B - One Real Execution | P0.5-P0.8 | Python path works through real KVM |
| C - Failure-Safe Runtime | P1.1-P1.8 | Runtime survives corruption, timeout, bad callers |
| D - Operable Service | P2.1-P2.6 | Maintainable code; docs match auth; SDKs usable; runbook exists |
| E - Fast and Measured | P2.3-P2.4 | Warm pool exists; latency/capacity measured |
| F - Controlled Release | P3.1-P3.4 | Artifact trust, promotion, live validation, security posture real |

---

## Execution Priority (First 10 Patches)

1. **README.md** - Remove false production claims
2. **docs/API.md** - Fix auth docs to match src/auth.rs
3. **scripts/make_api_keys.py** - Generate hashed records, not raw-key arrays
4. **src/config.rs** - Strengthen startup validation and proxy parsing
5. **scripts/preflight.sh** - Make checks real
6. **src/api/handlers.rs** - Extract errors.rs, types.rs, admission.rs
7. **src/template_manifest.rs** - Define one canonical manifest schema
8. **scripts/validate_template_manifest.py** - Validate same schema as runtime
9. **src/vmm/firecracker.rs + kvm.rs** - Add explicit quarantine and idempotent teardown
10. **guest/worker_supervisor.py + worker_child.py** - Enforce scratch dir, env scrub, output caps

That sequence gives truth, then startup integrity, then request-path safety.
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
