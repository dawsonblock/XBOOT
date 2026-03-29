use serde::Serialize;

use crate::config::PoolConfig;

use super::types::PooledVm;

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RecycleReason {
    Manual,
    IdleOverflow,
    RequestLimit,
    ExecBudget,
    GuestRequested,
    HostTimeout,
    ProtocolFailure,
    TransportFailure,
    HealthProbeFailure,
    ChildSignalDeath,
    LeaseDropped,
}

impl RecycleReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::IdleOverflow => "idle_overflow",
            Self::RequestLimit => "request_limit",
            Self::ExecBudget => "exec_budget",
            Self::GuestRequested => "guest_requested",
            Self::HostTimeout => "host_timeout",
            Self::ProtocolFailure => "protocol_failure",
            Self::TransportFailure => "transport_failure",
            Self::HealthProbeFailure => "health_probe_failure",
            Self::ChildSignalDeath => "child_signal_death",
            Self::LeaseDropped => "lease_dropped",
        }
    }
}

pub fn lifecycle_cap_reason(vm: &PooledVm, config: &PoolConfig) -> Option<RecycleReason> {
    if vm.request_count >= config.max_requests_per_vm {
        return Some(RecycleReason::RequestLimit);
    }
    if vm.cumulative_exec_ms >= config.max_cumulative_exec_ms_per_vm {
        return Some(RecycleReason::ExecBudget);
    }
    None
}
