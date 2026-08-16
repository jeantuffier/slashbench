use crate::common::{base_url, median, resolve_stacks, MEMORY_LADDER_MB, SLA_ERROR_RATE, SLA_P99_MS};
use crate::docker;
use crate::k6;
use crate::log::Logger;
use anyhow::Result;
use clap::Args;

// Spans every provider's supported range: GCP request-based / Scaleway
// Containers go down to ~0.07-0.08 vCPU; AWS Fargate / Azure Container
// Apps only go down to 0.25. Each provider's own `min_vcpu` (cost.rs)
// filters out levels it doesn't actually support when pricing the results
// — this one ladder covers all of them rather than needing a per-provider
// sweep (see CLAUDE.md progress log, Aug 16: "measure once, price N ways").
const CPU_LADDER: &[f64] = &[1.0, 0.5, 0.25, 0.125, 0.08];

/// The price-oriented complement to `sweep`/`capacity`: at the shared
/// target load, sweeps CPU *and* memory together (not CPU held fixed at
/// 1.0) to find the minimum memory at each CPU level — since fractional
/// CPU is normal for every provider except GCP's instance-based tier (see
/// cost.rs), this produces the (cpu, min_mem) frontier that a real
/// fractional-CPU deployment could actually use. Deliberately burst-only,
/// not soak-confirmed like `sweep` — soak-confirming every point in a full
/// cpu×memory grid would multiply an already-large test matrix further;
/// this is a comparative pricing analysis across providers, not a
/// replacement for the authoritative sweep/soak minimum-footprint number.
#[derive(Args)]
pub struct PriceSweepArgs {
    #[arg(long, default_value = "all")]
    stack: String,
    #[arg(long, default_value_t = 200)]
    target_rate: u32,
    #[arg(long, default_value = "20s")]
    duration: String,
    #[arg(long, default_value = "10s")]
    warmup: String,
    #[arg(long, default_value_t = 3)]
    repeats: u32,
}

struct CpuMemPoint {
    cpu: f64,
    min_mem_mb: Option<u32>,
}

fn append_price_sweep_jsonl(root: &std::path::Path, stack: &str, cpu: f64, mem_mb: u32, repeat: u32, r: &k6::K6Result) -> Result<()> {
    use std::io::Write as _;
    let path = root.join("results/price-sweep.jsonl");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "stack": stack,
        "cpu": cpu,
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

pub fn run(root: &std::path::Path, logger: &Logger, args: &PriceSweepArgs) -> Result<()> {
    let stacks = resolve_stacks(&args.stack);
    let base_url = base_url();
    let mut all_results: Vec<(String, Vec<CpuMemPoint>)> = Vec::new();

    for stack_name in stacks {
        logger.info(format!("=== {stack_name} price-sweep (target_rate={}req/s) ===", args.target_rate));
        docker::ensure_postgres(root, logger)?;

        let mut points = Vec::new();

        for &cpu in CPU_LADDER {
            let cpu_str = format!("{cpu}");
            let mut min_mem: Option<u32> = None;

            for &mem_mb in MEMORY_LADDER_MB {
                docker::stop_stack(root, logger, stack_name)?;
                let override_path = docker::write_mem_override(root, stack_name, mem_mb, &cpu_str)?;

                if let Err(e) = docker::start_stack_with_override(root, logger, stack_name, &override_path, mem_mb, &cpu_str) {
                    logger.warn(format!("cpu={cpu} mem={mem_mb}MiB -> failed to start ({e}) -> FAIL"));
                    docker::remove_override(&override_path);
                    break;
                }

                if docker::wait_http_ready(logger, &format!("{base_url}/items?page=1&limit=1"), 30).is_err() {
                    logger.warn(format!("cpu={cpu} mem={mem_mb}MiB -> failed to become ready -> FAIL"));
                    docker::remove_override(&override_path);
                    break;
                }

                docker::reseed(root, logger)?;
                let _ = k6::run(root, logger, &base_url, args.target_rate, &args.warmup);

                let mut p99s = Vec::with_capacity(args.repeats as usize);
                let mut error_rates = Vec::with_capacity(args.repeats as usize);
                for repeat in 0..args.repeats {
                    docker::reseed(root, logger)?;
                    let result = k6::run(root, logger, &base_url, args.target_rate, &args.duration)?;
                    append_price_sweep_jsonl(root, stack_name, cpu, mem_mb, repeat, &result)?;
                    p99s.push(result.p99_ms);
                    error_rates.push(result.error_rate);
                }
                docker::remove_override(&override_path);

                let median_p99 = median(p99s);
                let median_error = median(error_rates);
                let passed = median_p99 <= SLA_P99_MS && median_error <= SLA_ERROR_RATE;
                logger.info(format!(
                    "cpu={cpu} mem={mem_mb}MiB p99(median)={median_p99:.1}ms error_rate(median)={:.2}% -> {}",
                    median_error * 100.0,
                    if passed { "PASS" } else { "FAIL" }
                ));

                if passed {
                    min_mem = Some(mem_mb);
                } else {
                    break;
                }
            }

            logger.info(format!("cpu={cpu}: minimum memory = {min_mem:?}"));
            points.push(CpuMemPoint { cpu, min_mem_mb: min_mem });
        }

        docker::stop_stack(root, logger, stack_name)?;
        all_results.push((stack_name.to_string(), points));
    }

    logger.info("=== Price-sweep summary ===");
    let mut stack_entries = Vec::new();
    for (stack, points) in &all_results {
        for p in points {
            let mem_str = p.min_mem_mb.map(|m| format!("{m}MiB")).unwrap_or_else(|| "none".to_string());
            logger.info(format!("{stack:<15} cpu={:<6} min_mem={mem_str}", p.cpu));
        }
        stack_entries.push(serde_json::json!({
            "stack": stack,
            "points": points.iter().map(|p| serde_json::json!({"cpu": p.cpu, "min_mem_mb": p.min_mem_mb})).collect::<Vec<_>>(),
        }));
    }

    let report = serde_json::json!({
        "target_rate_rps": args.target_rate,
        "sla": {"p99_ms": SLA_P99_MS, "error_rate": SLA_ERROR_RATE},
        "cpu_ladder": CPU_LADDER,
        "memory_ladder_mb": MEMORY_LADDER_MB,
        "repeats": args.repeats,
        "note": "Burst-only measurement (no soak-confirmation) — a comparative pricing analysis across (cpu, memory) points, not the authoritative minimum-footprint number (see sweep/soak for that).",
        "results": stack_entries,
    });
    let out_path = root.join("results/price-sweep-summary.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    logger.info(format!("Written to {}", out_path.display()));

    Ok(())
}
