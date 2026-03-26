use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::{protocol, template_manifest};
use std::time::{Duration, Instant};

const FC_SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const GUEST_READY_PREFIX: &str = "ZEROBOOT_READY";

pub struct FirecrackerVm {
    process: Child,
    socket_path: String,
    snapshot_dir: String,
}

#[derive(Serialize)]
struct BootSource {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Serialize)]
struct MachineConfig {
    vcpu_count: u32,
    mem_size_mib: u32,
}

#[derive(Serialize)]
struct SnapshotCreate {
    snapshot_type: String,
    snapshot_path: String,
    mem_file_path: String,
}

#[derive(Serialize)]
struct VmAction {
    action_type: String,
}


impl FirecrackerVm {
    pub fn boot(kernel_path: &str, rootfs_path: &str, work_dir: &str, mem_mib: u32, init_path: &str) -> Result<Self> {
        let socket_path = format!("{}/firecracker.sock", work_dir);
        let snapshot_dir = format!("{}/snapshot", work_dir);
        let _ = std::fs::remove_file(&socket_path);
        std::fs::create_dir_all(&snapshot_dir)?;

        eprintln!("Starting Firecracker...");
        let process = Command::new("firecracker")
            .args(["--api-sock", &socket_path])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to start Firecracker")?;

        let start = Instant::now();
        while !Path::new(&socket_path).exists() {
            if start.elapsed() > Duration::from_secs(5) {
                bail!("Firecracker socket did not appear");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));

        let vm = Self { process, socket_path, snapshot_dir };
        vm.api_put("/machine-config", &MachineConfig { vcpu_count: 1, mem_size_mib: mem_mib })?;
        vm.api_put("/boot-source", &BootSource {
            kernel_image_path: kernel_path.to_string(),
            boot_args: format!("console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on init={}", init_path),
        })?;
        vm.api_put("/drives/rootfs", &Drive {
            drive_id: "rootfs".to_string(),
            path_on_host: rootfs_path.to_string(),
            is_root_device: true,
            is_read_only: false,
        })?;
        vm.api_put("/actions", &VmAction { action_type: "InstanceStart".to_string() })?;
        eprintln!("Firecracker VM started");
        Ok(vm)
    }

    pub fn wait_for_guest_ready(&mut self, timeout: Duration) -> Result<()> {
        let stdout = self.process.stdout.as_mut().context("Firecracker stdout pipe unavailable")?;
        let _reader = BufReader::new(stdout);
        let start = Instant::now();
        let mut line = String::new();
        loop {
            if start.elapsed() > timeout {
                let stderr_tail = self.read_stderr_tail();
                bail!("guest readiness handshake timed out after {:?}: {}", timeout, stderr_tail);
            }
            line.clear();
            let read_result = {
                let stdout = self.process.stdout.as_mut().context("Firecracker stdout pipe unavailable")?;
                let mut temp_reader = BufReader::new(stdout);
                temp_reader.read_line(&mut line)
            };
            // Check if process exited
            if let Ok(Some(status)) = self.process.try_wait() {
                bail!("Firecracker exited before guest became ready: {}", status);
            }
            match read_result {
                Ok(0) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        eprintln!("guest: {}", trimmed);
                    }
                    if trimmed.starts_with(GUEST_READY_PREFIX) {
                        return Ok(());
                    }
                }
                Err(e) => {
                    std::thread::sleep(Duration::from_millis(20));
                    if start.elapsed() > timeout {
                        bail!("guest readiness read failed: {}", e);
                    }
                }
            }
        }
    }

    pub fn snapshot(&mut self) -> Result<(String, String)> {
        let snapshot_path = format!("{}/vmstate", self.snapshot_dir);
        let mem_path = format!("{}/mem", self.snapshot_dir);
        eprintln!("Pausing VM...");
        self.api_patch("/vm", &serde_json::json!({"state": "Paused"}))?;
        eprintln!("Creating snapshot...");
        self.api_put("/snapshot/create", &SnapshotCreate {
            snapshot_type: "Full".to_string(),
            snapshot_path: snapshot_path.clone(),
            mem_file_path: mem_path.clone(),
        })?;
        std::thread::sleep(Duration::from_millis(500));
        if !Path::new(&snapshot_path).exists() { bail!("Snapshot state file not created"); }
        if !Path::new(&mem_path).exists() { bail!("Snapshot memory file not created"); }
        let mem_size = std::fs::metadata(&mem_path)?.len();
        eprintln!("Snapshot created: state={}B, mem={}MB", std::fs::metadata(&snapshot_path)?.len(), mem_size / 1024 / 1024);
        Ok((snapshot_path, mem_path))
    }

    fn api_put<T: Serialize>(&self, path: &str, body: &T) -> Result<String> { self.api_request("PUT", path, body) }
    fn api_patch<T: Serialize>(&self, path: &str, body: &T) -> Result<String> { self.api_request("PATCH", path, body) }

    fn api_request<T: Serialize>(&self, method: &str, path: &str, body: &T) -> Result<String> {
        let body_json = serde_json::to_string(body)?;
        let request = format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            method, path, body_json.len(), body_json
        );
        let mut stream = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("Connect to Firecracker socket at {}", self.socket_path))?;
        stream.set_read_timeout(Some(FC_SOCKET_TIMEOUT))?;
        stream.set_write_timeout(Some(FC_SOCKET_TIMEOUT))?;
        stream.write_all(request.as_bytes())?;
        stream.flush()?;
        let mut response = vec![0u8; 4096];
        let n = stream.read(&mut response)?;
        let resp = String::from_utf8_lossy(&response[..n]).to_string();
        if !resp.contains("204") && !resp.contains("200") {
            bail!("Firecracker API error on {} {}: {}", method, path, resp);
        }
        Ok(resp)
    }

    fn read_stderr_tail(&mut self) -> String {
        let mut buf = String::new();
        if let Some(stderr) = self.process.stderr.as_mut() {
            let _ = stderr.read_to_string(&mut buf);
        }
        buf.trim().to_string()
    }

    pub fn kill(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl Drop for FirecrackerVm {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub fn create_template_snapshot(
    kernel_path: &str,
    rootfs_path: &str,
    work_dir: &str,
    mem_mib: u32,
    wait_secs: u64,
    init_path: &str,
) -> Result<(String, String, u32)> {
    let mut vm = FirecrackerVm::boot(kernel_path, rootfs_path, work_dir, mem_mib, init_path)?;
    eprintln!("Waiting for guest readiness handshake...");
    vm.wait_for_guest_ready(Duration::from_secs(wait_secs))?;
    let (state_path, mem_path) = vm.snapshot()?;
    let state_bytes = std::fs::metadata(&state_path)?.len();
    let mem_bytes = std::fs::metadata(&mem_path)?.len();
    let manifest = template_manifest::TemplateManifest {
        language: Some(infer_language_from_rootfs(rootfs_path)),
        kernel_path: kernel_path.to_string(),
        kernel_sha256: Some(template_manifest::sha256_hex(Path::new(kernel_path))?),
        rootfs_path: rootfs_path.to_string(),
        rootfs_sha256: Some(template_manifest::sha256_hex(Path::new(rootfs_path))?),
        init_path: init_path.to_string(),
        mem_size_mib: mem_mib,
        snapshot_state_path: state_path.clone(),
        snapshot_mem_path: mem_path.clone(),
        snapshot_state_bytes: state_bytes,
        snapshot_mem_bytes: mem_bytes,
        snapshot_state_sha256: Some(template_manifest::sha256_hex(Path::new(&state_path))?),
        snapshot_mem_sha256: Some(template_manifest::sha256_hex(Path::new(&mem_path))?),
        firecracker_version: firecracker_version(),
        protocol_version: Some(protocol::PROTOCOL_VERSION.to_string()),
        vcpu_count: Some(1),
        created_at_unix_ms: Some(current_unix_ms()),
    };
    let manifest_path = format!("{}/template.manifest.json", work_dir);
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    eprintln!("Wrote template manifest to {}", manifest_path);
    Ok((state_path, mem_path, mem_mib))
}

fn infer_language_from_rootfs(rootfs_path: &str) -> String {
    let lower = rootfs_path.to_ascii_lowercase();
    if lower.contains("node") || lower.contains("javascript") {
        "node".to_string()
    } else {
        "python".to_string()
    }
}

fn firecracker_version() -> Option<String> {
    let output = Command::new("firecracker").arg("--version").output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    if text.is_empty() { None } else { Some(text) }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
