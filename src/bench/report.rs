use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkScenarioResult {
    pub scenario: String,
    pub language: String,
    pub concurrency: usize,
    pub samples: usize,
    pub successes: usize,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub p99_latency_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkModeResult {
    pub mode: String,
    pub targets: std::collections::HashMap<String, usize>,
    pub results: Vec<BenchmarkScenarioResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkArtifact {
    pub generated_at: u64,
    pub server_url: String,
    pub modes: Vec<BenchmarkModeResult>,
}

fn percentile(values: &[f64], pct: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let idx = ((values.len() - 1) * pct) / 100;
    values[idx]
}

pub fn summarize(
    scenario: &str,
    language: &str,
    concurrency: usize,
    latencies_ms: &[f64],
    successes: usize,
) -> BenchmarkScenarioResult {
    let mut sorted = latencies_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    BenchmarkScenarioResult {
        scenario: scenario.to_string(),
        language: language.to_string(),
        concurrency,
        samples: sorted.len(),
        successes,
        avg_latency_ms: avg,
        p50_latency_ms: percentile(&sorted, 50),
        p95_latency_ms: percentile(&sorted, 95),
        p99_latency_ms: percentile(&sorted, 99),
    }
}

pub fn write_artifact(out_dir: &Path, artifact: &BenchmarkArtifact) -> Result<PathBuf> {
    fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("{}.json", artifact.generated_at));
    fs::write(&path, serde_json::to_vec_pretty(artifact)?)?;
    Ok(path)
}

pub fn render_markdown(artifact: &BenchmarkArtifact) -> String {
    let mut out = String::new();
    out.push_str("# XBOOT Benchmarks\n\n");
    out.push_str(&format!("Server: `{}`\n\n", artifact.server_url));
    for mode in &artifact.modes {
        out.push_str(&format!("## {}\n\n", mode.mode));
        out.push_str("| Scenario | Lang | Concurrency | Samples | Successes | Avg ms | P50 ms | P95 ms | P99 ms |\n");
        out.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
        for result in &mode.results {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} |\n",
                result.scenario,
                result.language,
                result.concurrency,
                result.samples,
                result.successes,
                result.avg_latency_ms,
                result.p50_latency_ms,
                result.p95_latency_ms,
                result.p99_latency_ms,
            ));
        }
        out.push('\n');
    }
    out
}
