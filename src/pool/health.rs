use anyhow::{bail, Result};
use std::time::Duration;

use crate::protocol::{encode_request_frame, GuestRequest};

use super::types::ManagedVm;

pub fn health_code(language: &str) -> &'static str {
    match language {
        "node" => "console.log(\"ok\")",
        _ => "print(\"ok\")",
    }
}

pub fn probe_vm(vm: &mut dyn ManagedVm, language: &str, timeout: Duration) -> Result<()> {
    let frame = encode_request_frame(&GuestRequest {
        request_id: format!("pool-health-{}", language),
        language: language.to_string(),
        code: health_code(language).as_bytes().to_vec(),
        stdin: Vec::new(),
        timeout_ms: timeout.as_millis() as u64,
    });
    vm.send_serial(&frame)?;
    let response = vm.run_until_response_timeout(Some(timeout))?;
    if response.exit_code != 0 || response.error_type != "ok" {
        bail!(
            "health probe returned exit_code={} error_type={}",
            response.exit_code,
            response.error_type
        );
    }
    let stdout = String::from_utf8_lossy(&response.stdout);
    if stdout.trim() != "ok" {
        bail!("health probe stdout mismatch: {:?}", stdout.trim());
    }
    Ok(())
}
