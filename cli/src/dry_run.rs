use crate::common::{base_url, median, resolve_stacks, RATE_LADDER, SLA_ERROR_RATE, SLA_P99_MS};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use anyhow::Result;
use clap::Args;

/// Finds each stack's max sustainable throughput with no resource cap —
/// calibrates the shared target load every other command uses. Mirrors
/// sweep's own warm-up + N-repeats + median rigor (see CLAUDE.md progress
/// log, Aug 17): a single k6 pass per rate step was fast but noisy enough
/// on real cloud infrastructure to swing the recommended target load by 8x
/// between two otherwise-identical runs, and this number is load-bearing
/// for every downstream command.
#[derive(Args)]
pub struct DryRunArgs {
    #[arg(long, default_value = "all")]
    stack: String,
    #[arg(long, default_value = "20s")]
    duration: String,
    #[arg(long, default_value = "10s")]
    warmup: String,
    #[arg(long, default_value_t = 3)]
    repeats: u32,
}

pub fn run(root: &std::path::Path, logger: &Logger, args: &DryRunArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let base_url = base_url();
    let mut ceilings: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} ==="));
        docker::ensure_postgres(root, logger)?;
        docker::reseed(root, logger)?;
        docker::start_stack(root, logger, stack_name)?;
        docker::wait_http_ready(logger, &format!("{base_url}/items?page=1&limit=1"), 30)?;

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
