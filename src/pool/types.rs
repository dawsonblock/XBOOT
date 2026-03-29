use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::protocol::GuestResponse;
use crate::vmm::kvm::{ForkedVm, VmSnapshot};

pub trait ManagedVm: Send {
    fn send_serial(&mut self, data: &[u8]) -> Result<()>;
    fn run_until_response_timeout(&mut self, timeout: Option<Duration>) -> Result<GuestResponse>;
    fn fork_time_us(&self) -> f64;
}

impl ManagedVm for ForkedVm {
    fn send_serial(&mut self, data: &[u8]) -> Result<()> {
        ForkedVm::send_serial(self, data)
    }

    fn run_until_response_timeout(&mut self, timeout: Option<Duration>) -> Result<GuestResponse> {
        ForkedVm::run_until_response_timeout(self, timeout)
    }

    fn fork_time_us(&self) -> f64 {
        self.fork_time_us
    }
}

pub trait VmFactory: Send + Sync {
    fn create(&self, language: &str) -> Result<Box<dyn ManagedVm>>;
}

#[derive(Clone)]
pub struct TemplateRuntime {
    pub snapshot: Arc<VmSnapshot>,
    pub memfd: i32,
    pub workdir: String,
}

pub struct KvmVmFactory {
    templates: HashMap<String, TemplateRuntime>,
}

impl KvmVmFactory {
    pub fn new(templates: HashMap<String, TemplateRuntime>) -> Self {
        Self { templates }
    }
}

impl VmFactory for KvmVmFactory {
    fn create(&self, language: &str) -> Result<Box<dyn ManagedVm>> {
        let template = self
            .templates
            .get(language)
            .ok_or_else(|| anyhow::anyhow!("no pooled template for language {}", language))?;
        let vm = ForkedVm::fork_cow(template.snapshot.as_ref(), template.memfd)?;
        Ok(Box::new(vm))
    }
}

pub struct PooledVm {
    pub id: u64,
    pub language: String,
    pub vm: Box<dyn ManagedVm>,
    pub request_count: u32,
    pub cumulative_exec_ms: u64,
    pub last_health_probe: Instant,
    pub timeout_count: u32,
    pub signal_death_count: u32,
    pub recycle_generation: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolEventType {
    Created,
    Borrowed,
    ReturnedIdle,
    Recycled,
    Quarantined,
    Scaled,
    HealthProbePassed,
    HealthProbeFailed,
    BorrowTimedOut,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolEvent {
    pub ts: u64,
    pub language: String,
    pub event_type: PoolEventType,
    pub reason: String,
    pub vm_id: Option<u64>,
    pub details: String,
}

impl PoolEvent {
    pub fn new(
        language: impl Into<String>,
        event_type: PoolEventType,
        reason: impl Into<String>,
        vm_id: Option<u64>,
        details: impl Into<String>,
    ) -> Self {
        Self {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            language: language.into(),
            event_type,
            reason: reason.into(),
            vm_id,
            details: details.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct LaneSnapshot {
    pub idle: usize,
    pub active: usize,
    pub target_idle: usize,
    pub waiters: usize,
    pub healthy_idle: usize,
    pub quarantined: u64,
    pub avg_borrow_latency_ms: f64,
    pub recent_recycles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PoolStatusSnapshot {
    pub generated_at: u64,
    pub status: String,
    pub lanes: HashMap<String, LaneSnapshot>,
}

pub fn pool_status_now(lanes: HashMap<String, LaneSnapshot>, healthy: bool) -> PoolStatusSnapshot {
    PoolStatusSnapshot {
        generated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        status: if healthy {
            "ok".into()
        } else {
            "degraded".into()
        },
        lanes,
    }
}

pub fn check_language_known(language: &str) -> Result<()> {
    match language {
        "python" | "node" => Ok(()),
        other => bail!("unknown pool language {}", other),
    }
}
