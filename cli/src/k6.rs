use crate::log::Logger;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

fn summary_path_for(rate: u32) -> PathBuf {
    std::env::temp_dir().join(format!("slashbench-k6-{}-{}.json", rate, std::process::id()))
}

fn k6_args(summary_path: &Path, base_url: &str, rate: u32, duration: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--quiet".into(),
        "--summary-export".into(),
        summary_path.to_str().expect("temp path is valid utf8").to_string(),
        "-e".into(),
        format!("BASE_URL={base_url}"),
        "-e".into(),
        format!("RATE={rate}"),
        "-e".into(),
        format!("DURATION={duration}"),
        "loadtest/script.js".into(),
    ]
}

fn parse_summary(summary_path: &Path) -> Result<K6Result> {
    let raw = std::fs::read_to_string(summary_path).context("reading k6 summary export")?;
    let _ = std::fs::remove_file(summary_path);
    let summary: K6Summary = serde_json::from_str(&raw).context("parsing k6 summary JSON")?;
    Ok(K6Result {
        p95_ms: summary.metrics.http_req_duration.p95,
        p99_ms: summary.metrics.http_req_duration.p99,
        error_rate: summary.metrics.http_req_failed.value,
        achieved_rate: summary.metrics.http_reqs.rate,
    })
}

/// Runs loadtest/script.js at a fixed constant-arrival-rate and blocks until
/// done, returning the parsed summary. Used for short bursts (dry-run's rate
/// ladder, the sweep's measurement repeats) where blocking is fine.
/// `--summary-export` writes the full run summary as JSON; schema confirmed
/// against a live k6 2.2.0 run (see CLAUDE.md progress log) rather than
/// assumed, since k6's default summary omits p(99) entirely.
pub fn run(root: &Path, logger: &Logger, base_url: &str, rate: u32, duration: &str) -> Result<K6Result> {
    let summary_path = summary_path_for(rate);
    logger.info(format!("Running k6: rate={rate}req/s duration={duration} ..."));
    let started = Instant::now();

    let output = Command::new("k6")
        .current_dir(root)
        .args(k6_args(&summary_path, base_url, rate, duration))
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

    let result = parse_summary(&summary_path)?;

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

/// A non-blocking k6 run, for the soak test — one uninterrupted process for
/// the whole soak duration while the caller samples memory (and anything
/// else) on its own timer in the meantime, rather than k6 being restarted
/// every measurement window.
pub struct K6Handle {
    child: Child,
    summary_path: PathBuf,
    started: Instant,
}

pub fn spawn(root: &Path, base_url: &str, rate: u32, duration: &str) -> Result<K6Handle> {
    let summary_path = summary_path_for(rate);
    let child = Command::new("k6")
        .current_dir(root)
        .args(k6_args(&summary_path, base_url, rate, duration))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning k6")?;
    Ok(K6Handle { child, summary_path, started: Instant::now() })
}

impl K6Handle {
    /// Non-blocking check for whether k6 has already exited on its own
    /// (e.g. it hit its own internal error, unrelated to the target dying).
    pub fn try_finished(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    /// Kill the process outright — used when the target has died and there's
    /// no point letting k6 keep trying to reach it for the rest of the run.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Waits for k6 to finish normally and returns the whole-run aggregate.
    pub fn wait(self, logger: &Logger) -> Result<K6Result> {
        let elapsed = self.started.elapsed();
        let output = self.child.wait_with_output().context("waiting for k6")?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !output.status.success() {
            logger.error(format!("k6 (continuous) FAILED after {:.1}s ({})", elapsed.as_secs_f64(), output.status));
            logger.raw("stdout", &stdout);
            logger.raw("stderr", &stderr);
            let _ = std::fs::remove_file(&self.summary_path);
            bail!("k6 exited with status {}", output.status);
        }

        if logger.verbose {
            logger.raw("stdout", &stdout);
            logger.raw("stderr", &stderr);
        }

        let result = parse_summary(&self.summary_path)?;

        logger.info(format!(
            "k6 (continuous) done in {:.1}s: whole-run aggregate achieved={:.1}req/s p95={:.1}ms p99={:.1}ms error_rate={:.2}%",
            elapsed.as_secs_f64(),
            result.achieved_rate,
            result.p95_ms,
            result.p99_ms,
            result.error_rate * 100.0
        ));

        Ok(result)
    }
}
