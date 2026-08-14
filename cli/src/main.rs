mod docker;
mod k6;
mod log;
mod proc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use log::Logger;
use std::path::PathBuf;

const STACKS: &[&str] = &[
    "rocket",
    "actix-web",
    "spring-java",
    "spring-kotlin",
    "node-hono",
    "bun-hono",
];

// CLAUDE.md §4: SLA is p99 < 200ms, error rate < 1%.
const RATE_LADDER: &[u32] = &[50, 100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600];
const SLA_P99_MS: f64 = 200.0;
const SLA_ERROR_RATE: f64 = 0.01;
const BASE_URL: &str = "http://localhost:8080";

// CLAUDE.md §4 step 3: CPU fixed generously at 1 vCPU, only memory varies —
// memory is the headline claim, not CPU.
const MEMORY_LADDER_MB: &[u32] = &[512, 384, 256, 192, 128, 96, 64, 48, 32, 24, 16, 12, 8];
const SWEEP_CPU_LIMIT: &str = "1.0";

#[derive(Parser)]
#[command(name = "slashbench")]
struct Cli {
    /// Show raw docker/k6 subprocess output inline, not just on failure.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Dry-run each stack uncapped to find its max sustainable throughput
    /// (CLAUDE.md §4 step 1) and lock the shared target load for the sweep.
    DryRun {
        #[arg(long, default_value = "all")]
        stack: String,
        #[arg(long, default_value = "20s")]
        duration: String,
    },
    /// Step memory down at a fixed target load until the SLA breaks
    /// (CLAUDE.md §4 steps 3-4) — the last passing step is that stack's
    /// minimum viable footprint, the number the cost model actually uses.
    Sweep {
        #[arg(long, default_value = "all")]
        stack: String,
        /// The shared target load (req/s). Defaults to a safe local
        /// placeholder — swap in the real locked value once the dry-run has
        /// been run for real on the GCE VMs (see CLAUDE.md "Open next step").
        #[arg(long, default_value_t = 200)]
        target_rate: u32,
        #[arg(long, default_value = "20s")]
        duration: String,
        #[arg(long, default_value = "10s")]
        warmup: String,
        #[arg(long, default_value_t = 3)]
        repeats: u32,
    },
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli/ must be a direct child of the repo root")
        .to_path_buf()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root();

    match cli.command {
        Command::DryRun { stack, duration } => {
            let (logger, log_path) = Logger::new(&root, "dry-run", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            dry_run(&root, &logger, &stack, &duration)
        }
        Command::Sweep {
            stack,
            target_rate,
            duration,
            warmup,
            repeats,
        } => {
            let (logger, log_path) = Logger::new(&root, "sweep", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            sweep(&root, &logger, &stack, target_rate, &duration, &warmup, repeats)
        }
    }
}

/// Accepts "all", a single stack name, or a comma-separated list — the list
/// form matters because sweep-summary.json is written once at the end of a
/// single invocation; running stacks one-per-process would overwrite it down
/// to just the last stack instead of covering all of them.
fn resolve_stacks(stack_filter: &str) -> Vec<&'static str> {
    if stack_filter == "all" {
        return STACKS.to_vec();
    }
    stack_filter
        .split(',')
        .map(|name| {
            let name = name.trim();
            *STACKS
                .iter()
                .find(|s| **s == name)
                .unwrap_or_else(|| panic!("unknown stack '{name}', expected one of {STACKS:?}"))
        })
        .collect()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in k6 metrics"));
    let n = values.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

fn append_sweep_jsonl(root: &std::path::Path, stack: &str, mem_mb: u32, repeat: u32, r: &k6::K6Result) -> Result<()> {
    use std::io::Write as _;
    let path = root.join("results/sweep.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "stack": stack,
        "mem_mb": mem_mb,
        "repeat": repeat,
        "p95_ms": r.p95_ms,
        "p99_ms": r.p99_ms,
        "error_rate": r.error_rate,
        "achieved_rate": r.achieved_rate,
    });
    writeln!(file, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

fn dry_run(root: &std::path::Path, logger: &Logger, stack_filter: &str, duration: &str) -> Result<()> {
    let stacks = resolve_stacks(stack_filter);
    let mut ceilings: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} ==="));
        docker::ensure_postgres(root, logger)?;
        docker::reseed(root, logger)?;
        docker::start_stack(root, logger, stack_name)?;
        docker::wait_http_ready(logger, &format!("{BASE_URL}/items?page=1&limit=1"), 30)?;

        let mut ceiling = None;
        for &rate in RATE_LADDER {
            let result = k6::run(root, logger, BASE_URL, rate, duration)?;
            if result.p99_ms <= SLA_P99_MS && result.error_rate <= SLA_ERROR_RATE {
                ceiling = Some(rate);
                logger.info(format!("rate={rate} -> PASS (ceiling so far: {rate})"));
            } else {
                logger.info(format!("rate={rate} -> SLA broken, ceiling is {ceiling:?}"));
                break;
            }
        }

        docker::stop_stack(root, logger, stack_name)?;
        ceilings.push((stack_name.to_string(), ceiling));
    }

    logger.info("=== Dry-run summary ===");
    for (name, ceiling) in &ceilings {
        let ceiling_str = ceiling
            .map(|c| format!("{c} req/s"))
            .unwrap_or_else(|| "none (failed even at lowest rate)".to_string());
        logger.info(format!("{name:<15} ceiling: {ceiling_str}"));
    }

    let weakest = ceilings.iter().filter_map(|(_, c)| *c).min();

    if let Some(weakest) = weakest {
        let target = (weakest as f64 * 0.6).round() as u32;
        logger.info(format!("Weakest stack ceiling: {weakest} req/s"));
        logger.info(format!("Recommended shared target load (60%): {target} req/s"));

        let report = serde_json::json!({
            "ceilings": ceilings.iter().map(|(n, c)| serde_json::json!({"stack": n, "ceiling_rps": c})).collect::<Vec<_>>(),
            "weakest_ceiling_rps": weakest,
            "recommended_target_load_rps": target,
        });
        let out_path = root.join("results/dry-run.json");
        std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
        logger.info(format!("Written to {}", out_path.display()));
    } else {
        logger.warn(format!(
            "No stack passed even the lowest rate step ({} req/s) — check the SLA thresholds or the rate ladder.",
            RATE_LADDER[0]
        ));
    }

    Ok(())
}

fn sweep(
    root: &std::path::Path,
    logger: &Logger,
    stack_filter: &str,
    target_rate: u32,
    duration: &str,
    warmup: &str,
    repeats: u32,
) -> Result<()> {
    let stacks = resolve_stacks(stack_filter);
    let mut minimums: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} (target_rate={target_rate}req/s, cpu={SWEEP_CPU_LIMIT}) ==="));
        docker::ensure_postgres(root, logger)?;

        let mut minimum_mb = None;

        for &mem_mb in MEMORY_LADDER_MB {
            // Always restart at the new ceiling — a JVM sizes its default
            // heap off the cgroup limit visible at boot, not something that
            // adapts to a limit changed on a running container.
            docker::stop_stack(root, logger, stack_name)?;
            let override_path = docker::write_mem_override(root, stack_name, mem_mb, SWEEP_CPU_LIMIT)?;

            // A start failure (e.g. Docker enforces a 6MB minimum memory
            // limit and rejects anything lower) must fail just this step,
            // not the whole sweep — losing every remaining stack to one bad
            // rung would violate the "crash costs one step, not the run"
            // principle in CLAUDE.md §11.
            if let Err(e) = docker::start_stack_with_override(root, logger, stack_name, &override_path, mem_mb, SWEEP_CPU_LIMIT) {
                logger.warn(format!("mem={mem_mb}MiB -> failed to start ({e}) -> FAIL"));
                docker::remove_override(&override_path);
                break;
            }

            if docker::wait_http_ready(logger, &format!("{BASE_URL}/items?page=1&limit=1"), 30).is_err() {
                logger.warn(format!("mem={mem_mb}MiB -> failed to become ready (likely OOM at boot) -> FAIL"));
                docker::remove_override(&override_path);
                break;
            }

            docker::reseed(root, logger)?;
            logger.info("Warm-up run (result discarded) ...");
            let _ = k6::run(root, logger, BASE_URL, target_rate, warmup);

            let mut p99s = Vec::with_capacity(repeats as usize);
            let mut error_rates = Vec::with_capacity(repeats as usize);
            let mut achieved = Vec::with_capacity(repeats as usize);

            for repeat in 0..repeats {
                logger.info(format!("Measurement repeat {}/{repeats} ...", repeat + 1));
                docker::reseed(root, logger)?;
                let result = k6::run(root, logger, BASE_URL, target_rate, duration)?;
                append_sweep_jsonl(root, stack_name, mem_mb, repeat, &result)?;
                p99s.push(result.p99_ms);
                error_rates.push(result.error_rate);
                achieved.push(result.achieved_rate);
            }

            docker::remove_override(&override_path);

            let median_p99 = median(p99s);
            let median_error = median(error_rates);
            let median_achieved = median(achieved);
            let passed = median_p99 <= SLA_P99_MS && median_error <= SLA_ERROR_RATE;

            logger.info(format!(
                "mem={mem_mb}MiB achieved(median)={median_achieved:.1}req/s p99(median)={median_p99:.1}ms error_rate(median)={:.2}% -> {}",
                median_error * 100.0,
                if passed { "PASS" } else { "FAIL" }
            ));

            if passed {
                minimum_mb = Some(mem_mb);
            } else {
                break;
            }
        }

        docker::stop_stack(root, logger, stack_name)?;
        minimums.push((stack_name.to_string(), minimum_mb));
    }

    logger.info(format!("=== Sweep summary (target_rate={target_rate}req/s) ==="));
    for (name, mem) in &minimums {
        let s = mem
            .map(|m| format!("{m} MiB"))
            .unwrap_or_else(|| "none (failed even at highest step)".to_string());
        logger.info(format!("{name:<15} minimum viable footprint: {s}"));
    }

    let report = serde_json::json!({
        "target_rate_rps": target_rate,
        "sla": {"p99_ms": SLA_P99_MS, "error_rate": SLA_ERROR_RATE},
        "cpu_limit": SWEEP_CPU_LIMIT,
        "memory_ladder_mb": MEMORY_LADDER_MB,
        "repeats": repeats,
        "results": minimums.iter().map(|(n, m)| serde_json::json!({"stack": n, "minimum_viable_mb": m})).collect::<Vec<_>>(),
    });
    let out_path = root.join("results/sweep-summary.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    logger.info(format!("Written to {}", out_path.display()));

    Ok(())
}
