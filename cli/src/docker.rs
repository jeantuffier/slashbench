use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn ensure_postgres(root: &Path) -> Result<()> {
    run_compose(root, &["up", "-d", "postgres"])?;
    wait_container_healthy("slashbench-postgres-1", 30)
}

/// Truncate + reseed via db/reset.sql, per CLAUDE.md "Fairness & environment
/// rules" — every stack's run starts from the same dataset.
pub fn reseed(root: &Path) -> Result<()> {
    let sql = std::fs::read_to_string(root.join("db/reset.sql")).context("reading db/reset.sql")?;
    let mut child = Command::new("docker")
        .current_dir(root)
        .args([
            "compose", "exec", "-T", "postgres", "psql", "-U", "slashbench", "-d", "slashbench",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("spawning psql for reseed")?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(sql.as_bytes())
        .context("writing reset.sql to psql stdin")?;

    let status = child.wait().context("waiting for psql")?;
    if !status.success() {
        bail!("reseed failed with status {status}");
    }
    Ok(())
}

pub fn start_stack(root: &Path, stack: &str) -> Result<()> {
    run_compose(root, &["--profile", stack, "up", "-d", "--build", stack])
}

pub fn stop_stack(root: &Path, stack: &str) -> Result<()> {
    run_compose(root, &["--profile", stack, "stop", stack])
}

pub fn wait_http_ready(url: &str, timeout_secs: u64) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(resp) = client.get(url).send() {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!("service at {url} did not become ready within {timeout_secs}s");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_container_healthy(container: &str, timeout_secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let output = Command::new("docker")
            .args(["inspect", "--format", "{{.State.Health.Status}}", container])
            .output()
            .context("running docker inspect")?;
        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if status == "healthy" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("container {container} did not become healthy within {timeout_secs}s (last status: '{status}')");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn run_compose(root: &Path, args: &[&str]) -> Result<()> {
    let mut full_args = vec!["compose"];
    full_args.extend_from_slice(args);
    let status = Command::new("docker")
        .current_dir(root)
        .args(&full_args)
        .status()
        .with_context(|| format!("running docker {}", full_args.join(" ")))?;
    if !status.success() {
        bail!("docker {} failed with status {status}", full_args.join(" "));
    }
    Ok(())
}
