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
#[cfg(target_os = "macos")]
pub mod hvf;
#[cfg(target_os = "macos")]
pub use hvf as kvm;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub mod kvm_stub;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use kvm_stub as kvm;
