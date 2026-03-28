use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{AuthMode, ServerConfig};
use crate::template_manifest::{self, ManifestPolicy};

#[derive(Debug, Clone)]
pub struct ParsedTemplateSpec {
    pub language: String,
    pub workdir: PathBuf,
}

pub fn parse_template_specs(
    spec: &str,
    release_root: Option<&Path>,
) -> Result<Vec<ParsedTemplateSpec>> {
    let mut out = Vec::new();
    for raw_spec in spec.split(',') {
        let raw_spec = raw_spec.trim();
        if raw_spec.is_empty() {
            continue;
        }
        let (language, raw_path) = if let Some((lang, path)) = raw_spec.split_once(':') {
            (lang.trim().to_string(), path.trim())
        } else {
            ("python".to_string(), raw_spec)
        };
        if raw_path.is_empty() {
            bail!("template spec for '{}' is missing a path", language);
        }
        let mut workdir = PathBuf::from(raw_path);
        if workdir.is_relative() {
            if let Some(root) = release_root {
                workdir = root.join(workdir);
            } else {
                workdir = std::env::current_dir()?.join(workdir);
            }
        }
        out.push(ParsedTemplateSpec { language, workdir });
    }
    if out.is_empty() {
        bail!("no template specs provided");
    }
    Ok(out)
}

pub fn verify_startup(
    config: &ServerConfig,
    specs: &[ParsedTemplateSpec],
    release_root: Option<&Path>,
) -> Result<()> {
    config.validate_startup()?;
    verify_release_root(release_root)?;
    verify_template_roots(specs)?;
    verify_auth_and_logging_paths(config)?;
    verify_kvm()?;
    verify_cgroup_mode()?;
    verify_firecracker_binary(config)?;

    let mut policy = ManifestPolicy::from_config(config);
    for spec in specs {
        policy.expected_language = Some(&spec.language);
        template_manifest::verify_template_artifacts_with_policy(&spec.workdir, &policy)
            .with_context(|| {
                format!(
                    "template '{}' failed verification at {}",
                    spec.language,
                    spec.workdir.display()
                )
            })?;
    }

    ensure_disk_watermarks(config, specs)?;
    Ok(())
}

pub fn runtime_admission_paths(
    config: &ServerConfig,
    specs: &[ParsedTemplateSpec],
) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = specs.iter().map(|spec| spec.workdir.clone()).collect();
    if let Some(parent) = config.logging.path.parent() {
        paths.push(parent.to_path_buf());
    }
    paths.sort();
    paths.dedup();
    paths
}

pub fn ensure_runtime_admission(config: &ServerConfig, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    ensure_disk_watermarks_for_paths(config, paths)
}

fn verify_release_root(release_root: Option<&Path>) -> Result<()> {
    if let Some(root) = release_root {
        if !root.is_dir() {
            bail!(
                "release root does not exist or is not a directory: {}",
                root.display()
            );
        }
    }
    Ok(())
}

fn verify_template_roots(specs: &[ParsedTemplateSpec]) -> Result<()> {
    for spec in specs {
        if !spec.workdir.is_dir() {
            bail!(
                "template workdir for '{}' does not exist or is not a directory: {}",
                spec.language,
                spec.workdir.display()
            );
        }
    }
    Ok(())
}

fn verify_readable_file(path: &Path, label: &str) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("{} path is not accessible: {}", label, path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file: {}", label, path.display());
    }
    std::fs::File::open(path)
        .with_context(|| format!("{} is not readable: {}", label, path.display()))?;
    Ok(())
}

fn verify_writable_directory(path: &Path, label: &str) -> Result<()> {
    if !path.is_dir() {
        bail!("{} is not a directory: {}", label, path.display());
    }
    let probe = tempfile::NamedTempFile::new_in(path)
        .with_context(|| format!("{} is not writable: {}", label, path.display()))?;
    drop(probe);
    Ok(())
}

fn verify_auth_and_logging_paths(config: &ServerConfig) -> Result<()> {
    if matches!(config.auth_mode, AuthMode::Prod) {
        verify_readable_file(&config.api_keys_file, "api keys file")?;
        verify_readable_file(&config.api_key_pepper_file, "api key pepper file")?;
    }

    if config.artifacts.require_template_signatures {
        let keyring_path = config.artifacts.keyring_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!("signature verification requires ZEROBOOT_KEYRING_PATH")
        })?;
        verify_readable_file(keyring_path, "keyring file")?;
    }

    let log_dir = config
        .logging
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("request log path must have a parent directory"))?;
    if !log_dir.exists() {
        std::fs::create_dir_all(log_dir)
            .with_context(|| format!("create request log directory {}", log_dir.display()))?;
    }
    verify_writable_directory(log_dir, "request log directory")?;

    Ok(())
}

fn verify_kvm() -> Result<()> {
    let kvm = Path::new("/dev/kvm");
    if !kvm.exists() {
        bail!("/dev/kvm is missing");
    }
    let metadata = std::fs::metadata(kvm).context("stat /dev/kvm")?;
    if metadata.permissions().readonly() {
        bail!("/dev/kvm is not writable by the current user");
    }
    Ok(())
}

fn verify_cgroup_mode() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let cgroup_v2 = Path::new("/sys/fs/cgroup/cgroup.controllers");
        if !cgroup_v2.exists() {
            bail!("unsupported cgroup mode: expected unified cgroup v2");
        }
    }
    Ok(())
}

pub fn resolve_firecracker_binary() -> PathBuf {
    env::var("ZEROBOOT_FIRECRACKER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("firecracker"))
}

fn verify_firecracker_binary(config: &ServerConfig) -> Result<()> {
    let binary = resolve_firecracker_binary();
    let resolved_binary = resolve_executable(&binary)?;
    let version_output = Command::new(&resolved_binary)
        .arg("--version")
        .output()
        .with_context(|| format!("run {} --version", resolved_binary.display()))?;
    if !version_output.status.success() {
        bail!(
            "failed to query Firecracker version from {}",
            resolved_binary.display()
        );
    }
    let version = String::from_utf8_lossy(&version_output.stdout)
        .trim()
        .to_string();
    if let Some(expected) = config.artifacts.allowed_firecracker_version.as_deref() {
        if version != expected {
            bail!(
                "Firecracker version mismatch: expected '{}', got '{}'",
                expected,
                version
            );
        }
    }

    if let Some(expected_hash) = config
        .artifacts
        .allowed_firecracker_binary_sha256
        .as_deref()
    {
        let actual_hash = template_manifest::sha256_hex(&resolved_binary)
            .with_context(|| format!("sha256 {}", resolved_binary.display()))?;
        if actual_hash != expected_hash.to_ascii_lowercase() {
            bail!(
                "Firecracker binary sha256 mismatch: expected {}, got {}",
                expected_hash,
                actual_hash
            );
        }
    }

    Ok(())
}

fn resolve_executable(binary: &Path) -> Result<PathBuf> {
    if binary.components().count() > 1 || binary.is_absolute() {
        return Ok(binary.to_path_buf());
    }

    let path_env = env::var_os("PATH").ok_or_else(|| anyhow::anyhow!("PATH is not set"))?;
    for dir in env::split_paths(&path_env) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    bail!("executable not found in PATH: {}", binary.display());
}

fn ensure_disk_watermarks(config: &ServerConfig, specs: &[ParsedTemplateSpec]) -> Result<()> {
    let paths = runtime_admission_paths(config, specs);
    ensure_disk_watermarks_for_paths(config, &paths)
}

fn ensure_disk_watermarks_for_paths(config: &ServerConfig, paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        let target = if path.is_dir() {
            path.as_path()
        } else {
            path.parent().unwrap_or(Path::new("/"))
        };
        let stats = statvfs(target)?;
        if stats.free_bytes < config.storage.min_free_bytes {
            bail!(
                "disk watermark violated at {}: free bytes {} < required {}",
                target.display(),
                stats.free_bytes,
                config.storage.min_free_bytes
            );
        }
        if stats.free_inodes < config.storage.min_free_inodes {
            bail!(
                "inode watermark violated at {}: free inodes {} < required {}",
                target.display(),
                stats.free_inodes,
                config.storage.min_free_inodes
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct FsStats {
    free_bytes: u64,
    free_inodes: u64,
}

fn statvfs(path: &Path) -> Result<FsStats> {
    let path_c = CString::new(path.as_os_str().as_bytes().to_vec())
        .with_context(|| format!("path contains NUL byte: {}", path.display()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(path_c.as_ptr(), &mut stat) };
    if rc != 0 {
        bail!(
            "statvfs failed for {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(FsStats {
        free_bytes: (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64),
        free_inodes: stat.f_favail as u64,
    })
}
