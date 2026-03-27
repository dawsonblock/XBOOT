use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;

use crate::template_manifest::VerificationMode;

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
    pub allowed_firecracker_binary_sha256: Option<String>,
    pub release_channel: Option<String>,
    pub require_template_signatures: bool,
    pub keyring_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PoolConfig {
    pub min_idle_per_lang: usize,
    pub max_idle_per_lang: usize,
    pub borrow_timeout_ms: u64,
    pub health_check_interval_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub auth_mode: AuthMode,
    pub api_keys_file: PathBuf,
    pub api_key_pepper_file: PathBuf,
    pub trusted_proxies: Vec<IpAddr>,
    pub limits: Limits,
    pub logging: LoggingConfig,
    pub health: HealthConfig,
    pub bind_addr: String,
    pub queue: QueueConfig,
    pub artifacts: ArtifactConfig,
    #[allow(dead_code)]
    pub pool: PoolConfig,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self> {
        let auth_mode_prod = match env::var("ZEROBOOT_AUTH_MODE")
            .unwrap_or_else(|_| "dev".into())
            .to_lowercase()
            .as_str()
        {
            "dev" => false,
            "prod" | "production" => true,
            other => bail!("unsupported ZEROBOOT_AUTH_MODE: {}", other),
        };
        let auth_mode = if auth_mode_prod {
            AuthMode::Prod
        } else {
            AuthMode::Dev
        };
        let trusted_proxies = env::var("ZEROBOOT_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    s.parse().ok()
                }
            })
            .collect();
        Ok(Self {
            auth_mode,
            api_keys_file: env::var("ZEROBOOT_API_KEYS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("api_keys.json")),
            api_key_pepper_file: env::var("ZEROBOOT_API_KEY_PEPPER_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/etc/zeroboot/pepper")),
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
                require_template_hashes: bool_env(
                    "ZEROBOOT_REQUIRE_TEMPLATE_HASHES",
                    auth_mode_prod,
                ),
                allowed_firecracker_version: env::var("ZEROBOOT_ALLOWED_FIRECRACKER_VERSION")
                    .ok()
                    .filter(|s| !s.trim().is_empty()),
                allowed_firecracker_binary_sha256: env::var("ZEROBOOT_ALLOWED_FC_BINARY_SHA256")
                    .ok()
                    .filter(|s| !s.trim().is_empty()),
                release_channel: env::var("ZEROBOOT_RELEASE_CHANNEL")
                    .ok()
                    .filter(|s| !s.trim().is_empty()),
                require_template_signatures: bool_env(
                    "ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES",
                    auth_mode_prod,
                ),
                keyring_path: env::var("ZEROBOOT_KEYRING_PATH")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from),
            },
            pool: PoolConfig {
                min_idle_per_lang: usize_env("ZEROBOOT_POOL_MIN_PER_LANG", 0),
                max_idle_per_lang: usize_env("ZEROBOOT_POOL_MAX_PER_LANG", 4),
                borrow_timeout_ms: u64_env("ZEROBOOT_POOL_BORROW_TIMEOUT_MS", 5000),
                health_check_interval_secs: u64_env("ZEROBOOT_POOL_HEALTH_CHECK_INTERVAL_SECS", 30),
            },
        })
    }

    pub fn is_trusted_proxy(&self, addr: IpAddr) -> bool {
        self.trusted_proxies.contains(&addr)
    }

    /// Returns the verification mode based on auth mode
    pub fn verification_mode(&self) -> VerificationMode {
        match self.auth_mode {
            AuthMode::Prod => VerificationMode::Prod,
            AuthMode::Dev => VerificationMode::Dev,
        }
    }

    /// Returns the expected release channel from config
    pub fn expected_release_channel(&self) -> Option<&str> {
        self.artifacts.release_channel.as_deref()
    }

    /// Validate startup configuration for production mode.
    /// Returns error if prod mode would run with incomplete/missing required config.
    pub fn validate_startup(&self) -> Result<()> {
        // Only validate in prod mode
        if !matches!(self.auth_mode, AuthMode::Prod) {
            return Ok(());
        }

        // Validate ZEROBOOT_REQUIRE_TEMPLATE_HASHES is set
        if !self.artifacts.require_template_hashes {
            bail!("prod mode requires ZEROBOOT_REQUIRE_TEMPLATE_HASHES=true");
        }

        // Validate ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES is set
        if !self.artifacts.require_template_signatures {
            bail!("prod mode requires ZEROBOOT_REQUIRE_TEMPLATE_SIGNATURES=true");
        }

        // Validate keyring path exists
        let keyring_path = self.artifacts.keyring_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!("prod mode requires ZEROBOOT_KEYRING_PATH to be set")
        })?;
        if !keyring_path.exists() {
            bail!("prod mode requires ZEROBOOT_KEYRING_PATH file to exist: {}", keyring_path.display());
        }

        // Validate allowed firecracker version is set
        if self.artifacts.allowed_firecracker_version.is_none() {
            bail!("prod mode requires ZEROBOOT_ALLOWED_FIRECRACKER_VERSION to be set");
        }

        // Validate firecracker binary sha256 is set
        if self.artifacts.allowed_firecracker_binary_sha256.is_none() {
            bail!("prod mode requires ZEROBOOT_ALLOWED_FC_BINARY_SHA256 to be set");
        }

        // Validate release channel is set
        if self.artifacts.release_channel.is_none() {
            bail!("prod mode requires ZEROBOOT_RELEASE_CHANNEL to be set");
        }

        // Validate api_keys_file exists
        if !self.api_keys_file.exists() {
            bail!("prod mode requires api_keys_file to exist: {}", self.api_keys_file.display());
        }

        // Validate api_key_pepper_file exists
        if !self.api_key_pepper_file.exists() {
            bail!("prod mode requires api_key_pepper_file to exist: {}", self.api_key_pepper_file.display());
        }

        // Validate log_code is false in prod
        if self.logging.log_code {
            bail!("prod mode requires logging.log_code=false (no code logging in prod)");
        }

        Ok(())
    }
}

fn usize_env(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn u64_env(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
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
            logging: LoggingConfig {
                path: PathBuf::from("x"),
                log_code: false,
            },
            health: HealthConfig {
                probe_timeout_secs: 1,
                cache_ttl_secs: 1,
            },
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig { wait_timeout_ms: 1 },
            artifacts: ArtifactConfig {
                require_template_hashes: false,
                allowed_firecracker_version: None,
                allowed_firecracker_binary_sha256: None,
                release_channel: None,
                require_template_signatures: false,
                keyring_path: None,
            },
            pool: PoolConfig {
                min_idle_per_lang: 0,
                max_idle_per_lang: 4,
                borrow_timeout_ms: 5000,
                health_check_interval_secs: 30,
            },
        };
        assert!(cfg.is_trusted_proxy("127.0.0.1".parse().unwrap()));
        assert!(!cfg.is_trusted_proxy("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn prod_without_keyring_fails() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Prod,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig {
                path: PathBuf::from("x"),
                log_code: false,
            },
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig {
                require_template_hashes: true,
                allowed_firecracker_version: Some("1.0.0".into()),
                allowed_firecracker_binary_sha256: Some("abc123".into()),
                release_channel: Some("prod".into()),
                require_template_signatures: true,
                keyring_path: None, // Missing keyring
            },
            pool: PoolConfig::default(),
        };
        let result = cfg.validate_startup();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("keyring"), "expected keyring error: {}", err);
    }

    #[test]
    fn prod_without_release_channel_fails() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Prod,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig {
                path: PathBuf::from("x"),
                log_code: false,
            },
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig {
                require_template_hashes: true,
                allowed_firecracker_version: Some("1.0.0".into()),
                allowed_firecracker_binary_sha256: Some("abc123".into()),
                release_channel: None, // Missing release channel
                require_template_signatures: true,
                keyring_path: Some(PathBuf::from("/etc/zeroboot/keyring")),
            },
            pool: PoolConfig::default(),
        };
        let result = cfg.validate_startup();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("channel"), "expected channel error: {}", err);
    }

    #[test]
    fn prod_with_log_code_true_fails() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Prod,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig {
                path: PathBuf::from("x"),
                log_code: true, // Code logging enabled in prod - should fail
            },
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig {
                require_template_hashes: true,
                allowed_firecracker_version: Some("1.0.0".into()),
                allowed_firecracker_binary_sha256: Some("abc123".into()),
                release_channel: Some("prod".into()),
                require_template_signatures: true,
                keyring_path: Some(PathBuf::from("/etc/zeroboot/keyring")),
            },
            pool: PoolConfig::default(),
        };
        let result = cfg.validate_startup();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("log_code"), "expected log_code error: {}", err);
    }

    #[test]
    fn dev_with_missing_keyring_passes() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Dev,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig {
                path: PathBuf::from("x"),
                log_code: false,
            },
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig {
                require_template_hashes: false,
                allowed_firecracker_version: None,
                allowed_firecracker_binary_sha256: None,
                release_channel: None,
                require_template_signatures: false,
                keyring_path: None, // Missing but dev mode doesn't care
            },
            pool: PoolConfig::default(),
        };
        let result = cfg.validate_startup();
        assert!(result.is_ok(), "dev mode should pass without keyring: {:?}", result);
    }

    #[test]
    fn verification_mode_dev() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Dev,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig::default(),
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig::default(),
            pool: PoolConfig::default(),
        };
        assert_eq!(cfg.verification_mode(), VerificationMode::Dev);
    }

    #[test]
    fn verification_mode_prod() {
        let cfg = ServerConfig {
            auth_mode: AuthMode::Prod,
            api_keys_file: PathBuf::from("api_keys.json"),
            api_key_pepper_file: PathBuf::from("/etc/zeroboot/pepper"),
            trusted_proxies: vec![],
            limits: Limits::default(),
            logging: LoggingConfig::default(),
            health: HealthConfig::default(),
            bind_addr: "127.0.0.1".into(),
            queue: QueueConfig::default(),
            artifacts: ArtifactConfig::default(),
            pool: PoolConfig::default(),
        };
        assert_eq!(cfg.verification_mode(), VerificationMode::Prod);
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_body_bytes: 256 * 1024,
            max_code_bytes: 128 * 1024,
            max_stdin_bytes: 64 * 1024,
            max_batch_size: 16,
            max_stdout_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_total_output_bytes: 96 * 1024,
            max_timeout_secs: 30,
            max_concurrent_requests: 32,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("/var/lib/zeroboot/requests.jsonl"),
            log_code: false,
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            probe_timeout_secs: 2,
            cache_ttl_secs: 10,
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self { wait_timeout_ms: 250 }
    }
}

impl Default for ArtifactConfig {
    fn default() -> Self {
        Self {
            require_template_hashes: false,
            allowed_firecracker_version: None,
            allowed_firecracker_binary_sha256: None,
            release_channel: None,
            require_template_signatures: false,
            keyring_path: None,
        }
    }
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min_idle_per_lang: 0,
            max_idle_per_lang: 4,
            borrow_timeout_ms: 5000,
            health_check_interval_secs: 30,
        }
    }
}
