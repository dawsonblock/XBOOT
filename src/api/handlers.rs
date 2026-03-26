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

fn process_memory_usage_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page_size = if page_size > 0 { page_size as u64 } else { 4096 };
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(rss_pages) = statm.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                return rss_pages.saturating_mul(page_size);
            }
        }
    }
    0
}
use tokio::sync::{mpsc::UnboundedSender, OwnedSemaphorePermit, Semaphore};

use crate::config::ServerConfig;
use crate::protocol::{encode_request_frame, GuestRequest, GuestResponse};
use crate::vmm::kvm::{ForkedVm, VmSnapshot};

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

fn default_language() -> String { "python".to_string() }
fn default_timeout() -> u64 { 30 }

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
}

#[derive(Serialize, Clone)]
pub struct TemplateStatus {
    pub ready: bool,
    pub detail: String,
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
        Self { tokens: capacity, last_refill: Instant::now(), rate, capacity }
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
    pub snapshot: VmSnapshot,
    pub memfd: i32,
    #[allow(dead_code)]
    pub workdir: String,
}

pub struct AppState {
    pub templates: HashMap<String, Template>,
    pub template_statuses: HashMap<String, TemplateStatus>,
    pub api_keys: Vec<String>,
    pub rate_limiters: Mutex<HashMap<String, TokenBucket>>,
    pub metrics: Metrics,
    pub config: ServerConfig,
    pub execution_semaphore: Arc<Semaphore>,
    pub request_log_tx: UnboundedSender<String>,
    pub health_cache: Mutex<Option<CachedHealthState>>,
}

#[derive(Clone)]
pub struct CachedHealthState {
    pub generated_at: Instant,
    pub response: HealthResponse,
}

const HISTOGRAM_BUCKETS_MS: &[f64] = &[0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0];
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

    fn observe(&self, value_ms: f64) {
        let slot = HISTOGRAM_BUCKETS_MS.iter().position(|&bound| value_ms <= bound).unwrap_or(NUM_BUCKETS);
        self.buckets[slot].fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add((value_ms * 1000.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn render(&self, name: &str, help: &str) -> String {
        let mut out = format!("# HELP {} {}\n# TYPE {} histogram\n", name, help, name);
        let mut cumulative = 0u64;
        for (i, &bound) in HISTOGRAM_BUCKETS_MS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!("{}_bucket{{le=\"{}\"}} {}\n", name, format_bucket(bound), cumulative));
        }
        cumulative += self.buckets[NUM_BUCKETS].load(Ordering::Relaxed);
        out.push_str(&format!("{}_bucket{{le=\"+Inf\"}} {}\n", name, cumulative));
        out.push_str(&format!("{}_sum {}\n", name, self.sum_us.load(Ordering::Relaxed) as f64 / 1000.0));
        out.push_str(&format!("{}_count {}\n", name, self.count.load(Ordering::Relaxed)));
        out
    }
}

fn format_bucket(v: f64) -> String {
    if v == v.floor() { format!("{}", v as u64) } else { format!("{}", v) }
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
    pub protocol_errors: AtomicU64,
    pub output_truncations: AtomicU64,
    pub worker_recycles: AtomicU64,
    pub queue_rejections: AtomicU64,
    pub health_failures: AtomicU64,
    pub template_quarantines: AtomicU64,
    pub language_counters: Mutex<HashMap<String, LanguageMetrics>>,
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
            protocol_errors: AtomicU64::new(0),
            output_truncations: AtomicU64::new(0),
            worker_recycles: AtomicU64::new(0),
            queue_rejections: AtomicU64::new(0),
            health_failures: AtomicU64::new(0),
            template_quarantines: AtomicU64::new(0),
            language_counters: Mutex::new(HashMap::new()),
        }
    }

    fn record_language_outcome(&self, language: &str, outcome: &str) {
        let mut counters = self.language_counters.lock().unwrap();
        let entry = counters.entry(language.to_string()).or_default();
        match outcome {
            "ok" => entry.ok += 1,
            "timeout" => entry.timeout += 1,
            _ => entry.error += 1,
        }
    }
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    if state.api_keys.is_empty() {
        return Ok("anonymous".into());
    }
    match extract_api_key(headers) {
        Some(key) if state.api_keys.contains(&key) => Ok(key),
        Some(_) => Err((StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Invalid API key".into(), request_id: None }))),
        None => Err((StatusCode::UNAUTHORIZED, Json(ErrorResponse { error: "Missing Authorization header".into(), request_id: None }))),
    }
}

fn extract_client_ip(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> String {
    if state.config.is_trusted_proxy(addr.ip()) {
        if let Some(v) = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()) {
            let ip = v.trim();
            if !ip.is_empty() { return ip.to_string(); }
        }
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() { return ip.to_string(); }
            }
        }
    }
    addr.ip().to_string()
}

fn check_rate_limit(state: &AppState, tenant_key: &str, headers: &HeaderMap, addr: SocketAddr, cost: usize) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let is_demo = tenant_key.starts_with("zb_demo_");
    let client_ip = extract_client_ip(state, headers, addr);
    let bucket_key = if is_demo {
        format!("tenant:{}:{}", tenant_key, client_ip)
    } else {
        format!("tenant:{}", tenant_key)
    };
    let (rate, capacity) = if is_demo { (10.0 / 60.0, 10.0) } else { (100.0, 100.0) };
    let mut limiters = state.rate_limiters.lock().unwrap();
    let bucket = limiters.entry(bucket_key).or_insert_with(|| TokenBucket::with_capacity(rate, capacity));
    if bucket.try_consume_n(cost.max(1)) {
        Ok(())
    } else {
        Err((StatusCode::TOO_MANY_REQUESTS, Json(ErrorResponse {
            error: if is_demo { "Rate limit exceeded (10 req/min per demo tenant)".into() } else { "Rate limit exceeded".into() },
            request_id: None,
        })))
    }
}

const REQUEST_LOG_DEFAULT_CODE_REDACTION: &str = "[redacted]";

fn iso_now() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
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
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, m, s, millis)
}

fn log_request(state: &AppState, request_id: &str, client_ip: &str, api_key_masked: &str, req: &ExecRequest, response: &ExecResponse) {
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
    let _ = state.request_log_tx.send(line.to_string());
}

fn round2(v: f64) -> f64 { (v * 100.0).round() / 100.0 }

#[derive(Clone, Copy)]
enum ExecutionMode {
    User,
    Probe,
}

fn error_response(request_id: &str, stderr: String, kind: &str, total_start: Instant, fork_time_ms: f64, exec_time_ms: f64) -> ExecResponse {
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

fn execute_code_internal(state: &AppState, req: &ExecRequest, request_id: &str, mode: ExecutionMode) -> ExecResponse {
    let total_start = Instant::now();
    state.metrics.concurrent_forks.fetch_add(1, Ordering::Relaxed);
    let res = (|| -> ExecResponse {
        let lang = normalize_language(&req.language);
        let limits = &state.config.limits;
        if req.code.len() > limits.max_code_bytes {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(request_id, format!("Code too large: {} bytes > {} bytes", req.code.len(), limits.max_code_bytes), "validation", total_start, 0.0, 0.0);
        }
        if req.stdin.len() > limits.max_stdin_bytes {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(request_id, format!("stdin too large: {} bytes > {} bytes", req.stdin.len(), limits.max_stdin_bytes), "validation", total_start, 0.0, 0.0);
        }
        if req.timeout_seconds == 0 || req.timeout_seconds > limits.max_timeout_secs {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(request_id, format!("timeout_seconds must be between 1 and {}", limits.max_timeout_secs), "validation", total_start, 0.0, 0.0);
        }
        let template = match state.templates.get(&lang) {
            Some(t) => t,
            None => {
                state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                let detail = state
                    .template_statuses
                    .get(&lang)
                    .map(|status| status.detail.clone())
                    .unwrap_or_else(|| format!("No template for language: {}", req.language));
                return error_response(request_id, detail, "validation", total_start, 0.0, 0.0);
            }
        };

        let mut vm = match ForkedVm::fork_cow(&template.snapshot, template.memfd) {
            Ok(vm) => vm,
            Err(e) => {
                state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                return error_response(request_id, format!("Fork failed: {}", e), "fork", total_start, 0.0, 0.0);
            }
        };
        let fork_time_ms = round2(vm.fork_time_us / 1000.0);
        state.metrics.fork_time_sum_us.fetch_add(vm.fork_time_us as u64, Ordering::Relaxed);
        state.metrics.fork_time_hist.observe(vm.fork_time_us / 1000.0);

        let request_frame = encode_request_frame(&GuestRequest {
            request_id: request_id.to_string(),
            language: lang.clone(),
            code: req.code.as_bytes().to_vec(),
            stdin: req.stdin.as_bytes().to_vec(),
            timeout_ms: req.timeout_seconds * 1000,
        });

        if let Err(e) = vm.send_serial(&request_frame) {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            return error_response(request_id, format!("Send failed: {}", e), "transport", total_start, fork_time_ms, 0.0);
        }

        let exec_start = Instant::now();
        let timeout = Duration::from_secs(req.timeout_seconds);
        let guest_response: GuestResponse = match vm.run_until_response_timeout(Some(timeout)) {
            Ok(resp) => resp,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("timed out") {
                    state.metrics.total_timeouts.fetch_add(1, Ordering::Relaxed);
                    state.metrics.record_language_outcome(&lang, "timeout");
                    return error_response(request_id, format!("Execution timed out after {}s", req.timeout_seconds), "timeout", total_start, fork_time_ms, round2(exec_start.elapsed().as_secs_f64() * 1000.0));
                }
                state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
                state.metrics.protocol_errors.fetch_add(1, Ordering::Relaxed);
                state.metrics.record_language_outcome(&lang, "error");
                return error_response(request_id, format!("Protocol failure: {}", msg), "protocol", total_start, fork_time_ms, round2(exec_start.elapsed().as_secs_f64() * 1000.0));
            }
        };

        let exec_time_ms = round2(exec_start.elapsed().as_secs_f64() * 1000.0);
        state.metrics.exec_time_sum_us.fetch_add((exec_start.elapsed().as_secs_f64() * 1_000_000.0) as u64, Ordering::Relaxed);
        state.metrics.exec_time_hist.observe(exec_start.elapsed().as_secs_f64() * 1000.0);
        if matches!(mode, ExecutionMode::User) {
            state.metrics.total_executions.fetch_add(1, Ordering::Relaxed);
        }

        if guest_response.request_id != request_id {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            state.metrics.protocol_errors.fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "error");
            return error_response(request_id, format!("Mismatched response id: got {}, expected {}", guest_response.request_id, request_id), "protocol", total_start, fork_time_ms, exec_time_ms);
        }

        let (mut stdout, stdout_truncated_a) = truncate_bytes(guest_response.stdout, limits.max_stdout_bytes);
        let (mut stderr, stderr_truncated_a) = truncate_bytes(guest_response.stderr, limits.max_stderr_bytes);
        let mut stdout_truncated = stdout_truncated_a || guest_response.stdout_truncated;
        let mut stderr_truncated = stderr_truncated_a || guest_response.stderr_truncated;
        if stdout.len() + stderr.len() > limits.max_total_output_bytes {
            let available_stderr = limits.max_total_output_bytes.saturating_sub(stdout.len().min(limits.max_total_output_bytes));
            let available_stdout = limits.max_total_output_bytes.saturating_sub(stderr.len().min(limits.max_total_output_bytes));
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
            state.metrics.output_truncations.fetch_add(1, Ordering::Relaxed);
        }
        if guest_response.recycle_requested {
            state.metrics.worker_recycles.fetch_add(1, Ordering::Relaxed);
        }

        let runtime_error_type = if guest_response.error_type.is_empty() { "ok".to_string() } else { guest_response.error_type.clone() };
        if guest_response.exit_code == 0 && runtime_error_type == "ok" {
            state.metrics.record_language_outcome(&lang, "ok");
        } else if runtime_error_type == "timeout" {
            state.metrics.total_timeouts.fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "timeout");
        } else {
            state.metrics.total_errors.fetch_add(1, Ordering::Relaxed);
            state.metrics.record_language_outcome(&lang, "error");
        }

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
    state.metrics.concurrent_forks.fetch_sub(1, Ordering::Relaxed);
    res
}

fn probe_template(state: &AppState, language: &str) -> ExecResponse {
    let req = ExecRequest {
        code: if language == "node" { "console.log(\"ok\")".into() } else { "print(\"ok\")".into() },
        language: language.to_string(),
        timeout_seconds: state.config.health.probe_timeout_secs.max(1),
        stdin: String::new(),
    };
    execute_code_internal(state, &req, &format!("health-{}", language), ExecutionMode::Probe)
}

async fn acquire_permit(state: &Arc<AppState>) -> Result<OwnedSemaphorePermit, (StatusCode, Json<ErrorResponse>)> {
    let wait_start = Instant::now();
    let timeout = Duration::from_millis(state.config.queue.wait_timeout_ms.max(1));
    match tokio::time::timeout(timeout, state.execution_semaphore.clone().acquire_owned()).await {
        Ok(Ok(permit)) => {
            state.metrics.queue_wait_time_hist.observe(wait_start.elapsed().as_secs_f64() * 1000.0);
            Ok(permit)
        }
        Ok(Err(_)) | Err(_) => {
            state.metrics.queue_rejections.fetch_add(1, Ordering::Relaxed);
            Err((StatusCode::TOO_MANY_REQUESTS, Json(ErrorResponse { error: "Execution queue is full".into(), request_id: None })))
        }
    }
}

pub async fn exec_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> impl IntoResponse {
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

    let request_id = uuid::Uuid::now_v7().to_string();
    let rid = request_id.clone();
    let client_ip = extract_client_ip(&state, &headers, addr);
    let masked_key = mask_api_key(&api_key);
    let req_for_log = req.clone();
    let state_for_task = state.clone();
    let response = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        execute_code_internal(&state_for_task, &req, &rid, ExecutionMode::User)
    }).await.unwrap();

    log_request(&state, &request_id, &client_ip, &masked_key, &req_for_log, &response);
    (StatusCode::OK, Json(response)).into_response()
}

pub async fn batch_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<BatchRequest>,
) -> impl IntoResponse {
    let api_key = match check_auth(&state, &headers) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = check_rate_limit(&state, &api_key, &headers, addr, req.executions.len()) {
        return e.into_response();
    }
    if req.executions.len() > state.config.limits.max_batch_size {
        return (StatusCode::BAD_REQUEST, Json(ErrorResponse {
            error: format!("Batch too large: {} > {}", req.executions.len(), state.config.limits.max_batch_size),
            request_id: None,
        })).into_response();
    }

    let client_ip = extract_client_ip(&state, &headers, addr);
    let masked_key = mask_api_key(&api_key);
    let mut tasks = Vec::with_capacity(req.executions.len());
    for (index, exec_req) in req.executions.into_iter().enumerate() {
        let permit = match acquire_permit(&state).await {
            Ok(p) => p,
            Err(e) => return e.into_response(),
        };
        let state_for_task = state.clone();
        let request_id = uuid::Uuid::now_v7().to_string();
        let rid = request_id.clone();
        let req_for_log = exec_req.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let response = execute_code_internal(&state_for_task, &exec_req, &rid, ExecutionMode::User);
            (index, request_id, req_for_log, response)
        }));
    }

    let mut collected = Vec::with_capacity(tasks.len());
    for task in tasks {
        match task.await {
            Ok(tuple) => collected.push(tuple),
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse {
                    error: format!("Batch execution task failed: {}", e),
                    request_id: None,
                })).into_response();
            }
        }
    }
    collected.sort_by_key(|(index, _, _, _)| *index);

    let mut results = Vec::with_capacity(collected.len());
    for (_, request_id, req_for_log, response) in collected {
        log_request(&state, &request_id, &client_ip, &masked_key, &req_for_log, &response);
        results.push(response);
    }

    (StatusCode::OK, Json(BatchResponse { results })).into_response()
}

fn mask_api_key(api_key: &str) -> String {
    if api_key.len() > 8 {
        format!("{}...{}", &api_key[..4], &api_key[api_key.len()-4..])
    } else {
        "***".to_string()
    }
}

fn static_ready_state(state: &AppState) -> HealthResponse {
    let templates = state.template_statuses.clone();
    let all_ready = templates.values().all(|status| status.ready);
    HealthResponse {
        status: if all_ready { "ok".into() } else { "degraded".into() },
        templates,
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
            state.metrics.health_failures.fetch_add(1, Ordering::Relaxed);
            all_ready = false;
        }
        templates.insert(name.clone(), TemplateStatus {
            ready,
            detail: if ready { "probe ok".into() } else { probe.stderr },
        });
    }
    HealthResponse { status: if all_ready { "ok".into() } else { "degraded".into() }, templates }
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let ttl = Duration::from_secs(state.config.health.cache_ttl_secs.max(1));
    if let Some(cached) = state.health_cache.lock().unwrap().clone() {
        if cached.generated_at.elapsed() < ttl {
            return Json(cached.response);
        }
    }

    let state_for_task = state.clone();
    let response = tokio::task::spawn_blocking(move || probe_all_templates(&state_for_task)).await.unwrap();
    *state.health_cache.lock().unwrap() = Some(CachedHealthState {
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

    let mut out = format!(
        "# HELP zeroboot_total_executions Total number of user executions
\
         # TYPE zeroboot_total_executions counter
\
         zeroboot_total_executions {}
\
         # HELP zeroboot_total_errors Total number of execution errors
\
         # TYPE zeroboot_total_errors counter
\
         zeroboot_total_errors {}
\
         # HELP zeroboot_total_timeouts Total number of execution timeouts
\
         # TYPE zeroboot_total_timeouts counter
\
         zeroboot_total_timeouts {}
\
         # HELP zeroboot_protocol_errors Total number of protocol failures
\
         # TYPE zeroboot_protocol_errors counter
\
         zeroboot_protocol_errors {}
\
         # HELP zeroboot_output_truncations Total number of truncated responses
\
         # TYPE zeroboot_output_truncations counter
\
         zeroboot_output_truncations {}
\
         # HELP zeroboot_worker_recycles Total number of guest worker recycle requests observed
\
         # TYPE zeroboot_worker_recycles counter
\
         zeroboot_worker_recycles {}
\
         # HELP zeroboot_queue_rejections Total number of queue rejections
\
         # TYPE zeroboot_queue_rejections counter
\
         zeroboot_queue_rejections {}
\
         # HELP zeroboot_health_failures Total number of failed health probes
\
         # TYPE zeroboot_health_failures counter
\
         zeroboot_health_failures {}
\
         # HELP zeroboot_template_quarantines Total number of templates quarantined at startup
\
         # TYPE zeroboot_template_quarantines gauge
\
         zeroboot_template_quarantines {}
\
         # HELP zeroboot_concurrent_forks Current number of in-flight executions
\
         # TYPE zeroboot_concurrent_forks gauge
\
         zeroboot_concurrent_forks {}
\
         # HELP zeroboot_execution_slots_available Available execution slots in the admission semaphore
\
         # TYPE zeroboot_execution_slots_available gauge
\
         zeroboot_execution_slots_available {}
\
         # HELP zeroboot_execution_slots_used Execution slots currently consumed by in-flight or queued work
\
         # TYPE zeroboot_execution_slots_used gauge
\
         zeroboot_execution_slots_used {}
\
         # HELP zeroboot_execution_slots_capacity Total execution slot capacity
\
         # TYPE zeroboot_execution_slots_capacity gauge
\
         zeroboot_execution_slots_capacity {}
\
         # HELP zeroboot_memory_usage_bytes Resident memory usage for the zeroboot process
\
         # TYPE zeroboot_memory_usage_bytes gauge
\
         zeroboot_memory_usage_bytes {}
\
         # HELP zeroboot_fork_time_sum_milliseconds Sum of fork times in milliseconds
\
         # TYPE zeroboot_fork_time_sum_milliseconds counter
\
         zeroboot_fork_time_sum_milliseconds {}
\
         # HELP zeroboot_exec_time_sum_milliseconds Sum of exec times in milliseconds
\
         # TYPE zeroboot_exec_time_sum_milliseconds counter
\
         zeroboot_exec_time_sum_milliseconds {}
",
        total,
        errors,
        timeouts,
        m.protocol_errors.load(Ordering::Relaxed),
        m.output_truncations.load(Ordering::Relaxed),
        m.worker_recycles.load(Ordering::Relaxed),
        m.queue_rejections.load(Ordering::Relaxed),
        m.health_failures.load(Ordering::Relaxed),
        m.template_quarantines.load(Ordering::Relaxed),
        concurrent,
        available_slots,
        used_slots,
        total_slots,
        process_rss,
        fork_sum as f64 / 1000.0,
        exec_sum as f64 / 1000.0,
    );
    out.push_str(&m.fork_time_hist.render("zeroboot_fork_time_milliseconds", "Distribution of VM fork times in milliseconds"));
    out.push_str(&m.exec_time_hist.render("zeroboot_exec_time_milliseconds", "Distribution of guest execution times in milliseconds"));
    out.push_str(&m.total_time_hist.render("zeroboot_total_time_milliseconds", "Distribution of end-to-end request times in milliseconds"));
    out.push_str(&m.queue_wait_time_hist.render("zeroboot_queue_wait_time_milliseconds", "Distribution of queue wait time before execution in milliseconds"));
    for (language, status) in state.template_statuses.iter() {
        out.push_str(&format!(
            "# TYPE zeroboot_template_ready gauge\nzeroboot_template_ready{{language=\"{}\"}} {}\n",
            language,
            if status.ready { 1 } else { 0 }
        ));
    }
    let counters = m.language_counters.lock().unwrap();
    for (language, stats) in counters.iter() {
        out.push_str(&format!(
            "# TYPE zeroboot_language_executions_total counter\nzeroboot_language_executions_total{{language=\"{}\",result=\"ok\"}} {}\nzeroboot_language_executions_total{{language=\"{}\",result=\"error\"}} {}\nzeroboot_language_executions_total{{language=\"{}\",result=\"timeout\"}} {}\n",
            language, stats.ok, language, stats.error, language, stats.timeout
        ));
    }
    out
}

pub fn apply_request_log_path_fix(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}
