use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::protocol;
use crate::signing;

pub const TEMPLATE_MANIFEST_FILENAME: &str = "template.manifest.json";

/// Verification mode determines strictness of template validation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    Dev,
    Prod,
}

/// Centralized manifest verification policy.
/// This struct centralizes all verification parameters to avoid rebuilding
/// verification flags ad hoc throughout the codebase.
#[derive(Debug, Clone)]
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

impl<'a> ManifestPolicy<'a> {
    pub fn from_config(config: &'a crate::config::ServerConfig) -> Self {
        Self {
            mode: config.verification_mode(),
            expected_language: None,
            expected_release_channel: config.expected_release_channel(),
            allowed_firecracker_version: config.artifacts.allowed_firecracker_version.as_deref(),
            allowed_firecracker_binary_sha256: config
                .artifacts
                .allowed_firecracker_binary_sha256
                .as_deref(),
            require_hashes: config.artifacts.require_template_hashes,
            require_signatures: config.artifacts.require_template_signatures,
            keyring_path: config.artifacts.keyring_path.as_deref(),
        }
    }

    pub fn dev() -> Self {
        Self {
            mode: VerificationMode::Dev,
            expected_language: None,
            expected_release_channel: None,
            allowed_firecracker_version: None,
            allowed_firecracker_binary_sha256: None,
            require_hashes: false,
            require_signatures: false,
            keyring_path: None,
        }
    }

    /// Create a prod mode policy with default values for testing
    #[cfg(test)]
    pub fn prod_unchecked() -> Self {
        Self {
            mode: VerificationMode::Prod,
            expected_language: None,
            expected_release_channel: Some("prod"),
            allowed_firecracker_version: None,
            allowed_firecracker_binary_sha256: None,
            require_hashes: true,
            require_signatures: false, // Don't require for tests
            keyring_path: None,
        }
    }
}

/// Extended TemplateManifest with trust fields for production use
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TemplateManifest {
    // Core identity fields
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub build_id: Option<String>,
    #[serde(default)]
    pub artifact_set_id: Option<String>,

    // Trust and promotion
    #[serde(default)]
    pub promotion_channel: Option<String>, // dev | staging | prod
    #[serde(default)]
    pub signer_key_id: Option<String>,
    #[serde(default)]
    pub manifest_signature: Option<String>,
    #[serde(default)]
    pub manifest_signed_fields: Option<Vec<String>>,

    // Build provenance
    #[serde(default)]
    pub built_from_git_rev: Option<String>,
    #[serde(default)]
    pub build_host: Option<String>,
    #[serde(default)]
    pub firecracker_binary_sha256: Option<String>,

    // Original fields (kept for compatibility)
    #[serde(default)]
    pub language: Option<String>,
    pub kernel_path: String,
    #[serde(default)]
    pub kernel_sha256: Option<String>,
    pub rootfs_path: String,
    #[serde(default)]
    pub rootfs_sha256: Option<String>,
    pub init_path: String,
    pub mem_size_mib: u32,
    pub snapshot_state_path: String,
    pub snapshot_mem_path: String,
    pub snapshot_state_bytes: u64,
    pub snapshot_mem_bytes: u64,
    #[serde(default)]
    pub snapshot_state_sha256: Option<String>,
    #[serde(default)]
    pub snapshot_mem_sha256: Option<String>,
    #[serde(default)]
    pub firecracker_version: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub vcpu_count: Option<u32>,
    #[serde(default)]
    pub created_at_unix_ms: Option<u64>,
}

pub fn manifest_path_for(workdir: &Path) -> PathBuf {
    workdir.join(TEMPLATE_MANIFEST_FILENAME)
}

pub fn read_manifest(workdir: &Path) -> Result<TemplateManifest> {
    let manifest_path = manifest_path_for(workdir);
    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("missing template manifest {}", manifest_path.display()))?;
    serde_json::from_str::<TemplateManifest>(&raw)
        .with_context(|| format!("invalid template manifest {}", manifest_path.display()))
}

pub fn sha256_hex(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn verify_hash_if_present(
    path: &Path,
    expected: Option<&str>,
    require_hashes: bool,
    label: &str,
) -> Result<()> {
    match expected {
        Some(expected_hash) => {
            let actual = sha256_hex(path)?;
            if actual != expected_hash.to_ascii_lowercase() {
                bail!(
                    "{} sha256 mismatch for {}: expected {}, got {}",
                    label,
                    path.display(),
                    expected_hash,
                    actual
                );
            }
            Ok(())
        }
        None if require_hashes => bail!("template manifest missing {} sha256", label),
        None => Ok(()),
    }
}

pub fn resolve_path(workdir: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workdir.join(candidate)
    }
}

/// Resolve a path with confinement - ensures the resolved path stays within workdir.
/// This is critical for production use to prevent template artifacts from escaping.
pub fn resolve_path_confined(workdir: &Path, raw: &str) -> Result<PathBuf> {
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        workdir.join(raw)
    };

    let canon_workdir = workdir
        .canonicalize()
        .with_context(|| format!("canonicalize workdir {}", workdir.display()))?;
    let canon_joined = joined
        .canonicalize()
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

/// Verify template artifacts with enhanced security checks.
///
/// In Prod mode, this enforces:
/// - No missing schema_version
/// - promotion_channel must be "prod" (not "dev" or "staging")
/// - Signature verification when require_signatures is true
/// - Firecracker binary hash validation
/// - Path confinement (no escaping paths)
/// - Protocol version matching
/// - Firecracker version matching
#[allow(clippy::too_many_arguments)]
pub fn verify_template_artifacts(
    workdir: &Path,
    expected_language: Option<&str>,
    allowed_firecracker_version: Option<&str>,
    allowed_firecracker_binary_sha256: Option<&str>,
    require_hashes: bool,
    require_signatures: bool,
    mode: VerificationMode,
    keyring_path: Option<&Path>,
) -> Result<TemplateManifest> {
    // Build a temporary policy from individual parameters
    let policy = ManifestPolicy {
        mode,
        expected_language,
        expected_release_channel: None, // Not passed in this signature
        allowed_firecracker_version,
        allowed_firecracker_binary_sha256,
        require_hashes,
        require_signatures,
        keyring_path,
    };
    verify_template_artifacts_with_policy(workdir, &policy)
}

/// Verify template artifacts using a ManifestPolicy.
/// This is the preferred API - it centralizes all verification parameters.
pub fn verify_template_artifacts_with_policy(
    workdir: &Path,
    policy: &ManifestPolicy,
) -> Result<TemplateManifest> {
    let manifest = read_manifest(workdir)?;
    let mode = policy.mode;

    // === PRODUCTION MODE ENFORCEMENT ===
    if mode == VerificationMode::Prod {
        // 1. Require schema_version and check supported version
        match manifest.schema_version {
            Some(1) => {} // Version 1 is supported
            Some(unsupported) => {
                bail!(
                    "unsupported schema_version in prod mode: {}, only version 1 is supported",
                    unsupported
                );
            }
            None => {
                bail!("template manifest missing schema_version in prod mode");
            }
        }

        // 2. Require promotion_channel to match expected release channel from policy
        let expected_channel = policy.expected_release_channel.unwrap_or("prod");
        match manifest.promotion_channel.as_deref() {
            Some(actual) if actual == expected_channel => {}
            Some(actual) => bail!(
                "template not promoted to required channel: got '{}', expected '{}'",
                actual,
                expected_channel
            ),
            None => bail!("template manifest missing promotion_channel in prod mode"),
        }

        // 3. Require all core identity fields
        if manifest.template_id.is_none() {
            bail!("template manifest missing template_id in prod mode");
        }
        if manifest.build_id.is_none() {
            bail!("template manifest missing build_id in prod mode");
        }
        if manifest.artifact_set_id.is_none() {
            bail!("template manifest missing artifact_set_id in prod mode");
        }

        // 4. Require language and protocol version
        if manifest.language.is_none() {
            bail!("template manifest missing language in prod mode");
        }
        if manifest.protocol_version.is_none() {
            bail!("template manifest missing protocol_version in prod mode");
        }

        // 5. Require Firecracker version when pinned in policy
        if policy.allowed_firecracker_version.is_some() && manifest.firecracker_version.is_none() {
            bail!("template manifest missing firecracker_version in prod mode");
        }

        // 6. Require signatures if configured
        if manifest.manifest_signature.is_some()
            || manifest.signer_key_id.is_some()
            || manifest.manifest_signed_fields.is_some()
        {
            let signed_fields = manifest
                .manifest_signed_fields
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("signed template missing manifest_signed_fields"))?;
            signing::validate_manifest_signed_fields(signed_fields)?;
        }

        if policy.require_signatures {
            if manifest.manifest_signature.is_none() {
                bail!("prod mode requires template signatures but none present");
            }

            // Real signature verification
            let signer_key_id = manifest.signer_key_id.as_deref().ok_or_else(|| {
                anyhow::anyhow!("template has signature but missing signer_key_id")
            })?;
            let signature = manifest.manifest_signature.as_ref().unwrap();
            let signed_fields = manifest
                .manifest_signed_fields
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("template has signature but missing manifest_signed_fields"))?;
            signing::validate_manifest_signed_fields(signed_fields)?;

            // Serialize manifest to JSON for verification
            let manifest_json = serde_json::to_string(&manifest)?;

            // Load keyring if path provided
            let keyring = if let Some(path) = policy.keyring_path {
                Some(signing::load_keyring(path)?)
            } else {
                None
            };

            // Verify the signature
            signing::verify_manifest_signature(
                &manifest_json,
                signer_key_id,
                signature,
                keyring.as_ref(),
            )
            .map_err(|e| anyhow::anyhow!("signature verification failed: {}", e))?;
        }

        // 7. Validate Firecracker binary hash if configured
        if let Some(expected_fc_sha256) = policy.allowed_firecracker_binary_sha256 {
            match manifest.firecracker_binary_sha256.as_deref() {
                Some(actual_sha256)
                    if actual_sha256.to_lowercase() == expected_fc_sha256.to_lowercase() => {}
                Some(actual_sha256) => bail!(
                    "Firecracker binary sha256 mismatch: expected {}, got {}",
                    expected_fc_sha256,
                    actual_sha256
                ),
                None => bail!("template manifest missing firecracker_binary_sha256 in prod mode"),
            }
        }
    }

    // === EXISTING VALIDATION LOGIC ===

    if let Some(expected) = policy.expected_language {
        match manifest.language.as_deref() {
            Some(actual) if actual == expected => {}
            Some(actual) => bail!(
                "template language mismatch: expected {}, got {}",
                expected,
                actual
            ),
            None => bail!("template manifest missing language"),
        }
    }

    if let Some(expected_fc) = policy.allowed_firecracker_version {
        match manifest.firecracker_version.as_deref() {
            Some(actual) if actual == expected_fc => {}
            Some(actual) => bail!(
                "template Firecracker version mismatch: expected {}, got {}",
                expected_fc,
                actual
            ),
            None => bail!("template manifest missing firecracker_version"),
        }
    }

    if let Some(proto) = manifest.protocol_version.as_deref() {
        if proto != protocol::PROTOCOL_VERSION {
            bail!(
                "template protocol version mismatch: expected {}, got {}",
                protocol::PROTOCOL_VERSION,
                proto
            );
        }
    } else if policy.require_hashes {
        bail!("template manifest missing protocol_version");
    }

    // Use confined path resolution in prod mode
    let state_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.snapshot_state_path)?
    } else {
        resolve_path(workdir, &manifest.snapshot_state_path)
    };
    let mem_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.snapshot_mem_path)?
    } else {
        resolve_path(workdir, &manifest.snapshot_mem_path)
    };

    let state_meta = std::fs::metadata(&state_path)
        .with_context(|| format!("missing snapshot state file {}", state_path.display()))?;
    let mem_meta = std::fs::metadata(&mem_path)
        .with_context(|| format!("missing snapshot memory file {}", mem_path.display()))?;

    if state_meta.len() == 0 {
        bail!("snapshot state file is empty: {}", state_path.display());
    }
    if mem_meta.len() == 0 {
        bail!("snapshot memory file is empty: {}", mem_path.display());
    }
    if manifest.snapshot_state_bytes != 0 && state_meta.len() != manifest.snapshot_state_bytes {
        bail!(
            "template manifest state size mismatch: expected {}, got {}",
            manifest.snapshot_state_bytes,
            state_meta.len()
        );
    }
    if manifest.snapshot_mem_bytes != 0 && mem_meta.len() != manifest.snapshot_mem_bytes {
        bail!(
            "template manifest memory size mismatch: expected {}, got {}",
            manifest.snapshot_mem_bytes,
            mem_meta.len()
        );
    }

    verify_hash_if_present(
        &state_path,
        manifest.snapshot_state_sha256.as_deref(),
        policy.require_hashes,
        "snapshot state",
    )?;
    verify_hash_if_present(
        &mem_path,
        manifest.snapshot_mem_sha256.as_deref(),
        policy.require_hashes,
        "snapshot memory",
    )?;

    // Use confined path resolution for kernel and rootfs in prod mode
    let kernel_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.kernel_path)?
    } else {
        resolve_path(workdir, &manifest.kernel_path)
    };
    verify_hash_if_present(
        &kernel_path,
        manifest.kernel_sha256.as_deref(),
        policy.require_hashes,
        "kernel",
    )?;

    let rootfs_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.rootfs_path)?
    } else {
        resolve_path(workdir, &manifest.rootfs_path)
    };
    verify_hash_if_present(
        &rootfs_path,
        manifest.rootfs_sha256.as_deref(),
        policy.require_hashes,
        "rootfs",
    )?;

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_manifest(
        workdir: &Path,
        promotion_channel: &str,
        schema_version: Option<u32>,
    ) -> Result<PathBuf> {
        let mut manifest = serde_json::json!({
            "template_id": "tpl-test",
            "build_id": "build-test",
            "artifact_set_id": "artifact-test",
            "promotion_channel": promotion_channel,
            "language": "python",
            "kernel_path": "vmlinux",
            "rootfs_path": "rootfs.ext4",
            "init_path": "/init",
            "mem_size_mib": 512,
            "snapshot_mem_path": "snapshot.mem",
            "snapshot_state_path": "snapshot.vmstate",
            "snapshot_mem_bytes": 0,
            "snapshot_state_bytes": 0,
            "protocol_version": "ZB1",
        });
        if let Some(v) = schema_version {
            manifest["schema_version"] = serde_json::json!(v);
        }
        let manifest_path = workdir.join("template.manifest.json");
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
        Ok(manifest_path)
    }

    #[test]
    fn test_prod_rejects_missing_schema_version() {
        let td = TempDir::new().unwrap();
        create_test_manifest(td.path(), "prod", None).unwrap();

        let result = verify_template_artifacts(
            td.path(),
            None,
            None,
            None,
            false,
            false,
            VerificationMode::Prod,
            None,
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("schema_version"),
            "expected schema_version error: {}",
            err
        );
    }

    #[test]
    fn test_prod_rejects_wrong_promotion_channel() {
        let td = TempDir::new().unwrap();
        create_test_manifest(td.path(), "dev", Some(1)).unwrap();

        let result = verify_template_artifacts(
            td.path(),
            None,
            None,
            None,
            false,
            false,
            VerificationMode::Prod,
            None,
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("promotion_channel") || err.contains("required channel"),
            "expected promotion_channel error: {}",
            err
        );
    }

    #[test]
    fn test_dev_accepts_missing_schema() {
        let td = TempDir::new().unwrap();
        create_test_manifest(td.path(), "dev", None).unwrap();

        // Dev mode should not require schema_version
        let result = verify_template_artifacts(
            td.path(),
            None,
            None,
            None,
            false,
            false,
            VerificationMode::Dev,
            None,
        );

        // Should pass manifest validation (will fail on missing files, not schema)
        // We're just checking schema isn't required
        assert!(result.is_err()); // Missing files, but NOT schema error
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("schema_version"),
            "dev mode should not require schema: {}",
            err
        );
    }

    #[test]
    fn test_prod_accepts_valid_manifest() {
        let td = TempDir::new().unwrap();
        create_test_manifest(td.path(), "prod", Some(1)).unwrap();

        // Create dummy files
        fs::write(td.path().join("vmlinux"), "kernel").unwrap();
        fs::write(td.path().join("rootfs.ext4"), "rootfs").unwrap();
        fs::write(td.path().join("snapshot.mem"), "mem").unwrap();
        fs::write(td.path().join("snapshot.vmstate"), "state").unwrap();

        let result = verify_template_artifacts(
            td.path(),
            None,
            None,
            None,
            false,
            false,
            VerificationMode::Prod,
            None,
        );

        // Should succeed with valid manifest and present files
        assert!(
            result.is_ok(),
            "valid prod manifest should pass: {:?}",
            result
        );
    }
}

#[cfg(test)]
mod strict_enforcement_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_prod_rejects_missing_template_id() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path();

        // Create manifest with missing template_id
        let mut manifest = TemplateManifest::default();
        manifest.schema_version = Some(1);
        manifest.promotion_channel = Some("prod".to_string());
        manifest.template_id = None; // Missing in prod

        let path = workdir.join("template.manifest.json");
        let f = std::fs::File::create(&path).unwrap();
        serde_json::to_writer(f, &manifest).unwrap();

        // Also need snapshot files for verification to proceed
        std::fs::write(workdir.join("snapshot.state"), "test").unwrap();
        std::fs::write(workdir.join("snapshot.mem"), "test").unwrap();

        let policy = ManifestPolicy::prod_unchecked();
        let result = verify_template_artifacts_with_policy(workdir, &policy);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("template_id"),
            "expected template_id error: {}",
            err
        );
    }

    #[test]
    fn test_prod_rejects_path_escape() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path();

        // Create manifest with path escape attempt
        let mut manifest = TemplateManifest::default();
        manifest.schema_version = Some(1);
        manifest.promotion_channel = Some("prod".to_string());
        manifest.template_id = Some("test".to_string());
        manifest.build_id = Some("build".to_string());
        manifest.artifact_set_id = Some("artifact".to_string());
        manifest.language = Some("python".to_string());
        manifest.kernel_path = "kernel".to_string();
        manifest.rootfs_path = "rootfs".to_string();
        manifest.init_path = "/init".to_string();
        manifest.protocol_version = Some("ZB1".to_string());
        manifest.snapshot_state_path = "../../../etc/passwd".to_string();
        manifest.snapshot_mem_path = "mem".to_string();
        manifest.snapshot_state_bytes = 4;
        manifest.snapshot_mem_bytes = 4;

        let path = workdir.join("template.manifest.json");
        let f = std::fs::File::create(&path).unwrap();
        serde_json::to_writer(f, &manifest).unwrap();

        // Create dummy files
        std::fs::write(workdir.join("kernel"), "test").unwrap();
        std::fs::write(workdir.join("rootfs"), "test").unwrap();
        std::fs::write(workdir.join("mem"), "test").unwrap();

        let policy = ManifestPolicy::prod_unchecked();
        let result = verify_template_artifacts_with_policy(workdir, &policy);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("confined")
                || err.contains("outside")
                || err.contains("canonicalize template path"),
            "expected path escape error: {}",
            err
        );
    }

    #[test]
    fn test_prod_rejects_unsupported_schema_version() {
        let tmp = TempDir::new().unwrap();
        let workdir = tmp.path();

        // Create manifest with unsupported schema version
        let mut manifest = TemplateManifest::default();
        manifest.schema_version = Some(99); // Unsupported
        manifest.promotion_channel = Some("prod".to_string());
        manifest.template_id = Some("test".to_string());
        manifest.build_id = Some("build".to_string());
        manifest.artifact_set_id = Some("artifact".to_string());
        manifest.language = Some("python".to_string());
        manifest.kernel_path = "kernel".to_string();
        manifest.rootfs_path = "rootfs".to_string();
        manifest.init_path = "/init".to_string();
        manifest.protocol_version = Some("ZB1".to_string());

        let path = workdir.join("template.manifest.json");
        let f = std::fs::File::create(&path).unwrap();
        serde_json::to_writer(f, &manifest).unwrap();

        // Also need snapshot files
        std::fs::write(workdir.join("kernel"), "test").unwrap();
        std::fs::write(workdir.join("rootfs"), "test").unwrap();
        std::fs::write(workdir.join("snapshot.state"), "test").unwrap();
        std::fs::write(workdir.join("snapshot.mem"), "test").unwrap();

        let policy = ManifestPolicy::prod_unchecked();
        let result = verify_template_artifacts_with_policy(workdir, &policy);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("schema_version"),
            "expected schema_version error: {}",
            err
        );
    }
}

#[cfg(test)]
mod signature_enforcement_tests {
    use super::*;
    use crate::signing;
    use tempfile::TempDir;

    fn write_template_files(workdir: &Path) -> Result<()> {
        std::fs::write(workdir.join("kernel"), b"kernel-bytes")?;
        std::fs::write(workdir.join("rootfs.ext4"), b"rootfs-bytes")?;
        std::fs::write(workdir.join("snapshot.state"), b"state-bytes")?;
        std::fs::write(workdir.join("snapshot.mem"), b"mem-bytes")?;
        Ok(())
    }

    fn create_signed_manifest(
        workdir: &Path,
        channel: &str,
        sign_manifest: bool,
    ) -> Result<Option<PathBuf>> {
        write_template_files(workdir)?;
        let kernel_path = workdir.join("kernel");
        let rootfs_path = workdir.join("rootfs.ext4");
        let state_path = workdir.join("snapshot.state");
        let mem_path = workdir.join("snapshot.mem");

        let mut manifest = serde_json::json!({
            "schema_version": 1,
            "template_id": "tpl-prod",
            "build_id": "build-prod",
            "artifact_set_id": "artifact-prod",
            "promotion_channel": channel,
            "language": "python",
            "kernel_path": "kernel",
            "kernel_sha256": sha256_hex(&kernel_path)?,
            "rootfs_path": "rootfs.ext4",
            "rootfs_sha256": sha256_hex(&rootfs_path)?,
            "init_path": "/init",
            "mem_size_mib": 512,
            "snapshot_state_path": "snapshot.state",
            "snapshot_state_bytes": std::fs::metadata(&state_path)?.len(),
            "snapshot_state_sha256": sha256_hex(&state_path)?,
            "snapshot_mem_path": "snapshot.mem",
            "snapshot_mem_bytes": std::fs::metadata(&mem_path)?.len(),
            "snapshot_mem_sha256": sha256_hex(&mem_path)?,
            "firecracker_version": "1.12.0",
            "firecracker_binary_sha256": "fc-sha",
            "protocol_version": "ZB1",
            "vcpu_count": 1,
            "created_at_unix_ms": 1234567890u64,
            "signer_key_id": null,
            "manifest_signature": null,
            "manifest_signed_fields": null
        });

        let mut keyring_path = None;
        if sign_manifest {
            let (private_key, public_key) = signing::generate_key_pair()?;
            let key_id = signing::get_key_id(&public_key);
            manifest["signer_key_id"] = serde_json::json!(key_id.clone());
            manifest["manifest_signed_fields"] =
                serde_json::json!(signing::required_manifest_signed_fields_vec());
            let prepared = serde_json::to_string(&manifest)?;
            let (signature, _) =
                signing::sign_manifest_with_required_fields(&private_key, &prepared)?;
            manifest["manifest_signature"] = serde_json::json!(signature);

            let keyring = serde_json::json!({
                "keys": [{
                    "key_id": key_id,
                    "algorithm": signing::SIG_ALGORITHM_ED25519,
                    "public_key": signing::format_public_key_base64(&public_key),
                    "enabled": true,
                    "description": "test signer"
                }]
            });
            let path = workdir.join("keyring.json");
            std::fs::write(&path, serde_json::to_vec_pretty(&keyring)?)?;
            keyring_path = Some(path);
        }

        std::fs::write(
            workdir.join(TEMPLATE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(keyring_path)
    }

    fn prod_policy<'a>(keyring_path: Option<&'a Path>) -> ManifestPolicy<'a> {
        ManifestPolicy {
            mode: VerificationMode::Prod,
            expected_language: Some("python"),
            expected_release_channel: Some("prod"),
            allowed_firecracker_version: Some("1.12.0"),
            allowed_firecracker_binary_sha256: Some("fc-sha"),
            require_hashes: true,
            require_signatures: true,
            keyring_path,
        }
    }

    #[test]
    fn test_unsigned_dev_template_rejected_in_prod() {
        let tmp = TempDir::new().unwrap();
        create_signed_manifest(tmp.path(), "dev", false).unwrap();
        let result = verify_template_artifacts_with_policy(tmp.path(), &prod_policy(None));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("required channel") || err.contains("promotion_channel"),
            "expected dev-channel rejection, got: {}",
            err
        );
    }

    #[test]
    fn test_signed_wrong_channel_template_rejected_in_prod() {
        let tmp = TempDir::new().unwrap();
        let keyring = create_signed_manifest(tmp.path(), "staging", true).unwrap();
        let result =
            verify_template_artifacts_with_policy(tmp.path(), &prod_policy(keyring.as_deref()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("required channel") || err.contains("promotion_channel"),
            "expected signed wrong-channel rejection, got: {}",
            err
        );
    }

    #[test]
    fn test_signed_prod_template_accepted_with_matching_keyring() {
        let tmp = TempDir::new().unwrap();
        let keyring = create_signed_manifest(tmp.path(), "prod", true).unwrap();
        let result =
            verify_template_artifacts_with_policy(tmp.path(), &prod_policy(keyring.as_deref()));
        assert!(result.is_ok(), "expected signed prod template to pass: {:?}", result);
    }

    #[test]
    fn test_modified_signed_manifest_rejected() {
        let tmp = TempDir::new().unwrap();
        let keyring = create_signed_manifest(tmp.path(), "prod", true).unwrap();
        let manifest_path = tmp.path().join(TEMPLATE_MANIFEST_FILENAME);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["mem_size_mib"] = serde_json::json!(1024);
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let result =
            verify_template_artifacts_with_policy(tmp.path(), &prod_policy(keyring.as_deref()));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("signature verification failed") || err.contains("manifest_signed_fields"),
            "expected signature verification failure, got: {}",
            err
        );
    }
}
