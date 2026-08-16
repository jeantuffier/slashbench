mod capacity;
mod charts;
mod common;
mod cost;
mod docker;
mod dry_run;
mod k6;
mod log;
mod price_sweep;
mod proc;
mod report;
mod soak;
mod sweep;

use anyhow::Result;
use clap::{Parser, Subcommand};
use log::Logger;
use std::path::PathBuf;

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
    DryRun(dry_run::DryRunArgs),
    /// Step memory down at a fixed target load until the SLA breaks, then
    /// soak-confirm the result (CLAUDE.md §4 steps 3-4).
    Sweep(sweep::SweepArgs),
    /// Run sustained load at a stack's minimum viable footprint for an
    /// extended period, watching for memory growth or gradual latency/GC
    /// degradation that a short burst test can't catch (CLAUDE.md §4 step 4).
    Soak(soak::SoakArgs),
    /// Fixed-resource capacity test — the perf-oriented complement to
    /// `sweep`: at a fixed capped resource allocation, find max sustainable
    /// throughput per stack (CLAUDE.md progress log, Aug 16).
    Capacity(capacity::CapacityArgs),
    /// Price-oriented complement to `capacity`: sweeps CPU *and* memory
    /// together (not CPU fixed at 1.0) to find the minimum-cost combination
    /// meeting the SLA at the shared target load — fractional CPU is normal
    /// for every provider except GCP's instance-based tier (CLAUDE.md
    /// progress log, Aug 16).
    PriceSweep(price_sweep::PriceSweepArgs),
    /// Generate the static HTML report from everything under results/ —
    /// methodology, a cross-stack comparison grid, and per-stack detail
    /// charts. Safe to re-run at any point; degrades gracefully for stacks
    /// not yet measured.
    Report(report::ReportArgs),
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

    match &cli.command {
        Command::DryRun(args) => {
            let (logger, log_path) = Logger::new(&root, "dry-run", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            dry_run::run(&root, &logger, args)
        }
        Command::Sweep(args) => {
            let (logger, log_path) = Logger::new(&root, "sweep", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            sweep::run(&root, &logger, args)
        }
        Command::Soak(args) => {
            let (logger, log_path) = Logger::new(&root, "soak", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            soak::run(&root, &logger, args)
        }
        Command::Capacity(args) => {
            let (logger, log_path) = Logger::new(&root, "capacity", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            capacity::run(&root, &logger, args)
        }
        Command::PriceSweep(args) => {
            let (logger, log_path) = Logger::new(&root, "price-sweep", cli.verbose)?;
            logger.info(format!("Log file: {}", log_path.display()));
            price_sweep::run(&root, &logger, args)
        }
        Command::Report(args) => report::run(&root, args),
    }
}
