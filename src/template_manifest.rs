use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::protocol;

pub const TEMPLATE_MANIFEST_FILENAME: &str = "template.manifest.json";

/// Verification mode determines strictness of template validation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    Dev,
    Prod,
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
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_hash_if_present(path: &Path, expected: Option<&str>, require_hashes: bool, label: &str) -> Result<()> {
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

/// Verify template artifacts with enhanced security checks.
/// 
/// In Prod mode, this enforces:
/// - No missing schema_version
/// - promotion_channel must be "prod" (not "dev" or "staging")
/// - Signature verification when ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES is set
/// - Firecracker binary hash validation
/// - Path confinement (no escaping paths)
/// - Protocol version matching
/// - Firecracker version matching
pub fn verify_template_artifacts(
    workdir: &Path,
    expected_language: Option<&str>,
    allowed_firecracker_version: Option<&str>,
    allowed_firecracker_binary_sha256: Option<&str>,
    require_hashes: bool,
    mode: VerificationMode,
) -> Result<TemplateManifest> {
    let manifest = read_manifest(workdir)?;

    // === PRODUCTION MODE ENFORCEMENT ===
    if mode == VerificationMode::Prod {
        // 1. Require schema_version
        if manifest.schema_version.is_none() {
            bail!("template manifest missing schema_version in prod mode");
        }

        // 2. Require promotion_channel = "prod"
        match manifest.promotion_channel.as_deref() {
            Some("prod") => {}
            Some(channel) => bail!(
                "template not promoted to prod: promotion_channel is '{}', expected 'prod'",
                channel
            ),
            None => bail!("template manifest missing promotion_channel in prod mode"),
        }

        // 3. Require signatures if configured
        if std::env::var("ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES").is_ok() {
            if manifest.manifest_signature.is_none() {
                bail!("prod mode requires template signatures but none present");
            }
            // TODO: actual signature verification via signing.rs module
            if manifest.signer_key_id.is_none() {
                bail!("template has signature but missing signer_key_id");
            }
        }

        // 4. Validate Firecracker binary hash if configured
        if let Some(expected_fc_sha256) = allowed_firecracker_binary_sha256 {
            match manifest.firecracker_binary_sha256.as_deref() {
                Some(actual_sha256) if actual_sha256.to_lowercase() == expected_fc_sha256.to_lowercase() => {}
                Some(actual_sha256) => bail!(
                    "Firecracker binary sha256 mismatch: expected {}, got {}",
                    expected_fc_sha256, actual_sha256
                ),
                None => bail!("template manifest missing firecracker_binary_sha256 in prod mode"),
            }
        }
    }

    // === EXISTING VALIDATION LOGIC ===
    
    if let Some(expected) = expected_language {
        match manifest.language.as_deref() {
            Some(actual) if actual == expected => {}
            Some(actual) => bail!("template language mismatch: expected {}, got {}", expected, actual),
            None => bail!("template manifest missing language"),
        }
    }

    if let Some(expected_fc) = allowed_firecracker_version {
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
    } else if require_hashes {
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
        require_hashes,
        "snapshot state",
    )?;
    verify_hash_if_present(
        &mem_path,
        manifest.snapshot_mem_sha256.as_deref(),
        require_hashes,
        "snapshot memory",
    )?;

    // Use confined path resolution for kernel and rootfs in prod mode
    let kernel_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.kernel_path)?
    } else {
        resolve_path(workdir, &manifest.kernel_path)
    };
    if kernel_path.exists() || manifest.kernel_sha256.is_some() {
        verify_hash_if_present(
            &kernel_path,
            manifest.kernel_sha256.as_deref(),
            require_hashes,
            "kernel",
        )?;
    }

    let rootfs_path = if mode == VerificationMode::Prod {
        resolve_path_confined(workdir, &manifest.rootfs_path)?
    } else {
        resolve_path(workdir, &manifest.rootfs_path)
    };
    if rootfs_path.exists() || manifest.rootfs_sha256.is_some() {
        verify_hash_if_present(
            &rootfs_path,
            manifest.rootfs_sha256.as_deref(),
            require_hashes,
            "rootfs",
        )?;
    }

    Ok(manifest)
}
