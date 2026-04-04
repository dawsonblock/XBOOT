use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::{protocol, startup, template_manifest};
use std::time::{Duration, Instant};

const FC_SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const GUEST_READY_PREFIX: &str = "ZEROBOOT_READY";
const MAX_READY_LINE_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct GuestReady {
    pub protocol_version: String,
    pub worker_python: bool,
    pub worker_node: bool,
}

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
    Ok(GuestReady {
        protocol_version: proto,
        worker_python,
        worker_node,
    })
}

fn fcntl_getfl(fd: i32) -> Result<i32> {
    loop {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            return Ok(flags);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            continue;
        }
        return Err(err.into());
    }
}

fn fcntl_setfl(fd: i32, flags: i32) -> Result<()> {
    loop {
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        if rc >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            continue;
        }
        return Err(err.into());
    }
}

fn set_nonblocking<T: AsRawFd>(stream: &T, label: &str) -> Result<()> {
    let fd = stream.as_raw_fd();
    let flags = fcntl_getfl(fd).with_context(|| format!("failed to get {} flags", label))?;
    fcntl_setfl(fd, flags | libc::O_NONBLOCK)
        .with_context(|| format!("failed to set {} nonblocking", label))?;
    Ok(())
}

fn terminate_child(process: &mut Child) {
    let _ = process.kill();
    let _ = process.wait();
}

pub struct FirecrackerVm {
    process: Child,
    socket_path: String,
    snapshot_dir: String,
    stdout_reader: Option<BufReader<Box<dyn Read + Send>>>,
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
    pub fn boot(
        kernel_path: &str,
        rootfs_path: &str,
        work_dir: &str,
        mem_mib: u32,
        init_path: &str,
    ) -> Result<Self> {
        let socket_path = format!("{}/firecracker.sock", work_dir);
        let snapshot_dir = format!("{}/snapshot", work_dir);
        let _ = std::fs::remove_file(&socket_path);
        std::fs::create_dir_all(&snapshot_dir)?;
        let firecracker_bin = startup::resolved_firecracker_binary()?;

        eprintln!("Starting Firecracker...");
        let mut process = Command::new(&firecracker_bin)
            .args(["--api-sock", &socket_path])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to start Firecracker via {}",
                    firecracker_bin.display()
                )
            })?;

        if let Some(stdout) = process.stdout.as_ref() {
            if let Err(err) = set_nonblocking(stdout, "firecracker stdout") {
                terminate_child(&mut process);
                return Err(err);
            }
        }
        if let Some(stderr) = process.stderr.as_ref() {
            if let Err(err) = set_nonblocking(stderr, "firecracker stderr") {
                terminate_child(&mut process);
                return Err(err);
            }
        }

        let start = Instant::now();
        while !Path::new(&socket_path).exists() {
            if start.elapsed() > Duration::from_secs(5) {
                terminate_child(&mut process);
                bail!("Firecracker socket did not appear");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));

        let vm = Self {
            process,
            socket_path,
            snapshot_dir,
            stdout_reader: None,
        };
        vm.api_put(
            "/machine-config",
            &MachineConfig {
                vcpu_count: 1,
                mem_size_mib: mem_mib,
            },
        )?;
        vm.api_put(
            "/boot-source",
            &BootSource {
                kernel_image_path: kernel_path.to_string(),
                boot_args: format!(
                    "console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on init={}",
                    init_path
                ),
            },
        )?;
        vm.api_put(
            "/drives/rootfs",
            &Drive {
                drive_id: "rootfs".to_string(),
                path_on_host: rootfs_path.to_string(),
                is_root_device: true,
                is_read_only: false,
            },
        )?;
        vm.api_put(
            "/actions",
            &VmAction {
                action_type: "InstanceStart".to_string(),
            },
        )?;
        eprintln!("Firecracker VM started");
        Ok(vm)
    }

    pub fn wait_for_guest_ready(&mut self, timeout: Duration) -> Result<GuestReady> {
        let start = Instant::now();
        let mut line = String::with_capacity(256);

        if self.stdout_reader.is_none() {
            let stdout = self
                .process
                .stdout
                .take()
                .context("Firecracker stdout pipe unavailable")?;
            self.stdout_reader = Some(BufReader::new(Box::new(stdout)));
        }

        loop {
            if let Ok(Some(status)) = self.process.try_wait() {
                bail!("Firecracker exited before guest became ready: {}", status);
            }

            if start.elapsed() > timeout {
                let stderr_tail = self.read_stderr_tail();
                let stderr_detail = if stderr_tail.is_empty() {
                    "no stderr output captured".to_string()
                } else {
                    stderr_tail
                };
                bail!(
                    "guest readiness handshake timed out after {:?}: {}",
                    timeout,
                    stderr_detail
                );
            }

            let reader = self
                .stdout_reader
                .as_mut()
                .context("stdout reader not initialized")?;

            match reader.read_line(&mut line) {
                Ok(0) => {
                    if let Ok(Some(status)) = self.process.try_wait() {
                        bail!(
                            "Firecracker process exited during guest ready wait: {}",
                            status
                        );
                    }
                    if line.len() > MAX_READY_LINE_BYTES {
                        bail!("guest readiness line exceeded {} bytes", MAX_READY_LINE_BYTES);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(_) => {
                    if line.len() > MAX_READY_LINE_BYTES {
                        bail!("guest readiness line exceeded {} bytes", MAX_READY_LINE_BYTES);
                    }
                    if !line.ends_with('\n') {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }

                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        eprintln!("guest: {}", trimmed);
                    }
                    if trimmed.starts_with(GUEST_READY_PREFIX) {
                        let ready = parse_guest_ready_line(trimmed)?;
                        if ready.protocol_version != protocol::PROTOCOL_VERSION {
                            bail!(
                                "protocol version mismatch: guest sent '{}', we expect '{}'",
                                ready.protocol_version,
                                protocol::PROTOCOL_VERSION
                            );
                        }

                        eprintln!(
                            "Guest ready: proto={}, python={}, node={}",
                            ready.protocol_version, ready.worker_python, ready.worker_node
                        );
                        return Ok(ready);
                    }
                    line.clear();
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    bail!("guest readiness read failed: {}", e);
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
        self.api_put(
            "/snapshot/create",
            &SnapshotCreate {
                snapshot_type: "Full".to_string(),
                snapshot_path: snapshot_path.clone(),
                mem_file_path: mem_path.clone(),
            },
        )?;
        std::thread::sleep(Duration::from_millis(500));
        if !Path::new(&snapshot_path).exists() {
            bail!("Snapshot state file not created");
        }
        if !Path::new(&mem_path).exists() {
            bail!("Snapshot memory file not created");
        }
        let mem_size = std::fs::metadata(&mem_path)?.len();
        eprintln!(
            "Snapshot created: state={}B, mem={}MB",
            std::fs::metadata(&snapshot_path)?.len(),
            mem_size / 1024 / 1024
        );
        Ok((snapshot_path, mem_path))
    }

    fn api_put<T: Serialize>(&self, path: &str, body: &T) -> Result<String> {
        self.api_request("PUT", path, body)
    }

    fn api_patch<T: Serialize>(&self, path: &str, body: &T) -> Result<String> {
        self.api_request("PATCH", path, body)
    }

    fn parse_http_response(resp: &str) -> Result<u16> {
        let status_line = resp
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty HTTP response"))?;

        let parts: Vec<&str> = status_line.split_whitespace().collect();
        if parts.len() < 2 {
            bail!("malformed HTTP status line: {}", status_line);
        }

        let status_code: u16 = parts[1]
            .parse()
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

        let status_code = Self::parse_http_response(&resp)?;

        if !(200..=299).contains(&status_code) {
            bail!(
                "Firecracker API error on {} {}: HTTP {} - {}",
                method,
                path,
                status_code,
                resp
            );
        }
        Ok(resp)
    }

    fn read_stderr_tail(&mut self) -> String {
        let mut collected = Vec::with_capacity(4096);
        if let Some(stderr) = self.process.stderr.as_mut() {
            let mut buf = [0u8; 1024];
            while collected.len() < 4096 {
                let remaining = 4096 - collected.len();
                let slice_len = remaining.min(buf.len());
                match stderr.read(&mut buf[..slice_len]) {
                    Ok(0) => break,
                    Ok(n) => collected.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        String::from_utf8_lossy(&collected).trim().to_string()
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
        schema_version: Some(1),
        template_id: Some(uuid::Uuid::new_v4().to_string()),
        build_id: Some(uuid::Uuid::new_v4().to_string()),
        artifact_set_id: Some(uuid::Uuid::new_v4().to_string()),
        promotion_channel: Some("dev".to_string()),
        signer_key_id: None,
        manifest_signature: None,
        manifest_signed_fields: None,
        built_from_git_rev: std::env::var("ZEROBOOT_GIT_REV").ok(),
        build_host: std::env::var("ZEROBOOT_BUILD_HOST").ok(),
        firecracker_binary_sha256: firecracker_binary_sha256()
            .or_else(|| std::env::var("ZEROBOOT_FC_BINARY_SHA256").ok()),
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
        protocol_version: Some(ready.protocol_version),
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
    let binary = startup::resolved_firecracker_binary().ok()?;
    let output = Command::new(binary).arg("--version").output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn firecracker_binary_sha256() -> Option<String> {
    let binary = startup::resolved_firecracker_binary().ok()?;
    template_manifest::sha256_hex(&binary).ok()
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
