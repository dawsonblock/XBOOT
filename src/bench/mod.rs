mod report;
mod scenarios;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use report::{write_artifact, BenchmarkArtifact, BenchmarkModeResult};
use scenarios::{concurrency_matrix, scenarios, Scenario};

#[derive(Debug, Deserialize)]
struct ExecResponse {
    exit_code: i32,
    stdout: String,
    runtime_error_type: String,
    total_time_ms: f64,
}

#[derive(Debug, Deserialize)]
struct PoolStatusSnapshot {
    lanes: HashMap<String, LaneSnapshot>,
}

#[derive(Debug, Deserialize)]
struct LaneSnapshot {
    target_idle: usize,
}

#[derive(Debug, Serialize)]
struct ExecRequest<'a> {
    code: &'a str,
    language: &'a str,
    timeout_seconds: u64,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn auth_header(token: &str) -> String {
    format!("Bearer {}", token)
}

fn fetch_pool_status(server_url: &str, admin_api_key: &str) -> Result<PoolStatusSnapshot> {
    let response = ureq::get(&format!("{}/v1/admin/pool", server_url))
        .set("Authorization", &auth_header(admin_api_key))
        .call()
        .map_err(|e| anyhow::anyhow!("GET /v1/admin/pool failed: {}", e))?;
    Ok(response.into_json()?)
}

fn scale_pool(
    server_url: &str,
    admin_api_key: &str,
    targets: &HashMap<String, usize>,
) -> Result<HashMap<String, usize>> {
    #[derive(Serialize)]
    struct ScaleRequest<'a> {
        targets: &'a HashMap<String, usize>,
    }
    let response = ureq::post(&format!("{}/v1/admin/scale", server_url))
        .set("Authorization", &auth_header(admin_api_key))
        .send_json(serde_json::to_value(ScaleRequest { targets })?)
        .map_err(|e| anyhow::anyhow!("POST /v1/admin/scale failed: {}", e))?;
    let status: PoolStatusSnapshot = response.into_json()?;
    Ok(status
        .lanes
        .into_iter()
        .map(|(language, lane)| (language, lane.target_idle))
        .collect())
}

fn execute_once(server_url: &str, api_key: &str, scenario: &Scenario) -> Result<ExecResponse> {
    let response = ureq::post(&format!("{}/v1/exec", server_url))
        .set("Authorization", &auth_header(api_key))
        .send_json(serde_json::to_value(ExecRequest {
            code: scenario.code,
            language: scenario.language,
            timeout_seconds: scenario.timeout_seconds,
        })?)
        .map_err(|e| anyhow::anyhow!("POST /v1/exec failed: {}", e))?;
    Ok(response.into_json()?)
}

fn is_success(response: &ExecResponse, scenario: &Scenario) -> bool {
    if response.exit_code != 0 || response.runtime_error_type != "ok" {
        return false;
    }
    if scenario.expected_stdout.is_empty() {
        !response.stdout.is_empty()
    } else {
        response.stdout == scenario.expected_stdout
    }
}

fn run_scenario_mode(
    server_url: &str,
    api_key: &str,
    scenario: &Scenario,
    concurrency: usize,
) -> report::BenchmarkScenarioResult {
    let total_samples = concurrency.clamp(1, 128);
    let latencies = Arc::new(Mutex::new(Vec::with_capacity(total_samples)));
    let successes = Arc::new(Mutex::new(0usize));
    let mut threads = Vec::with_capacity(total_samples);

    for _ in 0..total_samples {
        let latencies = latencies.clone();
        let successes = successes.clone();
        let server_url = server_url.to_string();
        let api_key = api_key.to_string();
        let scenario = scenario.clone();
        threads.push(thread::spawn(move || {
            let started = Instant::now();
            let response = execute_once(&server_url, &api_key, &scenario);
            let latency_ms = match &response {
                Ok(resp) => resp
                    .total_time_ms
                    .max(started.elapsed().as_secs_f64() * 1000.0),
                Err(_) => started.elapsed().as_secs_f64() * 1000.0,
            };
            if let Ok(mut values) = latencies.lock() {
                values.push(latency_ms);
            }
            if let Ok(resp) = response {
                if is_success(&resp, &scenario) {
                    if let Ok(mut ok) = successes.lock() {
                        *ok += 1;
                    }
                }
            }
        }));
    }

    for handle in threads {
        let _ = handle.join();
    }

    let latencies = latencies.lock().map(|v| v.clone()).unwrap_or_default();
    let successes = successes.lock().map(|v| *v).unwrap_or_default();
    report::summarize(
        scenario.name,
        scenario.language,
        concurrency,
        &latencies,
        successes,
    )
}

pub fn cmd_bench(args: &[String]) -> Result<()> {
    if args.len() < 3 {
        bail!("Usage: zeroboot bench <server_url> <api_key> <admin_api_key> [--out-dir <path>]");
    }
    let server_url = args[0].trim_end_matches('/').to_string();
    let api_key = args[1].to_string();
    let admin_api_key = args[2].to_string();
    let mut out_dir = PathBuf::from("artifacts/bench");
    let mut idx = 3usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--out-dir" => {
                let value = args
                    .get(idx + 1)
                    .context("--out-dir requires a path argument")?;
                out_dir = PathBuf::from(value);
                idx += 2;
            }
            other => bail!("unexpected argument: {}", other),
        }
    }

    let pool_status = fetch_pool_status(&server_url, &admin_api_key)?;
    let languages: Vec<String> = pool_status.lanes.keys().cloned().collect();
    if languages.is_empty() {
        bail!("bench requires at least one healthy pool lane");
    }

    let mut modes = Vec::new();
    for (mode_name, target_idle) in [("cold_strict", 0usize), ("warm_pooled_strict", 1usize)] {
        let targets: HashMap<String, usize> = languages
            .iter()
            .cloned()
            .map(|language| (language, target_idle))
            .collect();
        let applied_targets = scale_pool(&server_url, &admin_api_key, &targets)?;
        thread::sleep(std::time::Duration::from_millis(750));

        let mut results = Vec::new();
        for scenario in scenarios().into_iter().filter(|scenario| {
            languages
                .iter()
                .any(|language| language == scenario.language)
        }) {
            for &concurrency in concurrency_matrix() {
                results.push(run_scenario_mode(
                    &server_url,
                    &api_key,
                    &scenario,
                    concurrency,
                ));
            }
        }
        modes.push(BenchmarkModeResult {
            mode: mode_name.to_string(),
            targets: applied_targets,
            results,
        });
    }

    let artifact = BenchmarkArtifact {
        generated_at: now_unix_ms(),
        server_url,
        modes,
    };
    let artifact_path = write_artifact(&out_dir, &artifact)?;
    let markdown = report::render_markdown(&artifact);
    let markdown_path = artifact_path.with_extension("md");
    std::fs::write(&markdown_path, markdown)?;
    println!("{}", artifact_path.display());
    println!("{}", markdown_path.display());
    Ok(())
}
