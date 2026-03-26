use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use crate::protocol;

pub const TEMPLATE_MANIFEST_FILENAME: &str = "template.manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TemplateManifest {
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

pub fn verify_template_artifacts(
    workdir: &Path,
    expected_language: Option<&str>,
    allowed_firecracker_version: Option<&str>,
    require_hashes: bool,
) -> Result<TemplateManifest> {
    let manifest = read_manifest(workdir)?;

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

    let state_path = resolve_path(workdir, &manifest.snapshot_state_path);
    let mem_path = resolve_path(workdir, &manifest.snapshot_mem_path);
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

    let kernel_path = resolve_path(workdir, &manifest.kernel_path);
    if kernel_path.exists() || manifest.kernel_sha256.is_some() {
        verify_hash_if_present(
            &kernel_path,
            manifest.kernel_sha256.as_deref(),
            require_hashes,
            "kernel",
        )?;
    }

    let rootfs_path = resolve_path(workdir, &manifest.rootfs_path);
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
