use std::sync::Arc;

use super::manager::PoolLane;
use super::policy::RecycleReason;
use super::types::{ManagedVm, PooledVm};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseOutcome {
    ReturnedIdle,
    Recycled { reason: RecycleReason },
    Quarantined { reason: RecycleReason },
}

pub struct VmLease {
    lane: Arc<PoolLane>,
    vm: Option<PooledVm>,
}

impl VmLease {
    pub(crate) fn new(lane: Arc<PoolLane>, vm: PooledVm) -> Self {
        Self { lane, vm: Some(vm) }
    }

    pub fn vm_mut(&mut self) -> &mut dyn ManagedVm {
        self.vm
            .as_mut()
            .expect("lease accessed after finalize")
            .vm
            .as_mut()
    }

    pub fn vm_id(&self) -> u64 {
        self.vm.as_ref().expect("lease accessed after finalize").id
    }

    pub fn language(&self) -> &str {
        &self
            .vm
            .as_ref()
            .expect("lease accessed after finalize")
            .language
    }

    pub fn record_exec(&mut self, exec_ms: u64) {
        let vm = self.vm.as_mut().expect("lease accessed after finalize");
        vm.request_count = vm.request_count.saturating_add(1);
        vm.cumulative_exec_ms = vm.cumulative_exec_ms.saturating_add(exec_ms);
    }

    pub fn record_timeout(&mut self) {
        let vm = self.vm.as_mut().expect("lease accessed after finalize");
        vm.timeout_count = vm.timeout_count.saturating_add(1);
    }

    pub fn record_signal_death(&mut self) {
        let vm = self.vm.as_mut().expect("lease accessed after finalize");
        vm.signal_death_count = vm.signal_death_count.saturating_add(1);
    }

    pub fn finish(mut self, outcome: LeaseOutcome) {
        if let Some(vm) = self.vm.take() {
            self.lane.finalize_vm(vm, outcome);
        }
    }
}

impl Drop for VmLease {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            self.lane.finalize_vm(
                vm,
                LeaseOutcome::Quarantined {
                    reason: RecycleReason::LeaseDropped,
                },
            );
        }
    }
}
