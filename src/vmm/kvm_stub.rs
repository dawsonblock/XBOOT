use anyhow::{bail, Result};
use std::time::Duration;

use crate::protocol::GuestResponse;

#[derive(Clone)]
pub struct VmSnapshot {
    pub mem_size: usize,
}

pub struct ForkedVm {
    pub fork_time_us: f64,
}

impl Drop for ForkedVm {
    fn drop(&mut self) {}
}

impl ForkedVm {
    pub fn fork_cow(_snapshot: &VmSnapshot, _memfd: i32) -> Result<Self> {
        bail!("KVM restore is only supported on Linux hosts with /dev/kvm")
    }

    pub fn send_serial(&mut self, _data: &[u8]) -> Result<()> {
        bail!("serial transport is unavailable on non-Linux builds")
    }

    pub fn run_until_response_timeout(
        &mut self,
        _timeout: Option<Duration>,
    ) -> Result<GuestResponse> {
        bail!("guest execution is unavailable on non-Linux builds")
    }
}

pub fn create_snapshot_memfd(_src: *const u8, _len: usize) -> Result<i32> {
    bail!("snapshot memfd creation is only supported on Linux hosts")
}
