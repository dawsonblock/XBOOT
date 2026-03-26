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

/// Parsed guest readiness information from handshake
#[derive(Debug, Clone)]
pub struct GuestReady {
    pub protocol_version: String,
    pub worker_python: bool,
    pub worker_node: bool,
}

/// Parse the guest ready line: "ZEROBOOT_READY proto=ZB1 worker_python=1 worker_node=1"
fn parse_guest_ready_line(line: &str) -> Result<GuestReady> {
    let mut proto = None;
    let mut worker_python = false;
    let mut worker_node = false;

    let mut parts = line.split_ascii_whitespace();
    let prefix = parts.next().unwrap_or_default();
    if prefix != GUEST_READY_PREFIX {
        bail!("not a ready line");
    }

    for part in parts {
        if let Some(v) = part.strip_prefix("proto=") {
            proto = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("worker_python=") {
            worker_python = v == "1";
        } else if let Some(v) = part.strip_prefix("worker_node=") {
            worker_node = v == "1";
        }
    }

    let proto = proto.ok_or_else(|| anyhow::anyhow!("ready line missing proto"))?;
    Ok(GuestReady { protocol_version: proto, worker_python, worker_node })
}

pub struct FirecrackerVm {
    process: Child,
    socket_path: String,
    snapshot_dir: String,
    stdout_reader: Option<BufReader<Box<dyn Read + Send>>>, // Persistent reader for guest output
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

        let vm = Self { 
            process, 
            socket_path, 
            snapshot_dir,
            stdout_reader: None, // Will be created on first use
        };
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

    /// Wait for guest to signal readiness with explicit protocol handshake.
    /// Returns parsed GuestReady information with protocol version and available workers.
    pub fn wait_for_guest_ready(&mut self, timeout: Duration) -> Result<GuestReady> {
        let start = Instant::now();
        let mut line = String::with_capacity(256);
        
        // Initialize persistent reader on first call
        if self.stdout_reader.is_none() {
            let stdout = self.process.stdout.take()
                .context("Firecracker stdout pipe unavailable")?;
            self.stdout_reader = Some(BufReader::new(Box::new(stdout)));
        }
        
        loop {
            // Check if process exited FIRST (before any borrow)
            if let Ok(Some(status)) = self.process.try_wait() {
                bail!("Firecracker exited before guest became ready: {}", status);
            }
            
            if start.elapsed() > timeout {
                let stderr_tail = self.read_stderr_tail();
                bail!("guest readiness handshake timed out after {:?}: {}", timeout, stderr_tail);
            }
            
            // Use persistent reader instead of creating new one each iteration
            let reader = self.stdout_reader.as_mut()
                .context("stdout reader not initialized")?;
            
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    // EOF - guest may have closed stdout, check if process exited
                    if let Ok(Some(status)) = self.process.try_wait() {
                        bail!("Firecracker process exited during guest ready wait: {}", status);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        eprintln!("guest: {}", trimmed);
                    }
                    if trimmed.starts_with(GUEST_READY_PREFIX) {
                        // Parse the ready line to extract protocol and worker info
                        let ready = parse_guest_ready_line(trimmed)?;
                        
                        // Validate protocol version matches our expected version
                        if ready.protocol_version != protocol::PROTOCOL_VERSION {
                            bail!(
                                "protocol version mismatch: guest sent '{}', we expect '{}'",
                                ready.protocol_version,
                                protocol::PROTOCOL_VERSION
                            );
                        }
                        
                        eprintln!("Guest ready: proto={}, python={}, node={}", 
                            ready.protocol_version, ready.worker_python, ready.worker_node);
                        return Ok(ready);
                    }
                }
                Err(e) => {
                    // WouldBlock or other error - this is expected when no data available
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

    /// Parse HTTP response and extract status code properly.
    /// Returns Ok(status_code) on success, or error with details on failure.
    fn parse_http_response(resp: &str) -> Result<u16> {
        // Find the status line (first line)
        let status_line = resp.lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty HTTP response"))?;
        
        // Parse "HTTP/1.1 XXX ..." 
        let parts: Vec<&str> = status_line.split_whitespace().collect();
        if parts.len() < 2 {
            bail!("malformed HTTP status line: {}", status_line);
        }
        
        let status_code: u16 = parts[1].parse()
            .map_err(|_| anyhow::anyhow!("invalid status code: {}", parts[1]))?;
        
        Ok(status_code)
    }

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
        
        // Parse HTTP response properly instead of naive string contains
        let status_code = Self::parse_http_response(&resp)?;
        
        // Consider 2xx status codes as success
        if !(200..=299).contains(&status_code) {
            bail!("Firecracker API error on {} {}: HTTP {} - {}", method, path, status_code, resp);
        }
        Ok(resp)
    }

    /// Read stderr tail with bounded reads to avoid blocking.
    /// Returns at most 4KB of stderr content.
    fn read_stderr_tail(&mut self) -> String {
        let mut buf = vec![0u8; 4096]; // 4KB max
        if let Some(stderr) = self.process.stderr.as_mut() {
            // Use non-blocking read with timeout
            match stderr.read(&mut buf) {
                Ok(n) => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        }
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
    let ready = vm.wait_for_guest_ready(Duration::from_secs(wait_secs))?;
    let (state_path, mem_path) = vm.snapshot()?;
    let state_bytes = std::fs::metadata(&state_path)?.len();
    let mem_bytes = std::fs::metadata(&mem_path)?.len();
    let fc_version = firecracker_version();
    let manifest = template_manifest::TemplateManifest {
        // Core identity fields
        schema_version: Some(1),
        template_id: Some(uuid::Uuid::new_v4().to_string()),
        build_id: Some(uuid::Uuid::new_v4().to_string()),
        artifact_set_id: Some(uuid::Uuid::new_v4().to_string()),
        
        // Trust and promotion (default to dev for newly created templates)
        promotion_channel: Some("dev".to_string()),
        
        // Trust - newly created templates are not yet signed
        signer_key_id: None,
        manifest_signature: None,
        manifest_signed_fields: None,
        
        // Build provenance
        built_from_git_rev: std::env::var("ZEROBOOT_GIT_REV").ok(),
        build_host: std::env::var("ZEROBOOT_BUILD_HOST").ok(),
        firecracker_binary_sha256: std::env::var("ZEROBOOT_FC_BINARY_SHA256").ok(),
        
        // Original fields
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
        firecracker_version: fc_version,
        protocol_version: Some(ready.protocol_version), // Use guest's reported protocol version
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
