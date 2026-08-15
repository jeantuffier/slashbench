use crate::common::{resolve_stacks, BASE_URL, SLA_ERROR_RATE, SLA_P99_MS};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use anyhow::Result;
use clap::Args;

const RATE_LADDER: &[u32] = &[50, 100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600];

#[derive(Args)]
pub struct DryRunArgs {
    #[arg(long, default_value = "all")]
    stack: String,
    #[arg(long, default_value = "20s")]
    duration: String,
}

pub fn run(root: &std::path::Path, logger: &Logger, args: &DryRunArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let mut ceilings: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} ==="));
        docker::ensure_postgres(root, logger)?;
        docker::reseed(root, logger)?;
        docker::start_stack(root, logger, stack_name)?;
        docker::wait_http_ready(logger, &format!("{BASE_URL}/items?page=1&limit=1"), 30)?;

        let mut ceiling = None;
        for &rate in RATE_LADDER {
            let result = k6::run(root, logger, BASE_URL, rate, &args.duration)?;
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
