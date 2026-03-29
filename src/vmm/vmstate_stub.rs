use anyhow::{bail, Result};

pub fn pre_restore_validate(
    _data: &[u8],
    _allowed_firecracker_version: Option<&str>,
    _expected_vcpu_count: Option<u32>,
) -> Result<()> {
    bail!("vmstate validation is only supported on Linux hosts")
}
