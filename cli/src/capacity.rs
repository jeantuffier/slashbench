use crate::common::{base_url, resolve_stacks, RATE_LADDER, SLA_ERROR_RATE, SLA_P99_MS};
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
/// discussion that led here.
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
            let result = k6::run(root, logger, &base_url, rate, &args.duration)?;
            if result.p99_ms <= SLA_P99_MS && result.error_rate <= SLA_ERROR_RATE {
                ceiling = Some(rate);
                logger.info(format!("rate={rate} -> PASS (ceiling so far: {rate})"));
            } else {
                logger.info(format!("rate={rate} -> SLA broken, ceiling is {ceiling:?}"));
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
