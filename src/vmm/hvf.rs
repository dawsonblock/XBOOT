// macOS Hypervisor.framework VMM implementation
// Provides a KVM-compatible interface using Apple's Hypervisor.framework
//
// NOTE: This is a stub implementation for Apple Silicon (ARM64).
// The actual Hypervisor.framework API on ARM64 is different from x86_64
// and requires additional research and proper FFI bindings.
// For now, this provides the interface but will error at runtime.

use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

use super::serial::Serial;
use crate::protocol::{find_response_frame, GuestResponse};

/// VM configuration for macOS
/// Note: On macOS, this represents a fresh boot configuration,
/// not a snapshot like on Linux/KVM
#[derive(Clone)]
pub struct VmSnapshot {
    pub mem_size: usize,
    pub kernel_path: Option<String>,
    pub rootfs_path: Option<String>,
    pub init_path: Option<String>,
}

/// A running VM instance using Hypervisor.framework
pub struct ForkedVm {
    pub vcpu_id: u32,
    pub mem_ptr: *mut u8,
    pub mem_size: usize,
    pub serial: Serial,
    pub fork_time_us: f64,
    pub kernel_entry: u64,
}

// SAFETY: ForkedVm is safe to send between threads as long as
// hypervisor framework operations are thread-safe
unsafe impl Send for ForkedVm {}

impl Drop for ForkedVm {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.mem_ptr as *mut libc::c_void, self.mem_size);
        }
    }
}

impl ForkedVm {
    /// Create a new VM using Hypervisor.framework
    /// On macOS, this boots a fresh VM from kernel/rootfs instead of
    /// restoring from a snapshot (since HVF doesn't support snapshot/restore)
    pub fn fork_cow(snapshot: &VmSnapshot, _memfd: i32) -> Result<Self> {
        let start = Instant::now();

        // STUB: Hypervisor.framework on ARM64 requires proper FFI bindings
        // For now, we return an error directing users to use the Firecracker/QEMU path
        bail!(
            "macOS Hypervisor.framework backend is not yet fully implemented.\n\
             On macOS, please use one of these alternatives:\n\
             1. Run on a Linux VM with KVM (UTM, VMware Fusion)\n\
             2. Use cloud instances with KVM support\n\
             3. Use QEMU with Hypervisor.framework:\n\
                qemu-system-aarch64 -accel hvf -m 512 -kernel kernel -drive file=rootfs.ext4"
        )
    }

    /// Send data to guest via serial port
    pub fn send_serial(&mut self, data: &[u8]) -> Result<()> {
        self.serial.queue_input(data);
        self.serial.set_ier_data_ready(true);
        Ok(())
    }

    /// Run vCPU until response or timeout
    pub fn run_until_response_timeout(
        &mut self,
        _timeout: Option<Duration>,
    ) -> Result<GuestResponse> {
        bail!("macOS Hypervisor.framework backend is not yet fully implemented")
    }
}

/// Create a memfd for snapshot (stub on macOS)
/// On macOS, we don't support snapshots, so this returns an error
pub fn create_snapshot_memfd(_src: *const u8, _len: usize) -> Result<i32> {
    // macOS doesn't support memfd_create or snapshot/restore
    // Return a dummy fd that will be ignored
    bail!("snapshot memfd creation is only supported on Linux hosts with KVM")
}
