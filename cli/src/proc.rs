use crate::log::Logger;
use anyhow::{bail, Result};
use std::process::{Command, Output};

/// Runs a subprocess with output captured (not streamed live) so the log
/// stays a clean decision trail by default. On success, raw stdout/stderr is
/// only shown when `--verbose` is set; on failure it's always shown, since a
/// failure without its output isn't debuggable.
pub fn run(logger: &Logger, description: &str, cmd: &mut Command) -> Result<Output> {
    logger.info(format!("{description} ..."));
    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        if logger.verbose {
            logger.raw("stdout", &stdout);
            logger.raw("stderr", &stderr);
        }
        logger.info(format!("{description}: done"));
    } else {
        logger.error(format!("{description}: FAILED ({})", output.status));
        logger.raw("stdout", &stdout);
        logger.raw("stderr", &stderr);
        bail!("{description} failed with status {}", output.status);
    }

    Ok(output)
}
