use anyhow::{Context, Result};
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Writes every decision the orchestrator makes to both stdout and a
/// persisted, timestamped file under results/logs/. The point isn't just
/// console prettiness — it's that someone re-running this benchmark (or
/// checking a run of ours) shouldn't have to trust scrollback. The log file
/// is the durable, shareable record that the tool did what it claims, which
/// is the whole "testable by others" premise behind this project.
pub struct Logger {
    file: Mutex<File>,
    pub verbose: bool,
    /// A unique identifier for this specific invocation, shared with the log
    /// filename's own timestamp. Every row this run appends to a
    /// results/*.jsonl file carries this as "run_id", and every summary
    /// JSON this run writes records it too — so a later `report` generation
    /// can filter each append-only file down to just its most recent run
    /// instead of silently blending every run ever appended together (the
    /// original bug: sweep.jsonl/soak-*.jsonl accumulate forever across
    /// the whole project's history unless something reads them run-aware).
    pub run_id: String,
}

impl Logger {
    pub fn new(root: &Path, command: &str, verbose: bool) -> Result<(Self, PathBuf)> {
        let logs_dir = root.join("results/logs");
        std::fs::create_dir_all(&logs_dir).context("creating results/logs")?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let path = logs_dir.join(format!("{command}-{stamp}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok((Logger { file: Mutex::new(file), verbose, run_id: stamp }, path))
    }

    fn write(&self, level: &str, msg: &str) {
        let line = format!("[{}] [{level}] {msg}", Utc::now().format("%H:%M:%S%.3f"));
        println!("{line}");
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.write("INFO", msg.as_ref());
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        self.write("WARN", msg.as_ref());
    }

    pub fn error(&self, msg: impl AsRef<str>) {
        self.write("ERROR", msg.as_ref());
    }

    /// Raw subprocess output (docker/k6 stdout+stderr). Shown/logged only
    /// when verbose or after a failure — keeping the default log a clean
    /// decision trail instead of docker build spam and full k6 summaries,
    /// while still making a failure fully inspectable.
    pub fn raw(&self, label: &str, output: &str) {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            return;
        }
        let indented = trimmed.replace('\n', "\n  ");
        let line = format!("  [{label}] {indented}");
        println!("{line}");
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}
