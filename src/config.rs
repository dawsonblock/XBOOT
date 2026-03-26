use anyhow::{bail, Result};
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    Dev,
    Prod,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_request_body_bytes: usize,
    pub max_code_bytes: usize,
    pub max_stdin_bytes: usize,
    pub max_batch_size: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_total_output_bytes: usize,
    pub max_timeout_secs: u64,
    pub max_concurrent_requests: usize,
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub path: PathBuf,
    pub log_code: bool,
}

#[derive(Debug, Clone)]
pub struct HealthConfig {
    pub probe_timeout_secs: u64,
    pub cache_ttl_secs: u64,
}

#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub wait_timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ArtifactConfig {
    pub require_template_hashes: bool,
    pub allowed_firecracker_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub auth_mode: AuthMode,
    pub api_keys_file: PathBuf,
    pub trusted_proxies: Vec<IpAddr>,
    pub limits: Limits,
    pub logging: LoggingConfig,
    pub health: HealthConfig,
    pub bind_addr: String,
    pub queue: QueueConfig,
    pub artifacts: ArtifactConfig,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self> {
        let auth_mode_prod = match env::var("ZEROBOOT_AUTH_MODE").unwrap_or_else(|_| "dev".into()).to_lowercase().as_str() {
            "dev" => false,
            "prod" | "production" => true,
            other => bail!("unsupported ZEROBOOT_AUTH_MODE: {}", other),
        };
        let auth_mode = if auth_mode_prod { AuthMode::Prod } else { AuthMode::Dev };
        let trusted_proxies = env::var("ZEROBOOT_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() { None } else { s.parse().ok() }
            })
            .collect();
        Ok(Self {
            auth_mode,
            api_keys_file: env::var("ZEROBOOT_API_KEYS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("api_keys.json")),
            trusted_proxies,
            limits: Limits {
                max_request_body_bytes: usize_env("ZEROBOOT_MAX_REQUEST_BODY_BYTES", 256 * 1024),
                max_code_bytes: usize_env("ZEROBOOT_MAX_CODE_BYTES", 128 * 1024),
                max_stdin_bytes: usize_env("ZEROBOOT_MAX_STDIN_BYTES", 64 * 1024),
                max_batch_size: usize_env("ZEROBOOT_MAX_BATCH_SIZE", 16),
                max_stdout_bytes: usize_env("ZEROBOOT_MAX_STDOUT_BYTES", 64 * 1024),
                max_stderr_bytes: usize_env("ZEROBOOT_MAX_STDERR_BYTES", 64 * 1024),
                max_total_output_bytes: usize_env("ZEROBOOT_MAX_TOTAL_OUTPUT_BYTES", 96 * 1024),
                max_timeout_secs: u64_env("ZEROBOOT_MAX_TIMEOUT_SECS", 30),
                max_concurrent_requests: usize_env("ZEROBOOT_MAX_CONCURRENT_REQUESTS", 32),
            },
            logging: LoggingConfig {
                path: env::var("ZEROBOOT_REQUEST_LOG_PATH")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/var/lib/zeroboot/requests.jsonl")),
                log_code: bool_env("ZEROBOOT_LOG_CODE", false),
            },
            health: HealthConfig {
                probe_timeout_secs: u64_env("ZEROBOOT_HEALTH_PROBE_TIMEOUT_SECS", 2),
                cache_ttl_secs: u64_env("ZEROBOOT_HEALTH_CACHE_TTL_SECS", 10),
            },
            bind_addr: env::var("ZEROBOOT_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".into()),
            queue: QueueConfig {
                wait_timeout_ms: u64_env("ZEROBOOT_QUEUE_WAIT_TIMEOUT_MS", 250),
            },
            artifacts: ArtifactConfig {
                require_template_hashes: bool_env("ZEROBOOT_REQUIRE_TEMPLATE_HASHES", auth_mode_prod),
                allowed_firecracker_version: env::var("ZEROBOOT_ALLOWED_FIRECRACKER_VERSION").ok().filter(|s| !s.trim().is_empty()),
            },
        })
    }

    pub fn is_trusted_proxy(&self, addr: IpAddr) -> bool {
        self.trusted_proxies.contains(&addr)
    }
}

fn usize_env(name: &str, default: usize) -> usize {
    env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn u64_env(name: &str, default: u64) -> u64 {
    env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn bool_env(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_proxy_match_is_exact() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Dev,
            api_keys_file: PathBuf::from("api_keys.json"),
            trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
            limits: Limits {
                max_request_body_bytes: 1,
                max_code_bytes: 1,
                max_stdin_bytes: 1,
                max_batch_size: 1,
                max_stdout_bytes: 1,
                max_stderr_bytes: 1,
                max_total_output_bytes: 1,
                max_timeout_secs: 1,
                max_concurrent_requests: 1,
            },
            logging: LoggingConfig { path: PathBuf::from("x"), log_code: false },
            health: HealthConfig { probe_timeout_secs: 1, cache_ttl_secs: 1 },
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig { wait_timeout_ms: 1 },
            artifacts: ArtifactConfig { require_template_hashes: false, allowed_firecracker_version: None },
        };
        assert!(cfg.is_trusted_proxy("127.0.0.1".parse().unwrap()));
        assert!(!cfg.is_trusted_proxy("10.0.0.1".parse().unwrap()));
    }
}
