mod vmm;
mod api;
mod auth;
mod config;
mod protocol;
mod signing;
mod template_manifest;

use anyhow::{bail, Result};
use axum::extract::DefaultBodyLimit;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::time::Instant;

use api::handlers::{
    apply_request_log_path_fix, batch_handler, exec_handler, health_handler, live_handler, metrics_handler, ready_handler,
    AppState, Metrics, Template,
};
use config::{AuthMode, ServerConfig};
use protocol::GuestRequest;
use template_manifest::{VerificationMode, verify_template_artifacts};
use vmm::firecracker;
use vmm::kvm::{create_snapshot_memfd, ForkedVm, VmSnapshot};
use vmm::vmstate;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    match command {
        "template" => cmd_template(&args[2..]),
        "bench" | "fork-bench" => cmd_fork_bench(&args[2..]),
        "serve" => cmd_serve(&args[2..]),
        "test-exec" => cmd_test_exec(&args[2..]),
        _ => {
            eprintln!("Usage: zeroboot <command>");
            eprintln!("  template <kernel> <rootfs> <workdir> [wait_secs] [init_path] [mem_mib]");
            eprintln!("  bench <workdir> [language]");
            eprintln!("  test-exec <workdir> [language] <code>");
            eprintln!("  serve <workdir>[,lang:workdir2,...] [port]");
            Ok(())
        }
    }
}

fn validate_snapshot_workdir(workdir: &str, expected_language: Option<&str>, config: Option<&ServerConfig>) -> Result<()> {
    let workdir_path = Path::new(workdir);
    let require_hashes = config.map(|cfg| cfg.artifacts.require_template_hashes).unwrap_or(false);
    let allowed_firecracker_version = config
        .and_then(|cfg| cfg.artifacts.allowed_firecracker_version.as_deref());
    let allowed_firecracker_binary_sha256 = config
        .and_then(|cfg| cfg.artifacts.allowed_firecracker_binary_sha256.as_deref());
    let keyring_path = config
        .and_then(|cfg| cfg.artifacts.keyring_path.as_ref())
        .map(|p| p.as_path());
    let mode = config
        .map(|cfg| if matches!(cfg.auth_mode, AuthMode::Prod) { VerificationMode::Prod } else { VerificationMode::Dev })
        .unwrap_or(VerificationMode::Dev);
    verify_template_artifacts(
        workdir_path,
        expected_language,
        allowed_firecracker_version,
        allowed_firecracker_binary_sha256,
        require_hashes,
        mode,
        keyring_path,
    )?;
    Ok(())
}

fn load_snapshot(workdir: &str, expected_language: Option<&str>, config: Option<&ServerConfig>) -> Result<(VmSnapshot, i32)> {
    // First validate and get the manifest to know the actual paths
    let workdir_path = Path::new(workdir);
    let require_hashes = config.map(|cfg| cfg.artifacts.require_template_hashes).unwrap_or(false);
    let allowed_firecracker_version = config
        .and_then(|cfg| cfg.artifacts.allowed_firecracker_version.as_deref());
    let allowed_firecracker_binary_sha256 = config
        .and_then(|cfg| cfg.artifacts.allowed_firecracker_binary_sha256.as_deref());
    let keyring_path = config
        .and_then(|cfg| cfg.artifacts.keyring_path.as_ref())
        .map(|p| p.as_path());
    let mode = config
        .map(|cfg| if matches!(cfg.auth_mode, AuthMode::Prod) { VerificationMode::Prod } else { VerificationMode::Dev })
        .unwrap_or(VerificationMode::Dev);
    
    let manifest = verify_template_artifacts(
        workdir_path,
        expected_language,
        allowed_firecracker_version,
        allowed_firecracker_binary_sha256,
        require_hashes,
        mode,
        keyring_path,
    )?;
    
    // Use paths from manifest, resolved with confinement in prod mode
    let (mem_path, state_path) = if mode == VerificationMode::Prod {
        let mem = template_manifest::resolve_path_confined(workdir_path, &manifest.snapshot_mem_path)?;
        let state = template_manifest::resolve_path_confined(workdir_path, &manifest.snapshot_state_path)?;
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
    let parsed = vmstate::parse_vmstate(&state_data)?;
    eprintln!(
        "  CPU state loaded: RIP={:#x}, RSP={:#x}, CR3={:#x}",
        parsed.regs.rip, parsed.regs.rsp, parsed.sregs.cr3
    );
    eprintln!("  MSRs: {} entries", parsed.msrs.len());
    eprintln!("  CPUID: {} entries from Firecracker snapshot", parsed.cpuid_entries.len());

    Ok((VmSnapshot {
        regs: parsed.regs,
        sregs: parsed.sregs,
        msrs: parsed.msrs,
        lapic: parsed.lapic,
        ioapic_redirtbl: parsed.ioapic_redirtbl,
        xcrs: parsed.xcrs,
        xsave: parsed.xsave,
        cpuid_entries: parsed.cpuid_entries,
        mem_size,
    }, memfd))
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
    let mem_mib: u32 = args.get(5).and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("ZEROBOOT_MEM_MIB").ok().and_then(|v| v.parse().ok()))
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
    let normalized_language = if matches!(language.as_str(), "node" | "javascript") { "node" } else { "python" };
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
    println!("=== stdout ===\n{}", String::from_utf8_lossy(&response.stdout));
    if !response.stderr.is_empty() {
        println!("=== stderr ===\n{}", String::from_utf8_lossy(&response.stderr));
    }
    println!("exit_code={} error_type={}", response.exit_code, response.error_type);

    unsafe { libc::close(memfd); }
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
    let bench_language = if matches!(language, "node" | "javascript") { "node" } else { "python" };
    let (snapshot, memfd) = load_snapshot(workdir, Some(bench_language), None)?;
    let mem_size = snapshot.mem_size;

    eprintln!("
=== Zeroboot Fork Benchmark ===
");

    let mut mmap_times: Vec<f64> = Vec::with_capacity(10_000);
    for _ in 0..100 {
        let p = unsafe {
            libc::mmap(ptr::null_mut(), mem_size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_NORESERVE, memfd, 0)
        };
        if p != libc::MAP_FAILED { unsafe { libc::munmap(p, mem_size); } }
    }
    for _ in 0..10_000 {
        let start = Instant::now();
        let p = unsafe {
            libc::mmap(ptr::null_mut(), mem_size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_NORESERVE, memfd, 0)
        };
        mmap_times.push(start.elapsed().as_secs_f64() * 1_000_000.0);
        if p != libc::MAP_FAILED { unsafe { libc::munmap(p, mem_size); } }
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
    eprintln!("Phase 3: Fork + framed {} request (100 iterations)...", bench_language);
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
    println!("  Fork + framed {} request ({}/100 successful):", bench_language, success_count);
    if !request_times.is_empty() {
        let n = request_times.len();
        println!("    P50:  {:>8.3} ms", request_times[n / 2]);
        println!("    P95:  {:>8.3} ms", request_times[n * 95 / 100]);
        println!("    P99:  {:>8.3} ms", request_times[n * 99 / 100]);
    }

    unsafe { libc::close(memfd); }
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
                        eprintln!("  Loaded {} API keys from {}", verifier.len(), config.api_keys_file.display());
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

    let mut templates = std::collections::HashMap::new();
    let mut template_statuses = std::collections::HashMap::new();
    let mut quarantined = 0u64;
    for spec in args[0].split(',') {
        let (lang, dir) = if let Some((l, d)) = spec.split_once(':') {
            (l.to_string(), d.to_string())
        } else {
            ("python".to_string(), spec.to_string())
        };

        match validate_snapshot_workdir(&dir, Some(&lang), Some(&config)) {
            Ok(()) => {
                let (snapshot, memfd) = load_snapshot(&dir, Some(&lang), Some(&config))?;
                eprintln!("  Template '{}' loaded from {}", lang, dir);
                templates.insert(lang.clone(), Template { snapshot, memfd, workdir: dir.clone() });
                template_statuses.insert(lang, api::handlers::TemplateStatus { ready: true, detail: "startup verification ok".into() });
            }
            Err(e) => {
                quarantined += 1;
                eprintln!("  Template '{}' quarantined: {}", lang, e);
                template_statuses.insert(lang, api::handlers::TemplateStatus { ready: false, detail: format!("quarantined: {}", e) });
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
    eprintln!("  Request log path: {} (log_code={})", config.logging.path.display(), config.logging.log_code);
    eprintln!("  Bind address: {}", config.bind_addr);
    eprintln!("  Queue wait timeout: {} ms", config.queue.wait_timeout_ms);
    eprintln!("  Health cache TTL: {} s", config.health.cache_ttl_secs);
    eprintln!("  Require template hashes: {}", config.artifacts.require_template_hashes);
    if let Some(version) = &config.artifacts.allowed_firecracker_version {
        eprintln!("  Allowed Firecracker version: {}", version);
    }
    apply_request_log_path_fix(&config.logging.path);

    let (request_log_tx, mut request_log_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let metrics = Metrics::new();
    metrics.template_quarantines.store(quarantined, std::sync::atomic::Ordering::Relaxed);
    let state = Arc::new(AppState {
        templates,
        template_statuses,
        api_key_verifier,
        rate_limiters: std::sync::Mutex::new(std::collections::HashMap::new()),
        metrics,
        execution_semaphore: Arc::new(tokio::sync::Semaphore::new(config.limits.max_concurrent_requests)),
        request_log_tx,
        health_cache: std::sync::Mutex::new(None),
        config: config.clone(),
    });
    let logger_config = config.clone();
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
                }
                if let Some(w) = writer.as_mut() {
                    if writeln!(w, "{}", line).is_err() {
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
        let listener = tokio::net::TcpListener::bind(&bind_target).await.unwrap();
        eprintln!("Zeroboot API server listening on {}", bind_target);
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .unwrap();
        eprintln!("Server shutdown complete");
    });
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.ok();
    eprintln!("Received SIGINT, shutting down gracefully...");
}

fn print_percentiles(label: &str, times: &[f64]) {
    let n = times.len();
    println!("  {} ({} iterations):", label, n);
    println!("    Min:  {:>8.1} µs ({:.3} ms)", times[0], times[0] / 1000.0);
    println!("    Avg:  {:>8.1} µs ({:.3} ms)", times.iter().sum::<f64>() / n as f64, times.iter().sum::<f64>() / n as f64 / 1000.0);
    println!("    P50:  {:>8.1} µs ({:.3} ms)", times[n / 2], times[n / 2] / 1000.0);
    println!("    P95:  {:>8.1} µs ({:.3} ms)", times[n * 95 / 100], times[n * 95 / 100] / 1000.0);
    println!("    P99:  {:>8.1} µs ({:.3} ms)", times[n * 99 / 100], times[n * 99 / 100] / 1000.0);
    println!("    Max:  {:>8.1} µs ({:.3} ms)", times[n - 1], times[n - 1] / 1000.0);
}
