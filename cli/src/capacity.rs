use crate::common::{base_url, median, resolve_stacks, RATE_LADDER, SLA_ERROR_RATE, SLA_P99_MS};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use anyhow::Result;
use clap::Args;

/// Fixed-resource capacity test — the perf-oriented complement to `sweep`'s
/// price-oriented "minimum footprint under a fixed load" question. Same
/// ascending rate ladder as `dry-run`, but at a FIXED, capped resource
/// allocation instead of uncapped: answers "how much throughput does this
/// stack deliver for a fixed cloud bill" rather than "what's its
/// theoretical ceiling." See CLAUDE.md progress log (Aug 16) for the
/// discussion that led here. Shares `dry-run`'s warm-up + N-repeats +
/// median rigor (Aug 17) for the same reason: a single k6 pass per rate
/// step is noisy enough on real cloud infrastructure to misreport the
/// ceiling this stack's throughput-per-dollar chart is built on.
#[derive(Args)]
pub struct CapacityArgs {
    #[arg(long, default_value = "all")]
    stack: String,
    /// CPU allocation to test at, e.g. "1.0", "0.5".
    #[arg(long, default_value = "1.0")]
    cpu: String,
    /// Memory allocation to test at (MiB). Default is the largest
    /// soak-confirmed footprint across all six stacks as of Aug 16, 2026
    /// (spring-java's 256 MiB) — a "same box for everyone" size every
    /// stack comfortably fits, so the comparison is fair.
    #[arg(long, default_value_t = 256)]
    mem_mb: u32,
    #[arg(long, default_value = "20s")]
    duration: String,
    #[arg(long, default_value = "10s")]
    warmup: String,
    #[arg(long, default_value_t = 3)]
    repeats: u32,
}

pub fn run(root: &std::path::Path, logger: &Logger, args: &CapacityArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let base_url = base_url();
    let mut results: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} capacity: cpu={} mem={}MiB ===", args.cpu, args.mem_mb));
        docker::ensure_postgres(root, logger)?;
        docker::stop_stack(root, logger, stack_name)?;
        let override_path = docker::write_mem_override(root, stack_name, args.mem_mb, &args.cpu)?;

        if let Err(e) = docker::start_stack_with_override(root, logger, stack_name, &override_path, args.mem_mb, &args.cpu) {
            logger.error(format!("{stack_name} failed to start at cpu={} mem={}MiB ({e})", args.cpu, args.mem_mb));
            docker::remove_override(&override_path);
            results.push((stack_name.to_string(), None));
            continue;
        }
        docker::remove_override(&override_path);

        if docker::wait_http_ready(logger, &format!("{base_url}/items?page=1&limit=1"), 30).is_err() {
            logger.error(format!("{stack_name} failed to become ready at cpu={} mem={}MiB", args.cpu, args.mem_mb));
            docker::stop_stack(root, logger, stack_name)?;
            results.push((stack_name.to_string(), None));
            continue;
        }

        docker::reseed(root, logger)?;

        let mut ceiling = None;
        for &rate in RATE_LADDER {
            logger.info("Warm-up run (result discarded) ...");
            let _ = k6::run(root, logger, &base_url, rate, &args.warmup);

            let mut p99s = Vec::with_capacity(args.repeats as usize);
            let mut error_rates = Vec::with_capacity(args.repeats as usize);
            for repeat in 0..args.repeats {
                logger.info(format!("Measurement repeat {}/{} ...", repeat + 1, args.repeats));
                docker::reseed(root, logger)?;
                let result = k6::run(root, logger, &base_url, rate, &args.duration)?;
                p99s.push(result.p99_ms);
                error_rates.push(result.error_rate);
            }

            let median_p99 = median(p99s);
            let median_error = median(error_rates);
            let passed = median_p99 <= SLA_P99_MS && median_error <= SLA_ERROR_RATE;
            logger.info(format!(
                "rate={rate} p99(median)={median_p99:.1}ms error_rate(median)={:.2}% -> {}",
                median_error * 100.0,
                if passed { "PASS" } else { "FAIL" }
            ));

            if passed {
                ceiling = Some(rate);
            } else {
                break;
            }
        }

        docker::stop_stack(root, logger, stack_name)?;
        results.push((stack_name.to_string(), ceiling));
    }

    logger.info(format!("=== Capacity summary (cpu={} mem={}MiB) ===", args.cpu, args.mem_mb));
    for (name, ceiling) in &results {
        let s = ceiling.map(|c| format!("{c} req/s")).unwrap_or_else(|| "none (failed even at lowest rate)".to_string());
        logger.info(format!("{name:<15} max throughput: {s}"));
    }

    let report = serde_json::json!({
        "cpu": args.cpu,
        "mem_mb": args.mem_mb,
        "sla": {"p99_ms": SLA_P99_MS, "error_rate": SLA_ERROR_RATE},
        "results": results.iter().map(|(n, c)| serde_json::json!({"stack": n, "max_rps": c})).collect::<Vec<_>>(),
    });
    let out_path = root.join("results/capacity-summary.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    logger.info(format!("Written to {}", out_path.display()));

    Ok(())
}
