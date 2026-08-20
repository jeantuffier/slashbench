use crate::common::{base_url, median, parse_duration_secs, resolve_stacks, MEMORY_LADDER_MB, SLA_ERROR_RATE, SLA_P99_MS, SWEEP_CPU_LIMIT};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use crate::soak::run_soak_once;
use anyhow::Result;
use clap::Args;

/// Step memory down at a fixed target load until the SLA breaks
/// (CLAUDE.md §4 steps 3-4), then soak-confirm the result: a rung only
/// counts as this stack's minimum viable footprint if it also survives
/// sustained load, not just a short burst. Bumps up to the next larger
/// burst-passing rung and re-soaks if a candidate fails.
#[derive(Args)]
pub struct SweepArgs {
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
    #[arg(long, default_value_t = 1)]
    repeats: u32,
    /// Total duration of each soak-confirmation attempt.
    #[arg(long, default_value = "10m")]
    soak_total_duration: String,
    /// How often the soak samples memory + sends a canary request.
    #[arg(long, default_value = "15s")]
    soak_sample_interval: String,
    /// Consecutive soak passes required before a candidate rung counts as
    /// confirmed.Defaults to 1.
    #[arg(long, default_value_t = 1)]
    soak_repeats: u32,
    /// Skip soak-confirmation entirely and report burst-only results —
    /// for fast iteration when you don't need the authoritative number.
    #[arg(long)]
    skip_soak_confirmation: bool,
}

fn append_sweep_jsonl(root: &std::path::Path, run_id: &str, stack: &str, mem_mb: u32, repeat: u32, r: &k6::K6Result) -> Result<()> {
    use std::io::Write as _;
    let path = root.join("results/sweep.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "run_id": run_id,
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

pub fn run(root: &std::path::Path, logger: &Logger, args: &SweepArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let base_url = base_url();
    let soak_total_secs = parse_duration_secs(&args.soak_total_duration)?;
    let soak_sample_secs = parse_duration_secs(&args.soak_sample_interval)?;
    let mut results: Vec<(String, Vec<u32>, Option<u32>)> = Vec::new(); // (stack, burst-passing rungs desc, soak_confirmed_mb)

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} (target_rate={}req/s, cpu={SWEEP_CPU_LIMIT}) ===", args.target_rate));
        docker::ensure_postgres(root, logger)?;

        let mut passing_rungs: Vec<u32> = Vec::new();

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

            if docker::wait_http_ready(logger, &format!("{base_url}/items?page=1&limit=1"), 30).is_err() {
                logger.warn(format!("mem={mem_mb}MiB -> failed to become ready (likely OOM at boot) -> FAIL"));
                docker::remove_override(&override_path);
                break;
            }

            docker::reseed(root, logger)?;
            logger.info("Warm-up run (result discarded) ...");
            let _ = k6::run(root, logger, &base_url, args.target_rate, &args.warmup);

            let mut p99s = Vec::with_capacity(args.repeats as usize);
            let mut error_rates = Vec::with_capacity(args.repeats as usize);
            let mut achieved = Vec::with_capacity(args.repeats as usize);

            for repeat in 0..args.repeats {
                logger.info(format!("Measurement repeat {}/{} ...", repeat + 1, args.repeats));
                docker::reseed(root, logger)?;
                let result = k6::run(root, logger, &base_url, args.target_rate, &args.duration)?;
                append_sweep_jsonl(root, &logger.run_id, stack_name, mem_mb, repeat, &result)?;
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
                passing_rungs.push(mem_mb);
            } else {
                break;
            }
        }

        docker::stop_stack(root, logger, stack_name)?;

        let soak_confirmed_mb = if args.skip_soak_confirmation || passing_rungs.is_empty() {
            None
        } else {
            logger.info(format!(
                "=== {stack_name}: soak-confirming (requires {} consecutive passes), trying smallest burst-passing rung first, bumping up on any failure ===",
                args.soak_repeats
            ));
            let mut confirmed = None;
            // passing_rungs is in descending order (tested high->low); try
            // smallest (most aggressive) first, then progressively larger.
            'candidates: for &candidate in passing_rungs.iter().rev() {
                logger.info(format!(
                    "Soak-confirming {stack_name} at mem={candidate}MiB (need {} consecutive passes) ...",
                    args.soak_repeats
                ));
                for attempt in 1..=args.soak_repeats {
                    match run_soak_once(root, logger, stack_name, args.target_rate, candidate, soak_total_secs, soak_sample_secs) {
                        Ok(outcome) if outcome.passed => {
                            logger.info(format!("{stack_name} mem={candidate}MiB pass {attempt}/{}", args.soak_repeats));
                            if attempt == args.soak_repeats {
                                logger.info(format!(
                                    "{stack_name} SOAK-CONFIRMED at mem={candidate}MiB ({0}/{0} consecutive passes)",
                                    args.soak_repeats
                                ));
                                confirmed = Some(candidate);
                                break 'candidates;
                            }
                        }
                        Ok(outcome) => {
                            logger.warn(format!(
                                "{stack_name} FAILED soak at mem={candidate}MiB on attempt {attempt}/{} (died_early={}) — abandoning this rung, trying next larger burst-passing rung",
                                args.soak_repeats, outcome.died_early
                            ));
                            continue 'candidates;
                        }
                        Err(e) => {
                            logger.error(format!(
                                "{stack_name} soak attempt at mem={candidate}MiB errored: {e} — treating as failed, abandoning this rung"
                            ));
                            continue 'candidates;
                        }
                    }
                }
            }
            if confirmed.is_none() {
                logger.error(format!(
                    "{stack_name}: NO tested rung survived soak confirmation, even the largest ({} MiB) — consider extending the memory ladder upward",
                    passing_rungs.first().copied().unwrap_or(0)
                ));
            }
            confirmed
        };

        results.push((stack_name.to_string(), passing_rungs, soak_confirmed_mb));
    }

    logger.info(format!("=== Sweep summary (target_rate={}req/s) ===", args.target_rate));
    for (name, passing_rungs, soak_confirmed_mb) in &results {
        let burst = passing_rungs.last().map(|m| format!("{m} MiB")).unwrap_or_else(|| "none".to_string());
        let confirmed = soak_confirmed_mb.map(|m| format!("{m} MiB")).unwrap_or_else(|| "NOT CONFIRMED".to_string());
        logger.info(format!("{name:<15} burst minimum: {burst:<12} soak-confirmed: {confirmed}"));
    }

    let report = serde_json::json!({
        "run_id": logger.run_id,
        "target_rate_rps": args.target_rate,
        "sla": {"p99_ms": SLA_P99_MS, "error_rate": SLA_ERROR_RATE},
        "cpu_limit": SWEEP_CPU_LIMIT,
        "memory_ladder_mb": MEMORY_LADDER_MB,
        "repeats": args.repeats,
        "soak_confirmation": {
            "skipped": args.skip_soak_confirmation,
            "total_duration_secs": soak_total_secs,
            "sample_interval_secs": soak_sample_secs,
            "required_consecutive_passes": args.soak_repeats,
        },
        "results": results.iter().map(|(n, passing, confirmed)| serde_json::json!({
            "stack": n,
            "burst_minimum_mb": passing.last(),
            "burst_passing_rungs_mb": passing,
            "soak_confirmed_mb": confirmed,
        })).collect::<Vec<_>>(),
    });
    let out_path = root.join("results/sweep-summary.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    logger.info(format!("Written to {}", out_path.display()));

    Ok(())
}
