use axum::extract::{ConnectInfo, Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::auth;
use crate::pool::{LeaseOutcome, PoolManager, RecycleReason};
use crate::startup;

fn process_memory_usage_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = if page_size > 0 {
            page_size as u64
        } else {
            4096
        };
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(rss_pages) = statm
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
            {
                return rss_pages.saturating_mul(page_size);
            }
        }
    }
    0
}
use tokio::sync::{mpsc::Sender, OwnedSemaphorePermit, Semaphore};

use crate::config::ServerConfig;
use crate::protocol::{encode_request_frame, GuestRequest, GuestResponse};
use crate::vmm::kvm::VmSnapshot;

#[derive(Deserialize, Clone)]
pub struct ExecRequest {
    pub code: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub stdin: String,
}

fn default_language() -> String {
    "python".to_string()
}
fn default_timeout() -> u64 {
    30
}

#[derive(Serialize, Clone)]
pub struct ExecResponse {
    pub id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub fork_time_ms: f64,
    pub exec_time_ms: f64,
    pub total_time_ms: f64,
    pub runtime_error_type: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Deserialize)]
pub struct BatchRequest {
    pub executions: Vec<ExecRequest>,
}

#[derive(Serialize)]
pub struct BatchResponse {
    pub results: Vec<ExecResponse>,
}

#[derive(Serialize, Clone)]
pub struct HealthResponse {
    pub status: String,
    pub templates: HashMap<String, TemplateStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Template health status categories for detailed diagnostics
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub enum TemplateHealth {
    /// Template is healthy and ready to serve requests
    #[default]
    Healthy,
    /// Template failed startup verification (missing fields, invalid signatures, etc.)
    QuarantinedTrust,
    /// Template failed runtime health check
    QuarantinedHealth,
    /// Template version incompatible with current Firecracker
    UnsupportedVersion,
}

#[derive(Serialize, Clone)]
pub struct TemplateStatus {
    pub ready: bool,
    pub detail: String,
    #[serde(default)]
    pub health: TemplateHealth,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

pub struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
    rate: f64,
    capacity: f64,
}

impl TokenBucket {
    fn with_capacity(rate: f64, capacity: f64) -> Self {
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
            rate,
            capacity,
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
    }

    fn try_consume_n(&mut self, n: usize) -> bool {
        self.refill();
        let needed = n as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    fn try_consume(&mut self) -> bool {
        self.try_consume_n(1)
    }
}

pub struct Template {
    pub snapshot: Arc<VmSnapshot>,
    pub memfd: i32,
    #[allow(dead_code)]
    pub workdir: String,
}

pub struct AppState {
    pub templates: HashMap<String, Template>,
    pub template_statuses: HashMap<String, TemplateStatus>,
    pub api_key_verifier: Option<auth::ApiKeyVerifier>,
    pub admin_api_key_verifier: Option<auth::ApiKeyVerifier>,
    pub rate_limiters: Mutex<HashMap<String, TokenBucket>>,
    pub metrics: Arc<Metrics>,
    pub pool: Arc<PoolManager>,
    pub config: ServerConfig,
    pub execution_semaphore: Arc<Semaphore>,
    pub request_log_tx: Sender<String>,
    pub health_cache: Mutex<Option<CachedHealthState>>,
    pub admission_paths: Vec<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct CachedHealthState {
    pub generated_at: Instant,
    pub response: HealthResponse,
}

const HISTOGRAM_BUCKETS_MS: &[f64] = &[
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0,
];
const NUM_BUCKETS: usize = 13;

pub struct Histogram {
    buckets: [AtomicU64; NUM_BUCKETS + 1],
    sum_us: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub(crate) fn observe(&self, value_ms: f64) {
        let slot = HISTOGRAM_BUCKETS_MS
            .iter()
            .position(|&bound| value_ms <= bound)
            .unwrap_or(NUM_BUCKETS);
        self.buckets[slot].fetch_add(1, Ordering::Relaxed);
        self.sum_us
            .fetch_add((value_ms * 1000.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn render(&self, name: &str, help: &str) -> String {
        let mut out = format!("# HELP {} {}\n# TYPE {} histogram\n", name, help, name);
        let mut cumulative = 0u64;
        for (i, &bound) in HISTOGRAM_BUCKETS_MS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "{}_bucket{{le=\"{}\"}} {}\n",
                name,
                format_bucket(bound),
                cumulative
            ));
        }
        cumulative += self.buckets[NUM_BUCKETS].load(Ordering::Relaxed);
        out.push_str(&format!("{}_bucket{{le=\"+Inf\"}} {}\n", name, cumulative));
        out.push_str(&format!(
            "{}_sum {}\n",
            name,
            self.sum_us.load(Ordering::Relaxed) as f64 / 1000.0
        ));
        out.push_str(&format!(
            "{}_count {}\n",
            name,
            self.count.load(Ordering::Relaxed)
        ));
        out
    }
}

fn format_bucket(v: f64) -> String {
    if v == v.floor() {
        format!("{}", v as u64)
    } else {
        format!("{}", v)
    }
}

#[derive(Default, Clone)]
pub struct LanguageMetrics {
    pub ok: u64,
    pub error: u64,
    pub timeout: u64,
}

pub struct Metrics {
    pub total_executions: AtomicU64,
    pub total_errors: AtomicU64,
    pub total_timeouts: AtomicU64,
    pub concurrent_forks: AtomicU64,
    pub fork_time_sum_us: AtomicU64,
    pub exec_time_sum_us: AtomicU64,
    pub fork_time_hist: Histogram,
    pub exec_time_hist: Histogram,
    pub total_time_hist: Histogram,
    pub queue_wait_time_hist: Histogram,
    pub pool_borrow_time_hist: Histogram,
    pub lease_release_time_hist: Histogram,
    pub protocol_errors: AtomicU64,
    pub output_truncations: AtomicU64,
    pub worker_recycles: AtomicU64,
    pub queue_rejections: AtomicU64,
    pub pool_borrow_timeouts: AtomicU64,
    pub pool_recycles: AtomicU64,
    pub pool_quarantines: AtomicU64,
    pub health_failures: AtomicU64,
    pub template_quarantines: AtomicU64,
    // Additional trust chain and restore metrics (Commit 14)
    pub manifest_verification_failures: AtomicU64,
    pub signature_verification_failures: AtomicU64,
    pub template_version_mismatches: AtomicU64,
    pub restore_failures: AtomicU64,
    pub worker_boot_failures: AtomicU64,
    pub worker_protocol_failures: AtomicU64,
    pub guest_unhealthy_templates: AtomicU64,
    pub disk_pressure_rejections: AtomicU64,
    pub request_log_write_failures: AtomicU64,
    pub request_log_drops: AtomicU64,
    pub language_counters: Mutex<HashMap<String, LanguageMetrics>>,
    pub request_mode_counters: Mutex<HashMap<String, u64>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            total_executions: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            total_timeouts: AtomicU64::new(0),
            concurrent_forks: AtomicU64::new(0),
            fork_time_sum_us: AtomicU64::new(0),
            exec_time_sum_us: AtomicU64::new(0),
            fork_time_hist: Histogram::new(),
            exec_time_hist: Histogram::new(),
            total_time_hist: Histogram::new(),
            queue_wait_time_hist: Histogram::new(),
            pool_borrow_time_hist: Histogram::new(),
            lease_release_time_hist: Histogram::new(),
            protocol_errors: AtomicU64::new(0),
            output_truncations: AtomicU64::new(0),
            worker_recycles: AtomicU64::new(0),
            queue_rejections: AtomicU64::new(0),
            pool_borrow_timeouts: AtomicU64::new(0),
            pool_recycles: AtomicU64::new(0),
            pool_quarantines: AtomicU64::new(0),
            health_failures: AtomicU64::new(0),
            template_quarantines: AtomicU64::new(0),
            // Additional trust chain and restore metrics (Commit 14)
            manifest_verification_failures: AtomicU64::new(0),
            signature_verification_failures: AtomicU64::new(0),
            template_version_mismatches: AtomicU64::new(0),
            restore_failures: AtomicU64::new(0),
            worker_boot_failures: AtomicU64::new(0),
            worker_protocol_failures: AtomicU64::new(0),
            guest_unhealthy_templates: AtomicU64::new(0),
            disk_pressure_rejections: AtomicU64::new(0),
            request_log_write_failures: AtomicU64::new(0),
            request_log_drops: AtomicU64::new(0),
            language_counters: Mutex::new(HashMap::new()),
            request_mode_counters: Mutex::new(HashMap::new()),
        }
    }

    fn record_language_outcome(&self, language: &str, outcome: &str) {
        let mut counters = match self.language_counters.lock() {
            Ok(counters) => counters,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = counters.entry(language.to_string()).or_default();
        match outcome {
            "ok" => entry.ok += 1,
            "timeout" => entry.timeout += 1,
            _ => entry.error += 1,
        }
    }

    fn record_request_mode(&self, language: &str, mode: &str) {
        let mut counters = match self.request_mode_counters.lock() {
            Ok(counters) => counters,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = format!("{}:{}", language, mode);
        *counters.entry(key).or_insert(0) += 1;
    }
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn check_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    // If no verifier (dev mode or no keys configured), allow anonymous
    let verifier = match &state.api_key_verifier {
        Some(v) => v,
        None => return Ok("anonymous".into()),
    };

    // If verifier exists but has no active keys, require auth
    if verifier.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "No active API keys configured".into(),
                request_id: None,
            }),
        ));
    }

    match extract_api_key(headers) {
        Some(key) => match verifier.verify(&key) {
            Ok(record) => Ok(record.id),
            Err(_) => Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid API key".into(),
                    request_id: None,
                }),
            )),
        },
        None => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Missing Authorization header".into(),
                request_id: None,
            }),
        )),
    }
}

fn extract_client_ip(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> String {
    if state.config.is_trusted_proxy(addr.ip()) {
        if let Some(v) = headers
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
        {
            let ip = v.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    addr.ip().to_string()
}

fn check_rate_limit(
    state: &AppState,
    tenant_key: &str,
    headers: &HeaderMap,
    addr: SocketAddr,
    cost: usize,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let is_demo = tenant_key.starts_with("zb_demo_");
    let client_ip = extract_client_ip(state, headers, addr);
    let bucket_key = if is_demo {
        format!("tenant:{}:{}", tenant_key, client_ip)
    } else {
        format!("tenant:{}", tenant_key)
    };
    let (rate, capacity) = if is_demo {
        (10.0 / 60.0, 10.0)
    } else {
        (100.0, 100.0)
    };
    let mut limiters = match state.rate_limiters.lock() {
        Ok(limiters) => limiters,
        Err(poisoned) => poisoned.into_inner(),
    };
    let bucket = limiters
        .entry(bucket_key)
        .or_insert_with(|| TokenBucket::with_capacity(rate, capacity));
    if bucket.try_consume_n(cost.max(1)) {
        Ok(())
    } else {
        Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: if is_demo {
                    "Rate limit exceeded (10 req/min per demo tenant)".into()
                } else {
                    "Rate limit exceeded".into()
                },
                request_id: None,
            }),
        ))
    }
}

const REQUEST_LOG_DEFAULT_CODE_REDACTION: &str = "[redacted]";

fn iso_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let days = secs / 86400;
    let time = secs % 86400;
    let h = time / 3600;
    let m = (time % 3600) / 60;
    let s = time % 60;
    let z = days as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, m, s, millis
    )
}

fn log_request(
    state: &AppState,
    request_id: &str,
    client_ip: &str,
    api_key_masked: &str,
    req: &ExecRequest,
    response: &ExecResponse,
) {
    let code_value = if state.config.logging.log_code {
        serde_json::Value::String(req.code.chars().take(1000).collect())
    } else {
        serde_json::Value::String(REQUEST_LOG_DEFAULT_CODE_REDACTION.into())
    };
    let line = serde_json::json!({
        "ts": iso_now(),
        "request_id": request_id,
        "client_ip": client_ip,
        "api_key": api_key_masked,
        "language": req.language,
        "stdin_bytes": req.stdin.len(),
        "code_bytes": req.code.len(),
        "code": code_value,
        "exit_code": response.exit_code,
        "runtime_error_type": response.runtime_error_type,
        "stdout_truncated": response.stdout_truncated,
        "stderr_truncated": response.stderr_truncated,
        "fork_time_ms": response.fork_time_ms,
        "exec_time_ms": response.exec_time_ms,
        "total_time_ms": response.total_time_ms,
    });
    if state.request_log_tx.try_send(line.to_string()).is_err() {
        state
            .metrics
            .request_log_drops
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Clone, Copy)]
enum ExecutionMode {
    User,
    Probe,
}

fn error_response(
    request_id: &str,
    stderr: String,
    kind: &str,
    total_start: Instant,
    fork_time_ms: f64,
    exec_time_ms: f64,
) -> ExecResponse {
    ExecResponse {
        id: request_id.to_string(),
        stdout: String::new(),
        stderr,
        exit_code: -1,
        fork_time_ms,
        exec_time_ms,
        total_time_ms: round2(total_start.elapsed().as_secs_f64() * 1000.0),
        runtime_error_type: kind.to_string(),
        stdout_truncated: false,
        stderr_truncated: false,
    }
}

fn truncate_bytes(mut data: Vec<u8>, max_len: usize) -> (Vec<u8>, bool) {
    if data.len() <= max_len {
        return (data, false);
    }
    data.truncate(max_len);
    (data, true)
}

fn normalize_language(language: &str) -> String {
    match language {
        "node" | "javascript" => "node".into(),
        _ => "python".into(),
    }
}

fn execute_code_internal(
    state: &AppState,
    req: &ExecRequest,
    request_id: &str,
    mode: ExecutionMode,
) -> ExecResponse {
    let total_start = Instant::now();
    state
        .metrics
        .concurrent_forks
        .fetch_add(1, Ordering::Relaxed);
    let res = (|| -> ExecResponse {
        let lang = normalize_language(&req.language);
        let limits = &state.config.limits;
        if req.code.len() > limits.max_code_bytes {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(
                request_id,
                format!(
                    "Code too large: {} bytes > {} bytes",
                    req.code.len(),
                    limits.max_code_bytes
                ),
                "validation",
                total_start,
                0.0,
                0.0,
            );
        }
        if req.stdin.len() > limits.max_stdin_bytes {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(
                request_id,
                format!(
                    "stdin too large: {} bytes > {} bytes",
                    req.stdin.len(),
                    limits.max_stdin_bytes
                ),
                "validation",
                total_start,
                0.0,
                0.0,
            );
        }
        if req.timeout_seconds == 0 || req.timeout_seconds > limits.max_timeout_secs {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(
                request_id,
                format!(
                    "timeout_seconds must be between 1 and {}",
                    limits.max_timeout_secs
                ),
                "validation",
                total_start,
                0.0,
                0.0,
            );
        }
        if !state.pool.has_language(&lang) {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            let detail = state
                .template_statuses
                .get(&lang)
                .map(|status| status.detail.clone())
                .unwrap_or_else(|| format!("No template for language: {}", req.language));
            return error_response(request_id, detail, "validation", total_start, 0.0, 0.0);
        }

        let mut lease = match state.pool.borrow(
            &lang,
            Duration::from_millis(state.config.pool.borrow_timeout_ms.max(1)),
        ) {
            Ok(lease) => lease,
            Err(e) => {
                state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                return error_response(
                    request_id,
                    format!("Pool borrow failed: {}", e),
                    "overload",
                    total_start,
                    0.0,
                    0.0,
                );
            }
        };

        let fork_time_us = lease.vm_mut().fork_time_us();
        let fork_time_ms = round2(fork_time_us / 1000.0);
        state
            .metrics
            .fork_time_sum_us
            .fetch_add(fork_time_us as u64, Ordering::Relaxed);
        state.metrics.fork_time_hist.observe(fork_time_ms);
        if matches!(mode, ExecutionMode::User) {
            state.metrics.record_request_mode(&lang, "strict");
        }

        let request_frame = encode_request_frame(&GuestRequest {
            request_id: request_id.to_string(),
            language: lang.clone(),
            code: req.code.as_bytes().to_vec(),
            stdin: req.stdin.as_bytes().to_vec(),
            timeout_ms: req.timeout_seconds * 1000,
        });

        if let Err(e) = lease.vm_mut().send_serial(&request_frame) {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            lease.finish(LeaseOutcome::Quarantined {
                reason: RecycleReason::TransportFailure,
            });
            return error_response(
                request_id,
                format!("Send failed: {}", e),
                "transport",
                total_start,
                fork_time_ms,
                0.0,
            );
        }

        let exec_start = Instant::now();
        let timeout = Duration::from_secs(req.timeout_seconds);
        let guest_response: GuestResponse =
            match lease.vm_mut().run_until_response_timeout(Some(timeout)) {
                Ok(resp) => resp,
                Err(e) => {
                    let msg = e.to_string();
                    let exec_time_ms = round2(exec_start.elapsed().as_secs_f64() * 1000.0);
                    lease.record_exec(exec_time_ms.max(0.0) as u64);
                    if msg.contains("timed out") {
                        lease.record_timeout();
                        lease.finish(LeaseOutcome::Recycled {
                            reason: RecycleReason::HostTimeout,
                        });
                        state.metrics.total_timeouts.fetch_add(1, Ordering::Relaxed);
                        state.metrics.record_language_outcome(&lang, "timeout");
                        return error_response(
                            request_id,
                            format!("Execution timed out after {}s", req.timeout_seconds),
                            "timeout",
                            total_start,
                            fork_time_ms,
                            exec_time_ms,
                        );
                    }
                    lease.finish(LeaseOutcome::Quarantined {
                        reason: RecycleReason::ProtocolFailure,
                    });
                    state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                    state
                        .metrics
                        .protocol_errors
                        .fetch_add(1, Ordering::Relaxed);
                    state.metrics.record_language_outcome(&lang, "error");
                    return error_response(
                        request_id,
                        format!("Protocol failure: {}", msg),
                        "protocol",
                        total_start,
                        fork_time_ms,
                        exec_time_ms,
                    );
                }
            };

        let exec_time_ms = round2(exec_start.elapsed().as_secs_f64() * 1000.0);
        lease.record_exec(exec_time_ms.max(0.0) as u64);
        state.metrics.exec_time_sum_us.fetch_add(
            (exec_start.elapsed().as_secs_f64() * 1_000_000.0) as u64,
            Ordering::Relaxed,
        );
        state
            .metrics
            .exec_time_hist
            .observe(exec_start.elapsed().as_secs_f64() * 1000.0);
        if matches!(mode, ExecutionMode::User) {
            state
                .metrics
                .total_executions
                .fetch_add(1, Ordering::Relaxed);
        }

        if guest_response.request_id != request_id {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            state
                .metrics
                .protocol_errors
                .fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "error");
            lease.finish(LeaseOutcome::Quarantined {
                reason: RecycleReason::ProtocolFailure,
            });
            return error_response(
                request_id,
                format!(
                    "Mismatched response id: got {}, expected {}",
                    guest_response.request_id, request_id
                ),
                "protocol",
                total_start,
                fork_time_ms,
                exec_time_ms,
            );
        }

        let (mut stdout, stdout_truncated_a) =
            truncate_bytes(guest_response.stdout, limits.max_stdout_bytes);
        let (mut stderr, stderr_truncated_a) =
            truncate_bytes(guest_response.stderr, limits.max_stderr_bytes);
        let mut stdout_truncated = stdout_truncated_a || guest_response.stdout_truncated;
        let mut stderr_truncated = stderr_truncated_a || guest_response.stderr_truncated;
        if stdout.len() + stderr.len() > limits.max_total_output_bytes {
            let available_stderr = limits
                .max_total_output_bytes
                .saturating_sub(stdout.len().min(limits.max_total_output_bytes));
            let available_stdout = limits
                .max_total_output_bytes
                .saturating_sub(stderr.len().min(limits.max_total_output_bytes));
            if stdout.len() > available_stdout {
                stdout.truncate(available_stdout);
                stdout_truncated = true;
            }
            if stderr.len() > available_stderr {
                stderr.truncate(available_stderr);
                stderr_truncated = true;
            }
        }
        if stdout_truncated || stderr_truncated {
            state
                .metrics
                .output_truncations
                .fetch_add(1, Ordering::Relaxed);
        }
        if guest_response.recycle_requested {
            state
                .metrics
                .worker_recycles
                .fetch_add(1, Ordering::Relaxed);
        }

        let runtime_error_type = if guest_response.error_type.is_empty() {
            "ok".to_string()
        } else {
            guest_response.error_type.clone()
        };
        let lease_outcome = if guest_response.recycle_requested {
            LeaseOutcome::Recycled {
                reason: RecycleReason::GuestRequested,
            }
        } else if guest_response.exit_code < 0 {
            lease.record_signal_death();
            LeaseOutcome::Quarantined {
                reason: RecycleReason::ChildSignalDeath,
            }
        } else {
            LeaseOutcome::ReturnedIdle
        };
        if guest_response.exit_code == 0 && runtime_error_type == "ok" {
            state.metrics.record_language_outcome(&lang, "ok");
        } else if runtime_error_type == "timeout" {
            state.metrics.total_timeouts.fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "timeout");
        } else {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "error");
        }
        lease.finish(lease_outcome);

        ExecResponse {
            id: request_id.to_string(),
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
            exit_code: guest_response.exit_code,
            fork_time_ms,
            exec_time_ms,
            total_time_ms: round2(total_start.elapsed().as_secs_f64() * 1000.0),
            runtime_error_type,
            stdout_truncated,
            stderr_truncated,
        }
    })();
    if matches!(mode, ExecutionMode::User) {
        state.metrics.total_time_hist.observe(res.total_time_ms);
    }
    state
        .metrics
        .concurrent_forks
        .fetch_sub(1, Ordering::Relaxed);
    res
}

fn probe_template(state: &AppState, language: &str) -> ExecResponse {
    let req = ExecRequest {
        code: if language == "node" {
            "console.log(\"ok\")".into()
        } else {
            "print(\"ok\")".into()
        },
        language: language.to_string(),
        timeout_seconds: state.config.health.probe_timeout_secs.max(1),
        stdin: String::new(),
    };
    execute_code_internal(
        state,
        &req,
        &format!("health-{}", language),
        ExecutionMode::Probe,
    )
}

async fn acquire_permit(
    state: &Arc<AppState>,
) -> Result<OwnedSemaphorePermit, (StatusCode, Json<ErrorResponse>)> {
    let wait_start = Instant::now();
    let timeout = Duration::from_millis(state.config.queue.wait_timeout_ms.max(1));
    match tokio::time::timeout(timeout, state.execution_semaphore.clone().acquire_owned()).await {
        Ok(Ok(permit)) => {
            state
                .metrics
                .queue_wait_time_hist
                .observe(wait_start.elapsed().as_secs_f64() * 1000.0);
            Ok(permit)
        }
        Ok(Err(_)) | Err(_) => {
            state
                .metrics
                .queue_rejections
                .fetch_add(1, Ordering::Relaxed);
            Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(ErrorResponse {
                    error: "Execution queue is full".into(),
                    request_id: None,
                }),
            ))
        }
    }
}

pub async fn exec_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> impl IntoResponse {
    if let Err(e) = check_runtime_admission(&state) {
        return e.into_response();
    }
    let api_key = match check_auth(&state, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = check_rate_limit(&state, &api_key, &headers, addr, 1) {
        return e.into_response();
    }
    let permit = match acquire_permit(&state).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let request_id = uuid::Uuid::new_v4().to_string();
    let rid = request_id.clone();
    let client_ip = extract_client_ip(&state, &headers, addr);
    let masked_key = mask_api_key(&api_key);
    let req_for_log = req.clone();
    let state_for_task = state.clone();
    let response = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        execute_code_internal(&state_for_task, &req, &rid, ExecutionMode::User)
    })
    .await
    {
        Ok(response) => response,
        Err(e) => {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            error_response(
                &request_id,
                format!("Execution task failed: {}", e),
                "internal",
                Instant::now(),
                0.0,
                0.0,
            )
        }
    };

    log_request(
        &state,
        &request_id,
        &client_ip,
        &masked_key,
        &req_for_log,
        &response,
    );
    (StatusCode::OK, Json(response)).into_response()
}

pub async fn batch_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<BatchRequest>,
) -> impl IntoResponse {
    if let Err(e) = check_runtime_admission(&state) {
        return e.into_response();
    }
    let api_key = match check_auth(&state, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = check_rate_limit(&state, &api_key, &headers, addr, req.executions.len()) {
        return e.into_response();
    }
    if req.executions.len() > state.config.limits.max_batch_size {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!(
                    "Batch too large: {} > {}",
                    req.executions.len(),
                    state.config.limits.max_batch_size
                ),
                request_id: None,
            }),
        )
            .into_response();
    }

    let client_ip = extract_client_ip(&state, &headers, addr);
    let masked_key = mask_api_key(&api_key);

    // Acquire ALL permits upfront before spawning any tasks (atomic batch)
    let mut permits = Vec::with_capacity(req.executions.len());
    for _ in 0..req.executions.len() {
        match acquire_permit(&state).await {
            Ok(p) => permits.push(p),
            Err(e) => return e.into_response(),
        };
    }

    // Now spawn all tasks - we have all permits
    let mut tasks = Vec::with_capacity(req.executions.len());
    for (index, exec_req) in req.executions.into_iter().enumerate() {
        let permit = permits.remove(0);
        let state_for_task = state.clone();
        let request_id_for_task = uuid::Uuid::new_v4().to_string();
        let rid_for_task = request_id_for_task.clone();
        let req_for_log = exec_req.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let response = execute_code_internal(
                &state_for_task,
                &exec_req,
                &rid_for_task,
                ExecutionMode::User,
            );
            (index, request_id_for_task, req_for_log, response)
        }));
    }

    let mut collected = Vec::with_capacity(tasks.len());
    for task in tasks {
        match task.await {
            Ok(tuple) => collected.push(tuple),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: format!("Batch execution task failed: {}", e),
                        request_id: None,
                    }),
                )
                    .into_response();
            }
        }
    }
    collected.sort_by_key(|(index, _, _, _)| *index);

    let mut results = Vec::with_capacity(collected.len());
    for (_, request_id, req_for_log, response) in collected {
        log_request(
            &state,
            &request_id,
            &client_ip,
            &masked_key,
            &req_for_log,
            &response,
        );
        results.push(response);
    }

    (StatusCode::OK, Json(BatchResponse { results })).into_response()
}

fn mask_api_key(api_key: &str) -> String {
    if api_key.len() > 8 {
        format!("{}...{}", &api_key[..4], &api_key[api_key.len() - 4..])
    } else {
        "***".to_string()
    }
}

fn classify_template_health(ready: bool, detail: &str) -> TemplateHealth {
    if ready {
        return TemplateHealth::Healthy;
    }

    let detail = detail.to_ascii_lowercase();
    if detail.contains("signature")
        || detail.contains("verify")
        || detail.contains("sha256")
        || detail.contains("hash")
    {
        TemplateHealth::QuarantinedTrust
    } else if detail.contains("version") || detail.contains("firecracker") {
        TemplateHealth::UnsupportedVersion
    } else {
        TemplateHealth::QuarantinedHealth
    }
}

fn check_runtime_admission(state: &AppState) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if let Err(error) = startup::ensure_runtime_admission(&state.config, &state.admission_paths) {
        state
            .metrics
            .disk_pressure_rejections
            .fetch_add(1, Ordering::Relaxed);
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: format!("runtime admission refused: {}", error),
                request_id: None,
            }),
        ));
    }
    Ok(())
}

fn static_ready_state(state: &AppState) -> HealthResponse {
    let mut templates = state.template_statuses.clone();
    for status in templates.values_mut() {
        status.health = classify_template_health(status.ready, &status.detail);
    }
    let mut all_ready = templates.values().all(|status| status.ready);
    let mut error = None;
    if let Err(admission_error) =
        startup::ensure_runtime_admission(&state.config, &state.admission_paths)
    {
        all_ready = false;
        error = Some(admission_error.to_string());
    }
    HealthResponse {
        status: if all_ready {
            "ok".into()
        } else {
            "degraded".into()
        },
        templates,
        error,
    }
}

fn probe_all_templates(state: &AppState) -> HealthResponse {
    let mut templates = HashMap::new();
    let mut all_ready = true;
    for (name, startup_status) in state.template_statuses.iter() {
        if !startup_status.ready || !state.templates.contains_key(name) {
            all_ready = false;
            templates.insert(name.clone(), startup_status.clone());
            continue;
        }
        let probe = probe_template(state, name);
        let ready = probe.exit_code == 0 && probe.stdout.trim() == "ok";
        if !ready {
            state
                .metrics
                .health_failures
                .fetch_add(1, Ordering::Relaxed);
            all_ready = false;
        }
        let health = classify_template_health(ready, &startup_status.detail);
        templates.insert(
            name.clone(),
            TemplateStatus {
                ready,
                detail: if ready {
                    "probe ok".into()
                } else {
                    probe.stderr
                },
                health,
            },
        );
    }
    HealthResponse {
        status: if all_ready {
            "ok".into()
        } else {
            "degraded".into()
        },
        templates,
        error: None,
    }
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    // Health probes must acquire a permit to avoid starving user requests
    let permit = match acquire_permit(&state).await {
        Ok(p) => p,
        Err((status, json)) => {
            return Json(HealthResponse {
                status: "unavailable".into(),
                templates: Default::default(),
                error: Some(format!("{:?} {}", status, json.0.error)),
            })
        }
    };

    let ttl = Duration::from_secs(state.config.health.cache_ttl_secs.max(1));
    let cached = match state.health_cache.lock() {
        Ok(cache) => cache.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if let Some(cached) = cached {
        if cached.generated_at.elapsed() < ttl {
            return Json(cached.response);
        }
    }

    let state_for_task = state.clone();
    let response = match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        probe_all_templates(&state_for_task)
    })
    .await
    {
        Ok(response) => response,
        Err(e) => {
            return Json(HealthResponse {
                status: "unavailable".into(),
                templates: Default::default(),
                error: Some(format!("health probe task failed: {}", e)),
            });
        }
    };
    let mut cache = match state.health_cache.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    *cache = Some(CachedHealthState {
        generated_at: Instant::now(),
        response: response.clone(),
    });
    Json(response)
}

pub async fn live_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

pub async fn ready_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(static_ready_state(&state))
}

pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> String {
    let m = &state.metrics;
    let pool_status = state.pool.status();
    let total = m.total_executions.load(Ordering::Relaxed);
    let errors = m.total_errors.load(Ordering::Relaxed);
    let timeouts = m.total_timeouts.load(Ordering::Relaxed);
    let concurrent = m.concurrent_forks.load(Ordering::Relaxed);
    let fork_sum = m.fork_time_sum_us.load(Ordering::Relaxed);
    let exec_sum = m.exec_time_sum_us.load(Ordering::Relaxed);
    let process_rss = process_memory_usage_bytes();
    let available_slots = state.execution_semaphore.available_permits() as u64;
    let total_slots = state.config.limits.max_concurrent_requests as u64;
    let used_slots = total_slots.saturating_sub(available_slots);

    let mut out = String::new();
    let mut push_metric = |name: &str, help: &str, metric_type: &str, value: String| {
        out.push_str(&format!(
            "# HELP {} {}\n# TYPE {} {}\n{} {}\n",
            name, help, name, metric_type, name, value
        ));
    };

    push_metric(
        "zeroboot_total_executions",
        "Total number of user executions",
        "counter",
        total.to_string(),
    );
    push_metric(
        "zeroboot_total_errors",
        "Total number of execution errors",
        "counter",
        errors.to_string(),
    );
    push_metric(
        "zeroboot_total_timeouts",
        "Total number of execution timeouts",
        "counter",
        timeouts.to_string(),
    );
    push_metric(
        "zeroboot_protocol_errors",
        "Total number of protocol failures",
        "counter",
        m.protocol_errors.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_output_truncations",
        "Total number of truncated responses",
        "counter",
        m.output_truncations.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_worker_recycles",
        "Total number of guest worker recycle requests observed",
        "counter",
        m.worker_recycles.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_queue_rejections",
        "Total number of queue rejections",
        "counter",
        m.queue_rejections.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_pool_borrow_timeouts",
        "Total number of pool borrow timeouts",
        "counter",
        m.pool_borrow_timeouts.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_pool_recycles_total",
        "Total number of pooled VM recycles",
        "counter",
        m.pool_recycles.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_pool_quarantines_total",
        "Total number of pooled VM quarantines",
        "counter",
        m.pool_quarantines.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_health_failures",
        "Total number of failed health probes",
        "counter",
        m.health_failures.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_template_quarantines",
        "Total number of templates quarantined at startup",
        "gauge",
        m.template_quarantines.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_manifest_verification_failures",
        "Total number of template manifest verification failures",
        "counter",
        m.manifest_verification_failures
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_signature_verification_failures",
        "Total number of template signature verification failures",
        "counter",
        m.signature_verification_failures
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_template_version_mismatches",
        "Total number of template version mismatches",
        "counter",
        m.template_version_mismatches
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_restore_failures",
        "Total number of VM restore failures",
        "counter",
        m.restore_failures.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_worker_boot_failures",
        "Total number of guest worker boot failures",
        "counter",
        m.worker_boot_failures.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_worker_protocol_failures",
        "Total number of guest worker protocol failures",
        "counter",
        m.worker_protocol_failures
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_guest_unhealthy_templates",
        "Total number of guest templates that failed health probing",
        "counter",
        m.guest_unhealthy_templates
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_disk_pressure_rejections",
        "Total number of requests refused due to disk or inode watermarks",
        "counter",
        m.disk_pressure_rejections
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_request_log_write_failures",
        "Total number of request log write failures",
        "counter",
        m.request_log_write_failures
            .load(Ordering::Relaxed)
            .to_string(),
    );
    push_metric(
        "zeroboot_request_log_drops",
        "Total number of request log records dropped due to a full queue",
        "counter",
        m.request_log_drops.load(Ordering::Relaxed).to_string(),
    );
    push_metric(
        "zeroboot_concurrent_forks",
        "Current number of in-flight executions",
        "gauge",
        concurrent.to_string(),
    );
    push_metric(
        "zeroboot_execution_slots_available",
        "Available execution slots in the admission semaphore",
        "gauge",
        available_slots.to_string(),
    );
    push_metric(
        "zeroboot_execution_slots_used",
        "Execution slots currently consumed by in-flight or queued work",
        "gauge",
        used_slots.to_string(),
    );
    push_metric(
        "zeroboot_execution_slots_capacity",
        "Total execution slot capacity",
        "gauge",
        total_slots.to_string(),
    );
    push_metric(
        "zeroboot_memory_usage_bytes",
        "Resident memory usage for the zeroboot process",
        "gauge",
        process_rss.to_string(),
    );
    push_metric(
        "zeroboot_fork_time_sum_milliseconds",
        "Sum of fork times in milliseconds",
        "counter",
        format!("{}", fork_sum as f64 / 1000.0),
    );
    push_metric(
        "zeroboot_exec_time_sum_milliseconds",
        "Sum of exec times in milliseconds",
        "counter",
        format!("{}", exec_sum as f64 / 1000.0),
    );
    out.push_str(&m.fork_time_hist.render(
        "zeroboot_fork_time_milliseconds",
        "Distribution of VM fork times in milliseconds",
    ));
    out.push_str(&m.exec_time_hist.render(
        "zeroboot_exec_time_milliseconds",
        "Distribution of guest execution times in milliseconds",
    ));
    out.push_str(&m.total_time_hist.render(
        "zeroboot_total_time_milliseconds",
        "Distribution of end-to-end request times in milliseconds",
    ));
    out.push_str(&m.queue_wait_time_hist.render(
        "zeroboot_queue_wait_time_milliseconds",
        "Distribution of queue wait time before execution in milliseconds",
    ));
    out.push_str(&m.pool_borrow_time_hist.render(
        "zeroboot_pool_borrow_time_milliseconds",
        "Distribution of pool borrow latency in milliseconds",
    ));
    out.push_str(&m.lease_release_time_hist.render(
        "zeroboot_pool_release_time_milliseconds",
        "Distribution of lease release and recycle time in milliseconds",
    ));
    for (language, status) in state.template_statuses.iter() {
        out.push_str(&format!(
            "# TYPE zeroboot_template_ready gauge\nzeroboot_template_ready{{language=\"{}\"}} {}\n",
            language,
            if status.ready { 1 } else { 0 }
        ));
    }
    for (language, lane) in pool_status.lanes.iter() {
        out.push_str(&format!(
            "# TYPE zeroboot_pool_idle_vms gauge\nzeroboot_pool_idle_vms{{language=\"{}\"}} {}\n",
            language, lane.idle
        ));
        out.push_str(&format!(
            "# TYPE zeroboot_pool_active_vms gauge\nzeroboot_pool_active_vms{{language=\"{}\"}} {}\n",
            language, lane.active
        ));
        out.push_str(&format!(
            "# TYPE zeroboot_pool_target_vms gauge\nzeroboot_pool_target_vms{{language=\"{}\"}} {}\n",
            language, lane.target_idle
        ));
        out.push_str(&format!(
            "# TYPE zeroboot_pool_waiters gauge\nzeroboot_pool_waiters{{language=\"{}\"}} {}\n",
            language, lane.waiters
        ));
        out.push_str(&format!(
            "# TYPE zeroboot_pool_quarantined gauge\nzeroboot_pool_quarantined{{language=\"{}\"}} {}\n",
            language, lane.quarantined
        ));
    }
    let counters = match m.language_counters.lock() {
        Ok(counters) => counters,
        Err(poisoned) => poisoned.into_inner(),
    };
    for (language, stats) in counters.iter() {
        out.push_str(&format!(
            "# TYPE zeroboot_language_executions_total counter\nzeroboot_language_executions_total{{language=\"{}\",result=\"ok\"}} {}\nzeroboot_language_executions_total{{language=\"{}\",result=\"error\"}} {}\nzeroboot_language_executions_total{{language=\"{}\",result=\"timeout\"}} {}\n",
            language, stats.ok, language, stats.error, language, stats.timeout
        ));
    }
    let request_modes = match m.request_mode_counters.lock() {
        Ok(counters) => counters,
        Err(poisoned) => poisoned.into_inner(),
    };
    for (key, total) in request_modes.iter() {
        if let Some((language, mode)) = key.split_once(':') {
            out.push_str(&format!(
                "# TYPE zeroboot_request_mode_total counter\nzeroboot_request_mode_total{{language=\"{}\",mode=\"{}\"}} {}\n",
                language, mode, total
            ));
        }
    }
    out
}

pub fn apply_request_log_path_fix(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}
