mod api;
mod auth;
mod config;
mod protocol;
mod signing;
mod startup;
mod template_manifest;
mod vmm;

use anyhow::{bail, Context, Result};
use axum::extract::DefaultBodyLimit;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::time::Instant;

use api::handlers::{
    apply_request_log_path_fix, batch_handler, exec_handler, health_handler, live_handler,
    metrics_handler, ready_handler, AppState, Metrics, Template,
};
use config::{AuthMode, ServerConfig};
use protocol::GuestRequest;
use template_manifest::ManifestPolicy;
#[cfg(target_os = "linux")]
use template_manifest::VerificationMode;
use vmm::firecracker;
#[cfg(target_os = "linux")]
use vmm::kvm::create_snapshot_memfd;
use vmm::kvm::{ForkedVm, VmSnapshot};
#[cfg(target_os = "linux")]
use vmm::vmstate;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    match command {
        "template" => cmd_template(&args[2..]),
        "bench" | "fork-bench" => cmd_fork_bench(&args[2..]),
        "serve" => cmd_serve(&args[2..]),
        "test-exec" => cmd_test_exec(&args[2..]),
        "verify-startup" => cmd_verify_startup(&args[2..]),
        "promote-template" => cmd_promote_template(&args[2..]),
        "sign" => cmd_sign(&args[2..]),
        "keygen" => cmd_keygen(&args[2..]),
        _ => {
            eprintln!("Usage: zeroboot <command>");
            eprintln!("  template <kernel> <rootfs> <workdir> [wait_secs] [init_path] [mem_mib]");
            eprintln!("  bench <workdir> [language]");
            eprintln!("  test-exec <workdir> [language] <code>");
            eprintln!("  serve <workdir>[,lang:workdir2,...] [port]");
            eprintln!("  verify-startup <workdir>[,lang:workdir2,...] [--release-root <path>]");
            eprintln!("  promote-template <manifest> --channel <dev|staging|prod> --key <path> --key-id <id> [--out <path>] [--receipt <path>]");
            eprintln!("  sign <key> <manifest>");
            eprintln!("  keygen");
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
struct PromotionReceipt {
    manifest_path: String,
    template_id: Option<String>,
    artifact_set_id: Option<String>,
    previous_channel: Option<String>,
    promotion_channel: String,
    signer_key_id: String,
    signed_fields: Vec<String>,
    signature_sha256: String,
    promoted_at_unix_ms: u64,
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("atomic write target must have a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temp file for {}", path.display()))?;
    tmp.write_all(bytes)?;
    tmp.flush()?;
    tmp.as_file().sync_all().ok();
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("persist {}: {}", path.display(), e.error))?;
    Ok(())
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Build a ManifestPolicy from optional config.
/// This centralizes verification policy construction.
fn manifest_policy(config: Option<&ServerConfig>) -> ManifestPolicy<'_> {
    match config {
        Some(cfg) => ManifestPolicy::from_config(cfg),
        None => ManifestPolicy::dev(),
    }
}

fn validate_snapshot_workdir(
    workdir: &str,
    expected_language: Option<&str>,
    config: Option<&ServerConfig>,
) -> Result<()> {
    let workdir_path = Path::new(workdir);
    let mut policy = manifest_policy(config);
    policy.expected_language = expected_language;
    template_manifest::verify_template_artifacts_with_policy(workdir_path, &policy)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn load_snapshot(
    workdir: &str,
    expected_language: Option<&str>,
    config: Option<&ServerConfig>,
) -> Result<(VmSnapshot, i32)> {
    // First validate and get the manifest to know the actual paths
    let workdir_path = Path::new(workdir);
    let mut policy = manifest_policy(config);
    policy.expected_language = expected_language;

    let manifest = template_manifest::verify_template_artifacts_with_policy(workdir_path, &policy)?;

    // Use paths from manifest, resolved with confinement in prod mode
    let (mem_path, state_path) = if policy.mode == VerificationMode::Prod {
        let mem =
            template_manifest::resolve_path_confined(workdir_path, &manifest.snapshot_mem_path)?;
        let state =
            template_manifest::resolve_path_confined(workdir_path, &manifest.snapshot_state_path)?;
        (mem, state)
    } else {
        (
            template_manifest::resolve_path(workdir_path, &manifest.snapshot_mem_path),
            template_manifest::resolve_path(workdir_path, &manifest.snapshot_state_path),
        )
    };

    eprintln!("Loading snapshot from {}...", workdir);
    let mem_data = std::fs::read(&mem_path)?;
    let mem_size = mem_data.len();
    eprintln!("  Memory: {} MiB", mem_size / 1024 / 1024);

    let memfd = create_snapshot_memfd(mem_data.as_ptr(), mem_size)?;
    drop(mem_data);

    let state_data = std::fs::read(&state_path)?;

    // Pre-restore validation: verify vmstate before any KVM mutation
    // This is the first line of defense against corrupt/mismatched snapshots
    let firecracker_version =
        config.and_then(|c| c.artifacts.allowed_firecracker_version.as_deref());
    if let Some(cfg) = config {
        if let Some(firecracker_version) = cfg.artifacts.allowed_firecracker_version.as_deref() {
            if let Err(e) = vmstate::pre_restore_validate(
                &state_data,
                Some(firecracker_version),
                manifest.vcpu_count,
            ) {
                anyhow::bail!("vmstate pre-restore validation failed: {:?}", e);
            }
        }
    }

    let parsed = vmstate::parse_vmstate(&state_data)?;
    eprintln!(
        "  CPU state loaded: RIP={:#x}, RSP={:#x}, CR3={:#x}",
        parsed.regs.rip, parsed.regs.rsp, parsed.sregs.cr3
    );
    eprintln!("  MSRs: {} entries", parsed.msrs.len());
    eprintln!(
        "  CPUID: {} entries from Firecracker snapshot",
        parsed.cpuid_entries.len()
    );

    Ok((
        VmSnapshot {
            regs: parsed.regs,
            sregs: parsed.sregs,
            msrs: parsed.msrs,
            lapic: parsed.lapic,
            ioapic_redirtbl: parsed.ioapic_redirtbl,
            xcrs: parsed.xcrs,
            xsave: parsed.xsave,
            cpuid_entries: parsed.cpuid_entries,
            mem_size,
        },
        memfd,
    ))
}

#[cfg(target_os = "macos")]
fn load_snapshot(
    workdir: &str,
    expected_language: Option<&str>,
    config: Option<&ServerConfig>,
) -> Result<(VmSnapshot, i32)> {
    // On macOS, we don't have KVM snapshots, so we create a VmSnapshot
    // that contains the configuration for a fresh boot
    let workdir_path = Path::new(workdir);
    let mut policy = manifest_policy(config);
    policy.expected_language = expected_language;

    let manifest = template_manifest::verify_template_artifacts_with_policy(workdir_path, &policy)?;

    eprintln!("Loading template for macOS HVF from {}...", workdir);

    // Resolve paths from manifest
    let kernel_path = template_manifest::resolve_path(workdir_path, &manifest.kernel_path);
    let rootfs_path = template_manifest::resolve_path(workdir_path, &manifest.rootfs_path);

    // Get memory size from manifest or default
    let mem_size = (manifest.mem_size_mib as usize) * 1024 * 1024;
    eprintln!("  Memory: {} MiB", manifest.mem_size_mib);
    eprintln!("  Kernel: {}", kernel_path.display());
    eprintln!("  Rootfs: {}", rootfs_path.display());

    // On macOS, we return a dummy memfd (-1) since HVF doesn't use memfd
    // The actual memory allocation happens in ForkedVm::fork_cow
    let snapshot = VmSnapshot {
        mem_size,
        kernel_path: Some(kernel_path.to_string_lossy().to_string()),
        rootfs_path: Some(rootfs_path.to_string_lossy().to_string()),
        init_path: Some(manifest.init_path.clone()),
    };

    Ok((snapshot, -1))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn load_snapshot(
    _workdir: &str,
    _expected_language: Option<&str>,
    _config: Option<&ServerConfig>,
) -> Result<(VmSnapshot, i32)> {
    bail!("snapshot restore is only supported on Linux hosts with /dev/kvm")
}

fn cmd_template(args: &[String]) -> Result<()> {
    if args.len() < 3 {
        bail!("Usage: zeroboot template <kernel> <rootfs> <workdir> [wait_secs] [init_path] [mem_mib]");
    }
    let kernel = &args[0];
    let rootfs = &args[1];
    let workdir = &args[2];
    let wait_secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let init_path = args.get(4).map(|s| s.as_str()).unwrap_or("/init");
    let mem_mib: u32 = args
        .get(5)
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            std::env::var("ZEROBOOT_MEM_MIB")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(512);

    std::fs::create_dir_all(workdir)?;
    let start = Instant::now();
    let (state_path, mem_path, mem_mib) = firecracker::create_template_snapshot(
        kernel, rootfs, workdir, mem_mib, wait_secs, init_path,
    )?;
    let elapsed = start.elapsed();
    println!("Template created in {:.2}s", elapsed.as_secs_f64());
    println!("  State: {}", state_path);
    println!("  Memory: {} ({} MiB)", mem_path, mem_mib);
    Ok(())
}

fn cmd_test_exec(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: zeroboot test-exec <workdir> [language] <code>");
    }
    let workdir = &args[0];
    let (language, code_start) = match args.get(1).map(|s| s.as_str()) {
        Some("python") | Some("node") | Some("javascript") => (args[1].clone(), 2usize),
        _ => ("python".to_string(), 1usize),
    };
    if args.len() <= code_start {
        bail!("Usage: zeroboot test-exec <workdir> [language] <code>");
    }
    let code = args[code_start..].join(" ");
    let normalized_language = if matches!(language.as_str(), "node" | "javascript") {
        "node"
    } else {
        "python"
    };
    let (snapshot, memfd) = load_snapshot(workdir, Some(normalized_language), None)?;

    eprintln!("Forking VM...");
    let fork_start = Instant::now();
    let mut vm = ForkedVm::fork_cow(&snapshot, memfd)?;
    eprintln!("  Fork time: {:.1}µs", vm.fork_time_us);

    let frame = protocol::encode_request_frame(&GuestRequest {
        request_id: "test-exec".into(),
        language: normalized_language.to_string(),
        code: code.as_bytes().to_vec(),
        stdin: Vec::new(),
        timeout_ms: 30_000,
    });
    vm.send_serial(&frame)?;

    let exec_start = Instant::now();
    let response = vm.run_until_response_timeout(Some(std::time::Duration::from_secs(30)))?;
    let exec_time = exec_start.elapsed();
    let total_time = fork_start.elapsed();

    eprintln!("  Exec time: {:.2}ms", exec_time.as_secs_f64() * 1000.0);
    eprintln!("  Total time: {:.2}ms", total_time.as_secs_f64() * 1000.0);
    println!(
        "=== stdout ===\n{}",
        String::from_utf8_lossy(&response.stdout)
    );
    if !response.stderr.is_empty() {
        println!(
            "=== stderr ===\n{}",
            String::from_utf8_lossy(&response.stderr)
        );
    }
    println!(
        "exit_code={} error_type={}",
        response.exit_code, response.error_type
    );

    unsafe {
        if memfd >= 0 {
            libc::close(memfd);
        }
    }
    Ok(())
}

fn cmd_fork_bench(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: zeroboot bench <workdir> [language]");
    }
    let workdir = &args[0];
    let language = args.get(1).map(|s| s.as_str()).unwrap_or("python");
    let bench_code = if matches!(language, "node" | "javascript") {
        b"console.log(1 + 1)".to_vec()
    } else {
        b"print(1 + 1)".to_vec()
    };
    let bench_language = if matches!(language, "node" | "javascript") {
        "node"
    } else {
        "python"
    };
    let (snapshot, memfd) = load_snapshot(workdir, Some(bench_language), None)?;
    let mem_size = snapshot.mem_size;

    eprintln!(
        "
=== Zeroboot Fork Benchmark ===
"
    );

    let mut mmap_times: Vec<f64> = Vec::with_capacity(10_000);
    for _ in 0..100 {
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                mem_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                memfd,
                0,
            )
        };
        if p != libc::MAP_FAILED {
            unsafe {
                libc::munmap(p, mem_size);
            }
        }
    }
    for _ in 0..10_000 {
        let start = Instant::now();
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                mem_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                memfd,
                0,
            )
        };
        mmap_times.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        if p != libc::MAP_FAILED {
            unsafe {
                libc::munmap(p, mem_size);
            }
        }
    }
    mmap_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    print_percentiles("Pure mmap CoW", &mmap_times);

    eprintln!();
    let mut fork_times: Vec<f64> = Vec::with_capacity(1000);
    for _ in 0..20 {
        let vm = ForkedVm::fork_cow(&snapshot, memfd)?;
        drop(vm);
    }
    for _ in 0..1000 {
        let start = Instant::now();
        let vm = ForkedVm::fork_cow(&snapshot, memfd)?;
        fork_times.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        drop(vm);
    }
    fork_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    print_percentiles("Full fork (KVM + CoW + CPU restore)", &fork_times);

    eprintln!();
    eprintln!(
        "Phase 3: Fork + framed {} request (100 iterations)...",
        bench_language
    );
    let mut request_times: Vec<f64> = Vec::with_capacity(100);
    let mut success_count = 0;
    for _ in 0..100 {
        let start = Instant::now();
        let mut vm = ForkedVm::fork_cow(&snapshot, memfd)?;
        let frame = protocol::encode_request_frame(&GuestRequest {
            request_id: "bench".into(),
            language: bench_language.to_string(),
            code: bench_code.clone(),
            stdin: Vec::new(),
            timeout_ms: 5_000,
        });
        vm.send_serial(&frame)?;
        let output = vm.run_until_response_timeout(Some(std::time::Duration::from_secs(5)))?;
        let t = start.elapsed().as_secs_f64() * 1000.0;
        request_times.push(t);
        if output.exit_code == 0 && String::from_utf8_lossy(&output.stdout).trim() == "2" {
            success_count += 1;
        }
        drop(vm);
    }
    request_times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  Fork + framed {} request ({}/100 successful):",
        bench_language, success_count
    );
    if !request_times.is_empty() {
        let n = request_times.len();
        println!("    P50:  {:>8.3} ms", request_times[n / 2]);
        println!("    P95:  {:>8.3} ms", request_times[n * 95 / 100]);
        println!("    P99:  {:>8.3} ms", request_times[n * 99 / 100]);
    }

    unsafe {
        if memfd >= 0 {
            libc::close(memfd);
        }
    }
    Ok(())
}

fn load_api_key_verifier(config: &ServerConfig) -> Result<Option<auth::ApiKeyVerifier>> {
    // In dev mode, allow no keys
    if matches!(config.auth_mode, AuthMode::Dev) {
        // Try to load if file exists, but don't fail if missing
        if let Ok(pepper) = std::fs::read_to_string(&config.api_key_pepper_file) {
            let pepper = pepper.trim();
            if !pepper.is_empty() {
                match auth::ApiKeyVerifier::load_from_file(&config.api_keys_file, pepper) {
                    Ok(verifier) => {
                        eprintln!(
                            "  Loaded {} API keys from {}",
                            verifier.len(),
                            config.api_keys_file.display()
                        );
                        return Ok(Some(verifier));
                    }
                    Err(e) => {
                        eprintln!("  Dev mode: could not load API keys ({}), continuing with auth disabled", e);
                    }
                }
            }
        }
        eprintln!("  Dev auth mode: no pepper or keys configured, continuing with auth disabled");
        return Ok(None);
    }

    // In prod mode, require both pepper and keys
    let pepper = std::fs::read_to_string(&config.api_key_pepper_file)
        .map(|p| p.trim().to_string())
        .map_err(|e| anyhow::anyhow!("auth mode is prod but pepper file is missing: {}", e))?;

    if pepper.is_empty() {
        bail!("auth mode is prod but pepper file is empty");
    }

    let verifier = auth::ApiKeyVerifier::load_from_file(&config.api_keys_file, &pepper)
        .map_err(|e| anyhow::anyhow!("auth mode is prod but API key file is invalid: {}", e))?;

    if verifier.is_empty() {
        bail!("auth mode is prod but no active API keys in key file");
    }

    eprintln!("  Loaded {} active API keys", verifier.len());
    Ok(Some(verifier))
}

fn cmd_serve(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Usage: zeroboot serve <workdir>[,lang:workdir2,...] [port]");
    }
    let port: u16 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(8080);
    let config = ServerConfig::from_env()?;

    // Validate startup configuration - fail-closed in prod mode
    config.validate_startup()?;
    let parsed_specs = startup::parse_template_specs(&args[0], None)?;
    startup::verify_startup(&config, &parsed_specs, None)?;

    let mut templates = std::collections::HashMap::new();
    let mut template_statuses = std::collections::HashMap::new();
    let mut quarantined = 0u64;
    for spec in &parsed_specs {
        let lang = spec.language.clone();
        let dir = spec.workdir.display().to_string();

        match validate_snapshot_workdir(&dir, Some(&lang), Some(&config)) {
            Ok(()) => {
                let (snapshot, memfd) = load_snapshot(&dir, Some(&lang), Some(&config))?;
                eprintln!("  Template '{}' loaded from {}", lang, dir);
                templates.insert(
                    lang.clone(),
                    Template {
                        snapshot,
                        memfd,
                        workdir: dir.clone(),
                    },
                );
                template_statuses.insert(
                    lang,
                    api::handlers::TemplateStatus {
                        ready: true,
                        detail: "startup verification ok".into(),
                        health: api::handlers::TemplateHealth::Healthy,
                    },
                );
            }
            Err(e) => {
                quarantined += 1;
                eprintln!("  Template '{}' quarantined: {}", lang, e);
                template_statuses.insert(
                    lang,
                    api::handlers::TemplateStatus {
                        ready: false,
                        detail: format!("quarantined: {}", e),
                        health: api::handlers::TemplateHealth::QuarantinedTrust,
                    },
                );
            }
        }
    }

    // In Prod mode, fail hard if any template is quarantined - no partial activation
    if matches!(config.auth_mode, AuthMode::Prod) && quarantined > 0 {
        bail!(
            "prod mode requires all templates to be valid, but {} template(s) quarantined",
            quarantined
        );
    }

    let api_key_verifier = load_api_key_verifier(&config)?;

    eprintln!("  Auth mode: {:?}", config.auth_mode);
    eprintln!("  Trusted proxies: {}", config.trusted_proxies.len());
    eprintln!(
        "  Request log path: {} (log_code={})",
        config.logging.path.display(),
        config.logging.log_code
    );
    eprintln!("  Bind address: {}", config.bind_addr);
    eprintln!("  Queue wait timeout: {} ms", config.queue.wait_timeout_ms);
    eprintln!("  Health cache TTL: {} s", config.health.cache_ttl_secs);
    eprintln!(
        "  Disk watermarks: free_bytes>={} free_inodes>={}",
        config.storage.min_free_bytes, config.storage.min_free_inodes
    );
    eprintln!(
        "  Require template hashes: {}",
        config.artifacts.require_template_hashes
    );
    if let Some(version) = &config.artifacts.allowed_firecracker_version {
        eprintln!("  Allowed Firecracker version: {}", version);
    }
    apply_request_log_path_fix(&config.logging.path);

    let (request_log_tx, mut request_log_rx) =
        tokio::sync::mpsc::channel::<String>(config.storage.request_log_queue_capacity);
    let metrics = Metrics::new();
    metrics
        .template_quarantines
        .store(quarantined, std::sync::atomic::Ordering::Relaxed);
    let state = Arc::new(AppState {
        templates,
        template_statuses,
        api_key_verifier,
        rate_limiters: std::sync::Mutex::new(std::collections::HashMap::new()),
        metrics,
        execution_semaphore: Arc::new(tokio::sync::Semaphore::new(
            config.limits.max_concurrent_requests,
        )),
        request_log_tx,
        health_cache: std::sync::Mutex::new(None),
        admission_paths: startup::runtime_admission_paths(&config, &parsed_specs),
        config: config.clone(),
    });
    let logger_config = config.clone();
    let logger_state = state.clone();
    let bind_addr = config.bind_addr.clone();
    let max_request_body_bytes = config.limits.max_request_body_bytes;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        tokio::spawn(async move {
            use std::io::Write;
            if let Some(parent) = logger_config.logging.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut writer = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&logger_config.logging.path)
                .ok()
                .map(std::io::BufWriter::new);
            while let Some(line) = request_log_rx.recv().await {
                if writer.is_none() {
                    writer = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&logger_config.logging.path)
                        .ok()
                        .map(std::io::BufWriter::new);
                    if writer.is_none() {
                        logger_state
                            .metrics
                            .request_log_write_failures
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                if let Some(w) = writer.as_mut() {
                    if writeln!(w, "{}", line).is_err() {
                        logger_state
                            .metrics
                            .request_log_write_failures
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        writer = None;
                    } else {
                        let _ = w.flush();
                    }
                }
            }
        });

        let app = axum::Router::new()
            .route("/exec", axum::routing::post(exec_handler))
            .route("/v1/exec", axum::routing::post(exec_handler))
            .route("/v1/exec/batch", axum::routing::post(batch_handler))
            .route("/live", axum::routing::get(live_handler))
            .route("/ready", axum::routing::get(ready_handler))
            .route("/health", axum::routing::get(health_handler))
            .route("/v1/health", axum::routing::get(health_handler))
            .route("/v1/ready", axum::routing::get(ready_handler))
            .route("/v1/live", axum::routing::get(live_handler))
            .route("/v1/metrics", axum::routing::get(metrics_handler))
            .layer(DefaultBodyLimit::max(max_request_body_bytes))
            .with_state(state);

        let bind_target = format!("{}:{}", bind_addr, port);
        let listener = tokio::net::TcpListener::bind(&bind_target)
            .await
            .with_context(|| format!("failed to bind API listener on {}", bind_target))?;
        eprintln!("Zeroboot API server listening on {}", bind_target);
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("API server failed while serving requests")?;
        eprintln!("Server shutdown complete");
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

fn cmd_verify_startup(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "Usage: zeroboot verify-startup <workdir>[,lang:workdir2,...] [--release-root <path>]"
        );
    }
    let mut template_spec = None;
    let mut release_root = None;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--release-root" => {
                let value = args
                    .get(idx + 1)
                    .context("--release-root requires a path argument")?;
                release_root = Some(Path::new(value));
                idx += 2;
            }
            other if template_spec.is_none() => {
                template_spec = Some(other.to_string());
                idx += 1;
            }
            other => bail!("unexpected argument: {}", other),
        }
    }

    let config = ServerConfig::from_env()?;
    let template_spec = template_spec.context("missing template spec")?;
    let parsed_specs = startup::parse_template_specs(&template_spec, release_root)?;
    startup::verify_startup(&config, &parsed_specs, release_root)?;
    println!("startup verification ok");
    for spec in parsed_specs {
        println!("  {} {}", spec.language, spec.workdir.display());
    }
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    eprintln!("Received SIGINT, shutting down gracefully...");
}

fn print_percentiles(label: &str, times: &[f64]) {
    let n = times.len();
    println!("  {} ({} iterations):", label, n);
    println!(
        "    Min:  {:>8.1} µs ({:.3} ms)",
        times[0],
        times[0] / 1000.0
    );
    println!(
        "    Avg:  {:>8.1} µs ({:.3} ms)",
        times.iter().sum::<f64>() / n as f64,
        times.iter().sum::<f64>() / n as f64 / 1000.0
    );
    println!(
        "    P50:  {:>8.1} µs ({:.3} ms)",
        times[n / 2],
        times[n / 2] / 1000.0
    );
    println!(
        "    P95:  {:>8.1} µs ({:.3} ms)",
        times[n * 95 / 100],
        times[n * 95 / 100] / 1000.0
    );
    println!(
        "    P99:  {:>8.1} µs ({:.3} ms)",
        times[n * 99 / 100],
        times[n * 99 / 100] / 1000.0
    );
    println!(
        "    Max:  {:>8.1} µs ({:.3} ms)",
        times[n - 1],
        times[n - 1] / 1000.0
    );
}

fn cmd_keygen(args: &[String]) -> Result<()> {
    let (pkcs8, public_key) = signing::generate_key_pair()?;
    let key_id = signing::get_key_id(&public_key);

    println!("Key ID: {}", key_id);
    println!("Public Key (base64):");
    println!("  {}", signing::format_public_key_base64(&public_key));

    // Write private key to file
    let key_path_str = args.first().map(|s| s.as_str()).unwrap_or("key.pkcs8");
    let key_path = Path::new(key_path_str);
    let mut file = std::fs::File::create(key_path)?;
    file.write_all(&pkcs8)?;

    println!("Private key written to: {}", key_path.display());
    println!();
    println!("To add to keyring, add this to your keyring.json:");
    println!("{{");
    println!("  \"key_id\": \"{}\",", key_id);
    println!("  \"algorithm\": \"ed25519\",");
    println!(
        "  \"public_key\": \"{}\",",
        signing::format_public_key_base64(&public_key)
    );
    println!("  \"enabled\": true,");
    println!("  \"description\": \"production signing key\"");
    println!("}}");

    Ok(())
}

fn cmd_promote_template(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "Usage: zeroboot promote-template <manifest> --channel <dev|staging|prod> --key <path> --key-id <id> [--out <path>] [--receipt <path>]"
        );
    }

    let manifest_path = Path::new(&args[0]);
    let mut channel = None;
    let mut key_path = None;
    let mut key_id = None;
    let mut out_path = None;
    let mut receipt_path = None;

    let mut idx = 1usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--channel" => {
                let value = args.get(idx + 1).context("--channel requires a value")?;
                channel = Some(value.to_string());
                idx += 2;
            }
            "--key" => {
                let value = args.get(idx + 1).context("--key requires a path")?;
                key_path = Some(value.to_string());
                idx += 2;
            }
            "--key-id" => {
                let value = args.get(idx + 1).context("--key-id requires a value")?;
                key_id = Some(value.to_string());
                idx += 2;
            }
            "--out" => {
                let value = args.get(idx + 1).context("--out requires a path")?;
                out_path = Some(value.to_string());
                idx += 2;
            }
            "--receipt" => {
                let value = args.get(idx + 1).context("--receipt requires a path")?;
                receipt_path = Some(value.to_string());
                idx += 2;
            }
            other => bail!("unexpected argument: {}", other),
        }
    }

    let channel = channel.context("--channel is required")?;
    if !matches!(channel.as_str(), "dev" | "staging" | "prod") {
        bail!("--channel must be one of: dev, staging, prod");
    }
    let key_path_value = key_path.context("--key is required")?;
    let key_path = Path::new(&key_path_value);
    let key_id = key_id.context("--key-id is required")?;
    let workdir = manifest_path
        .parent()
        .context("manifest path must live inside a template workdir")?;

    let mut policy = ManifestPolicy::dev();
    policy.require_hashes = true;
    let existing_manifest = template_manifest::verify_template_artifacts_with_policy(
        workdir, &policy,
    )
    .with_context(|| {
        format!(
            "pre-promotion verification failed for {}",
            manifest_path.display()
        )
    })?;
    if existing_manifest.schema_version != Some(1) {
        bail!(
            "promote-template only supports schema_version=1, got {:?}",
            existing_manifest.schema_version
        );
    }

    let key_bytes = std::fs::read(key_path)?;
    let public_key = signing::export_public_key(&key_bytes)?;
    let derived_key_id = signing::get_key_id(&public_key);
    if key_id != derived_key_id {
        bail!(
            "provided --key-id does not match key material: expected {}, got {}",
            derived_key_id,
            key_id
        );
    }

    let manifest_json = std::fs::read_to_string(manifest_path)?;
    let mut manifest_value: serde_json::Value = serde_json::from_str(&manifest_json)?;
    let previous_channel = manifest_value
        .get("promotion_channel")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    manifest_value["promotion_channel"] = serde_json::json!(channel);
    manifest_value["signer_key_id"] = serde_json::json!(key_id);
    manifest_value["manifest_signature"] = serde_json::Value::Null;
    manifest_value["manifest_signed_fields"] =
        serde_json::json!(signing::required_manifest_signed_fields_vec());

    let prepared_json = serde_json::to_string(&manifest_value)?;
    let (signature, _) = signing::sign_manifest_with_required_fields(&key_bytes, &prepared_json)?;
    manifest_value["manifest_signature"] = serde_json::json!(signature);

    let final_json = serde_json::to_string_pretty(&manifest_value)?;
    signing::verify_manifest_signature(
        &final_json,
        &key_id,
        &signature,
        Some(&signing::Keyring::from_keys(vec![signing::TrustedKey {
            key_id: key_id.clone(),
            algorithm: signing::SIG_ALGORITHM_ED25519.to_string(),
            public_key,
            enabled: true,
            description: Some("promotion verifier".to_string()),
        }])),
    )?;

    let out_path = out_path.as_deref().map(Path::new).unwrap_or(manifest_path);
    write_bytes_atomic(out_path, final_json.as_bytes())?;

    let signature_sha256 = {
        let signature_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &signature)
                .context("generated signature was not valid base64")?;
        hex::encode(Sha256::digest(signature_bytes))
    };
    let receipt = PromotionReceipt {
        manifest_path: out_path.display().to_string(),
        template_id: existing_manifest.template_id.clone(),
        artifact_set_id: existing_manifest.artifact_set_id.clone(),
        previous_channel,
        promotion_channel: channel,
        signer_key_id: key_id.clone(),
        signed_fields: signing::required_manifest_signed_fields_vec(),
        signature_sha256,
        promoted_at_unix_ms: current_unix_ms(),
    };
    if let Some(path) = receipt_path.as_deref().map(Path::new) {
        write_bytes_atomic(path, serde_json::to_vec_pretty(&receipt)?.as_slice())?;
        println!("promotion receipt written to {}", path.display());
    } else {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
    }

    println!("promoted manifest written to {}", out_path.display());
    Ok(())
}

fn cmd_sign(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("Usage: zeroboot sign <key_path> <manifest_path>");
    }

    let key_path = Path::new(&args[0]);
    let manifest_path = Path::new(&args[1]);

    let key_bytes = std::fs::read(key_path)?;
    let manifest_json = std::fs::read_to_string(manifest_path)?;

    let (signature, _) = signing::sign_manifest_with_required_fields(&key_bytes, &manifest_json)?;

    // Get key ID
    let public_key = signing::export_public_key(&key_bytes)?;
    let key_id = signing::get_key_id(&public_key);

    // Update manifest with signature
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_json)?;
    manifest["signer_key_id"] = serde_json::json!(key_id);
    manifest["manifest_signature"] = serde_json::json!(signature);
    manifest["manifest_signed_fields"] =
        serde_json::json!(signing::required_manifest_signed_fields_vec());

    // Write signed manifest
    let signed_json = serde_json::to_string_pretty(&manifest)?;
    write_bytes_atomic(manifest_path, signed_json.as_bytes())?;

    println!("Manifest signed with key ID: {}", key_id);
    println!("Signature: {}", signature);
    println!(
        "Signed fields ({}): {:?}",
        signing::REQUIRED_MANIFEST_SIGNED_FIELDS.len(),
        signing::REQUIRED_MANIFEST_SIGNED_FIELDS
    );

    Ok(())
}
