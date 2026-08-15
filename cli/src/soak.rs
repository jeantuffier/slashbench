use crate::common::{parse_duration_secs, resolve_stacks, BASE_URL, SLA_ERROR_RATE, SLA_P99_MS, SWEEP_CPU_LIMIT};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use anyhow::{Context, Result};
use clap::Args;

// A container is considered dead (not just slow) after this many
// consecutive canary-request failures during a soak.
const SOAK_DEATH_THRESHOLD: u32 = 2;

/// Run sustained load at a stack's minimum viable footprint for an
/// extended period, watching for memory growth or gradual latency/GC
/// degradation that a short burst test can't catch (CLAUDE.md §4 step
/// 4). One continuous k6 process for the whole duration — memory and a
/// lightweight canary request are sampled on a timer while it runs,
/// rather than restarting k6 every measurement window.
#[derive(Args)]
pub struct SoakArgs {
    #[arg(long)]
    stack: String,
    #[arg(long, default_value_t = 200)]
    target_rate: u32,
    /// Total soak duration: "1h", "30m", "90s", or a plain number of
    /// seconds. Official runs use ~1h; shorten for local validation.
    #[arg(long, default_value = "1h")]
    total_duration: String,
    /// How often memory + a canary request are sampled.
    #[arg(long, default_value = "30s")]
    sample_interval: String,
    /// Memory ceiling to soak at (MiB). Defaults to this stack's
    /// soak-confirmed (or burst) minimum from results/sweep-summary.json.
    #[arg(long)]
    mem_mb: Option<u32>,
}

pub fn run(root: &std::path::Path, logger: &Logger, args: &SoakArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let total_secs = parse_duration_secs(&args.total_duration)?;
    let sample_secs = parse_duration_secs(&args.sample_interval)?;

    for stack_name in stacks {
        let mem_mb = match args.mem_mb {
            Some(m) => m,
            None => lookup_minimum_viable_mb(root, logger, stack_name)?,
        };
        let outcome = run_soak_once(root, logger, stack_name, args.target_rate, mem_mb, total_secs, sample_secs)?;
        logger.info(format!(
            "{stack_name} soak result: {} (died_early={})",
            if outcome.passed { "PASSED" } else { "FAILED" },
            outcome.died_early
        ));
    }

    Ok(())
}

/// Reads a stack's confirmed footprint from the sweep's output, preferring
/// the soak-confirmed number over the raw burst-only one (see CLAUDE.md §4
/// step 4 — a footprint that only survived a short burst isn't necessarily
/// real). `--mem-mb` overrides this when the sweep hasn't been run yet.
fn lookup_minimum_viable_mb(root: &std::path::Path, logger: &Logger, stack: &str) -> Result<u32> {
    let path = root.join("results/sweep-summary.json");
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {} — run `slashbench sweep` for this stack first, or pass --mem-mb explicitly",
            path.display()
        )
    })?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    let results = json["results"].as_array().context("sweep-summary.json has no 'results' array")?;
    let entry = results
        .iter()
        .find(|r| r["stack"].as_str() == Some(stack))
        .with_context(|| format!("no sweep result for stack '{stack}' in {}", path.display()))?;

    if let Some(confirmed) = entry["soak_confirmed_mb"].as_u64() {
        return Ok(confirmed as u32);
    }
    logger.warn(format!(
        "{stack} has no soak_confirmed_mb in {} — falling back to burst_minimum_mb, which is NOT validated under sustained load",
        path.display()
    ));
    entry["burst_minimum_mb"].as_u64().map(|v| v as u32).with_context(|| {
        format!(
            "stack '{stack}' has no burst_minimum_mb in {} either (it may have failed even at the sweep's highest step)",
            path.display()
        )
    })
}

fn append_soak_sample_jsonl(
    root: &std::path::Path,
    stack: &str,
    mem_ceiling_mb: u32,
    elapsed_secs: u64,
    mem_bytes: u64,
    canary_ok: bool,
    canary_ms: f64,
) -> Result<()> {
    use std::io::Write as _;
    let path = root.join(format!("results/soak-{stack}.jsonl"));
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "kind": "sample",
        "stack": stack,
        "mem_ceiling_mb": mem_ceiling_mb,
        "elapsed_secs": elapsed_secs,
        "mem_bytes": mem_bytes,
        "mem_mib": mem_bytes as f64 / (1024.0 * 1024.0),
        "canary_ok": canary_ok,
        "canary_latency_ms": canary_ms,
    });
    writeln!(file, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

fn append_soak_aggregate_jsonl(
    root: &std::path::Path,
    stack: &str,
    mem_ceiling_mb: u32,
    elapsed_secs: u64,
    passed: bool,
    r: &k6::K6Result,
) -> Result<()> {
    use std::io::Write as _;
    let path = root.join(format!("results/soak-{stack}.jsonl"));
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "kind": "aggregate",
        "stack": stack,
        "mem_ceiling_mb": mem_ceiling_mb,
        "elapsed_secs": elapsed_secs,
        "passed": passed,
        "p95_ms": r.p95_ms,
        "p99_ms": r.p99_ms,
        "error_rate": r.error_rate,
        "achieved_rate": r.achieved_rate,
    });
    writeln!(file, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

pub struct SoakOutcome {
    pub passed: bool,
    pub died_early: bool,
}

/// One continuous k6 process for the whole duration — memory and a
/// lightweight canary request are sampled on a timer while it runs, instead
/// of restarting k6 every measurement window (which would leave small gaps
/// in the load). Two consecutive canary failures = the container died;
/// kill k6 and stop early rather than waiting out the rest of the duration.
///
/// Shared between the standalone `soak` subcommand and `sweep`'s
/// soak-confirmation loop (CLAUDE.md §4 step 4 / Aug 15 progress log).
pub fn run_soak_once(
    root: &std::path::Path,
    logger: &Logger,
    stack: &str,
    target_rate: u32,
    mem_mb: u32,
    total_secs: u64,
    sample_interval_secs: u64,
) -> Result<SoakOutcome> {
    logger.info(format!(
        "=== {stack} soak: mem={mem_mb}MiB target_rate={target_rate}req/s total={total_secs}s sample_interval={sample_interval_secs}s (continuous) ==="
    ));

    docker::ensure_postgres(root, logger)?;
    docker::stop_stack(root, logger, stack)?;
    let override_path = docker::write_mem_override(root, stack, mem_mb, SWEEP_CPU_LIMIT)?;

    if let Err(e) = docker::start_stack_with_override(root, logger, stack, &override_path, mem_mb, SWEEP_CPU_LIMIT) {
        logger.error(format!("{stack} failed to start at mem={mem_mb}MiB ({e}) — cannot soak"));
        docker::remove_override(&override_path);
        return Ok(SoakOutcome { passed: false, died_early: true });
    }
    docker::remove_override(&override_path);

    if docker::wait_http_ready(logger, &format!("{BASE_URL}/items?page=1&limit=1"), 30).is_err() {
        logger.error(format!("{stack} failed to become ready at mem={mem_mb}MiB — cannot soak"));
        docker::stop_stack(root, logger, stack)?;
        return Ok(SoakOutcome { passed: false, died_early: true });
    }

    // Reseed ONCE, not per sample — a real service's dataset grows under
    // continuous writes, and soaking the list/pagination queries against a
    // growing table is part of the point, not an artifact to reset away.
    docker::reseed(root, logger)?;

    logger.info("Warm-up run (result discarded) ...");
    let _ = k6::run(root, logger, BASE_URL, target_rate, "10s");

    logger.info(format!("Starting continuous k6 run for {total_secs}s ..."));
    let mut handle = k6::spawn(root, BASE_URL, target_rate, &format!("{total_secs}s"))?;

    let client = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(2)).build()?;
    let canary_url = format!("{BASE_URL}/items?page=1&limit=1");
    let start = std::time::Instant::now();
    let mut consecutive_canary_failures = 0u32;
    let mut died_early = false;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(sample_interval_secs));
        let elapsed = start.elapsed().as_secs();

        let mem_bytes = docker::sample_memory_bytes(stack).unwrap_or(0);
        let canary_start = std::time::Instant::now();
        let canary_ok = client.get(&canary_url).send().map(|r| r.status().is_success()).unwrap_or(false);
        let canary_ms = canary_start.elapsed().as_secs_f64() * 1000.0;

        logger.info(format!(
            "Sample at elapsed={elapsed}s: mem={:.1}MiB canary={}({canary_ms:.1}ms)",
            mem_bytes as f64 / (1024.0 * 1024.0),
            if canary_ok { "ok" } else { "FAIL" }
        ));
        append_soak_sample_jsonl(root, stack, mem_mb, elapsed, mem_bytes, canary_ok, canary_ms)?;

        if canary_ok {
            consecutive_canary_failures = 0;
        } else {
            consecutive_canary_failures += 1;
            if consecutive_canary_failures >= SOAK_DEATH_THRESHOLD {
                logger.error(format!(
                    "{stack} appears to have died at elapsed={elapsed}s ({SOAK_DEATH_THRESHOLD} consecutive canary failures) — stopping early"
                ));
                handle.kill();
                died_early = true;
                break;
            }
        }

        if let Some(status) = handle.try_finished()? {
            logger.info(format!("k6 process exited on its own with status {status} at elapsed={elapsed}s"));
            break;
        }

        if elapsed >= total_secs {
            break;
        }
    }

    let final_elapsed = start.elapsed().as_secs();
    let passed = if died_early {
        false
    } else {
        let aggregate = handle.wait(logger)?;
        let ok = aggregate.p99_ms <= SLA_P99_MS && aggregate.error_rate <= SLA_ERROR_RATE;
        append_soak_aggregate_jsonl(root, stack, mem_mb, final_elapsed, ok, &aggregate)?;
        ok
    };

    docker::stop_stack(root, logger, stack)?;

    logger.info(format!("=== {stack} soak at mem={mem_mb}MiB: {} ===", if passed { "PASSED" } else { "FAILED" }));

    Ok(SoakOutcome { passed, died_early })
}
