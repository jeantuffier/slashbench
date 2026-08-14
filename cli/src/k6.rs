use crate::log::Logger;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub struct K6Result {
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub error_rate: f64,
    pub achieved_rate: f64,
}

#[derive(Deserialize)]
struct K6Summary {
    metrics: K6Metrics,
}

#[derive(Deserialize)]
struct K6Metrics {
    http_req_duration: DurationMetric,
    http_req_failed: FailedMetric,
    http_reqs: ReqsMetric,
}

#[derive(Deserialize)]
struct DurationMetric {
    #[serde(rename = "p(95)")]
    p95: f64,
    #[serde(rename = "p(99)")]
    p99: f64,
}

#[derive(Deserialize)]
struct FailedMetric {
    value: f64,
}

#[derive(Deserialize)]
struct ReqsMetric {
    rate: f64,
}

/// Runs loadtest/script.js at a fixed constant-arrival-rate and returns the
/// parsed summary. `--summary-export` writes the full run summary as JSON;
/// schema confirmed against a live k6 2.2.0 run (see CLAUDE.md progress log)
/// rather than assumed, since k6's default summary omits p(99) entirely.
pub fn run(root: &Path, logger: &Logger, base_url: &str, rate: u32, duration: &str) -> Result<K6Result> {
    let summary_path = std::env::temp_dir().join(format!(
        "slashbench-k6-{}-{}.json",
        rate,
        std::process::id()
    ));

    logger.info(format!("Running k6: rate={rate}req/s duration={duration} ..."));
    let started = Instant::now();

    let output = Command::new("k6")
        .current_dir(root)
        .args([
            "run",
            "--quiet",
            "--summary-export",
            summary_path.to_str().expect("temp path is valid utf8"),
            "-e",
            &format!("BASE_URL={base_url}"),
            "-e",
            &format!("RATE={rate}"),
            "-e",
            &format!("DURATION={duration}"),
            "loadtest/script.js",
        ])
        .output()
        .context("running k6")?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        logger.error(format!("k6 run FAILED after {:.1}s ({})", started.elapsed().as_secs_f64(), output.status));
        logger.raw("stdout", &stdout);
        logger.raw("stderr", &stderr);
        let _ = std::fs::remove_file(&summary_path);
        bail!("k6 exited with status {}", output.status);
    }

    if logger.verbose {
        logger.raw("stdout", &stdout);
        logger.raw("stderr", &stderr);
    }

    let raw = std::fs::read_to_string(&summary_path).context("reading k6 summary export")?;
    let _ = std::fs::remove_file(&summary_path);
    let summary: K6Summary = serde_json::from_str(&raw).context("parsing k6 summary JSON")?;

    let result = K6Result {
        p95_ms: summary.metrics.http_req_duration.p95,
        p99_ms: summary.metrics.http_req_duration.p99,
        error_rate: summary.metrics.http_req_failed.value,
        achieved_rate: summary.metrics.http_reqs.rate,
    };

    logger.info(format!(
        "k6 done in {:.1}s: achieved={:.1}req/s p95={:.1}ms p99={:.1}ms error_rate={:.2}%",
        started.elapsed().as_secs_f64(),
        result.achieved_rate,
        result.p95_ms,
        result.p99_ms,
        result.error_rate * 100.0
    ));

    Ok(result)
}
