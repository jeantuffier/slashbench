use crate::log::Logger;
use crate::proc;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn ensure_postgres(root: &Path, logger: &Logger) -> Result<()> {
    proc::run(
        logger,
        "Ensuring postgres is up",
        Command::new("docker").current_dir(root).args(["compose", "up", "-d", "postgres"]),
    )?;
    wait_container_healthy(logger, "slashbench-postgres-1", 30)
}

/// Truncate + reseed via db/reset.sql, per CLAUDE.md "Fairness & environment
/// rules" — every stack's run starts from the same dataset.
pub fn reseed(root: &Path, logger: &Logger) -> Result<()> {
    let sql = std::fs::read_to_string(root.join("db/reset.sql")).context("reading db/reset.sql")?;
    let started = Instant::now();
    logger.info("Reseeding database (truncate + 100k rows) ...");

    let mut child = Command::new("docker")
        .current_dir(root)
        .args([
            "compose", "exec", "-T", "postgres", "psql", "-U", "slashbench", "-d", "slashbench",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning psql for reseed")?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(sql.as_bytes())
        .context("writing reset.sql to psql stdin")?;

    let output = child.wait_with_output().context("waiting for psql")?;
    let elapsed = started.elapsed();

    if !output.status.success() {
        logger.error(format!("Reseed FAILED after {:.1}s ({})", elapsed.as_secs_f64(), output.status));
        logger.raw("stdout", &String::from_utf8_lossy(&output.stdout));
        logger.raw("stderr", &String::from_utf8_lossy(&output.stderr));
        bail!("reseed failed with status {}", output.status);
    }

    if logger.verbose {
        logger.raw("stdout", &String::from_utf8_lossy(&output.stdout));
    }
    logger.info(format!("Reseed done in {:.1}s", elapsed.as_secs_f64()));
    Ok(())
}

pub fn start_stack(root: &Path, logger: &Logger, stack: &str) -> Result<()> {
    let started = Instant::now();
    proc::run(
        logger,
        &format!("Starting {stack} (uncapped)"),
        Command::new("docker").current_dir(root).args(["compose", "--profile", stack, "up", "-d", "--build", stack]),
    )?;
    logger.info(format!("{stack} container up after {:.1}s (build may be cached)", started.elapsed().as_secs_f64()));
    Ok(())
}

pub fn stop_stack(root: &Path, logger: &Logger, stack: &str) -> Result<()> {
    proc::run(
        logger,
        &format!("Stopping {stack}"),
        Command::new("docker").current_dir(root).args(["compose", "--profile", stack, "stop", stack]),
    )?;
    Ok(())
}

/// Writes a throwaway compose override pinning this stack's memory ceiling
/// and CPU allocation (CLAUDE.md §4 step 3: CPU fixed at 1 vCPU, only memory
/// varies). `mem_limit`/`cpus` are Compose Specification top-level fields
/// (not `deploy.resources`, which is Swarm-only) — confirmed empirically
/// against `docker inspect` before writing this, not assumed from docs.
pub fn write_mem_override(root: &Path, stack: &str, mem_mb: u32, cpus: &str) -> Result<PathBuf> {
    let override_path = root.join(format!(".slashbench-mem-override-{stack}.yml"));
    let contents = format!("services:\n  {stack}:\n    mem_limit: \"{mem_mb}m\"\n    cpus: \"{cpus}\"\n");
    std::fs::write(&override_path, contents)
        .with_context(|| format!("writing {}", override_path.display()))?;
    Ok(override_path)
}

pub fn remove_override(override_path: &Path) {
    let _ = std::fs::remove_file(override_path);
}

/// Starts (or restarts) a stack under a memory/CPU override. Always
/// force-recreates: the container must actually restart at the new ceiling
/// for it to take effect (a JVM sizes its default heap off the cgroup limit
/// visible at startup, not something that updates live).
pub fn start_stack_with_override(root: &Path, logger: &Logger, stack: &str, override_path: &Path, mem_mb: u32, cpus: &str) -> Result<()> {
    let override_str = override_path.to_str().expect("override path is valid utf8");
    let started = Instant::now();
    proc::run(
        logger,
        &format!("Starting {stack} at mem={mem_mb}MiB cpu={cpus}"),
        Command::new("docker").current_dir(root).args([
            "compose",
            "-f",
            "docker-compose.yml",
            "-f",
            override_str,
            "--profile",
            stack,
            "up",
            "-d",
            "--build",
            "--force-recreate",
            stack,
        ]),
    )?;
    logger.info(format!("{stack} container up at mem={mem_mb}MiB after {:.1}s", started.elapsed().as_secs_f64()));
    Ok(())
}

pub fn wait_http_ready(logger: &Logger, url: &str, timeout_secs: u64) -> Result<()> {
    logger.info(format!("Waiting for {url} to become ready (timeout {timeout_secs}s) ..."));
    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = started + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(resp) = client.get(url).send() {
            if resp.status().is_success() {
                logger.info(format!("Ready after {:.1}s", started.elapsed().as_secs_f64()));
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            logger.warn(format!("Not ready after {timeout_secs}s, giving up"));
            bail!("service at {url} did not become ready within {timeout_secs}s");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_container_healthy(logger: &Logger, container: &str, timeout_secs: u64) -> Result<()> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_secs);
    loop {
        let output = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Health.Status}}", container])
            .output()
            .context("running docker inspect")?;
        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if status == "healthy" {
            logger.info(format!("{container} healthy after {:.1}s", started.elapsed().as_secs_f64()));
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("container {container} did not become healthy within {timeout_secs}s (last status: '{status}')");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
