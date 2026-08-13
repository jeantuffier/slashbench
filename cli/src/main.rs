mod docker;
mod k6;

use anyhow::Result;
use clap::{Parser, Subcommand};
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

#[derive(Parser)]
#[command(name = "slashbench")]
struct Cli {
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
        Command::DryRun { stack, duration } => dry_run(&root, &stack, &duration),
    }
}

fn dry_run(root: &std::path::Path, stack_filter: &str, duration: &str) -> Result<()> {
    let stacks: Vec<&str> = if stack_filter == "all" {
        STACKS.to_vec()
    } else {
        let found = STACKS.iter().find(|s| **s == stack_filter).unwrap_or_else(|| {
            panic!("unknown stack '{stack_filter}', expected one of {STACKS:?}")
        });
        vec![*found]
    };

    let mut ceilings: Vec<(String, Option<u32>)> = Vec::new();

    for stack_name in stacks {
        println!("\n=== {stack_name} ===");
        docker::ensure_postgres(root)?;
        docker::reseed(root)?;
        docker::start_stack(root, stack_name)?;
        docker::wait_http_ready(&format!("{BASE_URL}/items?page=1&limit=1"), 30)?;

        let mut ceiling = None;
        for &rate in RATE_LADDER {
            let result = k6::run(root, BASE_URL, rate, duration)?;
            println!(
                "  rate={rate:>5} achieved={:>8.1}req/s p95={:>7.1}ms p99={:>7.1}ms error_rate={:>6.2}%",
                result.achieved_rate,
                result.p95_ms,
                result.p99_ms,
                result.error_rate * 100.0
            );
            if result.p99_ms <= SLA_P99_MS && result.error_rate <= SLA_ERROR_RATE {
                ceiling = Some(rate);
            } else {
                println!("  -> SLA broken at rate={rate}, ceiling is {ceiling:?}");
                break;
            }
        }

        docker::stop_stack(root, stack_name)?;
        ceilings.push((stack_name.to_string(), ceiling));
    }

    println!("\n=== Dry-run summary ===");
    for (name, ceiling) in &ceilings {
        let ceiling_str = ceiling
            .map(|c| format!("{c} req/s"))
            .unwrap_or_else(|| "none (failed even at lowest rate)".to_string());
        println!("  {name:<15} ceiling: {ceiling_str}");
    }

    let weakest = ceilings.iter().filter_map(|(_, c)| *c).min();

    if let Some(weakest) = weakest {
        let target = (weakest as f64 * 0.6).round() as u32;
        println!("\nWeakest stack ceiling: {weakest} req/s");
        println!("Recommended shared target load (60%): {target} req/s");

        let report = serde_json::json!({
            "ceilings": ceilings.iter().map(|(n, c)| serde_json::json!({"stack": n, "ceiling_rps": c})).collect::<Vec<_>>(),
            "weakest_ceiling_rps": weakest,
            "recommended_target_load_rps": target,
        });
        let out_path = root.join("results/dry-run.json");
        std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
        println!("Written to {}", out_path.display());
    } else {
        println!(
            "\nNo stack passed even the lowest rate step ({} req/s) — check the SLA thresholds or the rate ladder.",
            RATE_LADDER[0]
        );
    }

    Ok(())
}
