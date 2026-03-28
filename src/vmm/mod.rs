pub mod firecracker;
pub mod serial;
#[cfg(target_os = "linux")]
pub mod vmstate;
#[cfg(not(target_os = "linux"))]
pub mod vmstate_stub;
#[cfg(not(target_os = "linux"))]
pub use vmstate_stub as vmstate;

#[cfg(target_os = "linux")]
pub mod kvm;
#[cfg(not(target_os = "linux"))]
pub mod kvm_stub;
#[cfg(not(target_os = "linux"))]
pub use kvm_stub as kvm;
