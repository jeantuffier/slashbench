mod common;
mod docker;
mod dry_run;
mod k6;
mod log;
mod proc;
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
    }
}
