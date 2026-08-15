use anyhow::Result;

pub const STACKS: &[&str] = &[
    "rocket",
    "actix-web",
    "spring-java",
    "spring-kotlin",
    "node-hono",
    "bun-hono",
];

// CLAUDE.md §4: SLA is p99 < 200ms, error rate < 1%.
pub const SLA_P99_MS: f64 = 200.0;
pub const SLA_ERROR_RATE: f64 = 0.01;
pub const BASE_URL: &str = "http://localhost:8080";

// CLAUDE.md §4 step 3: CPU fixed generously at 1 vCPU, only memory varies —
// memory is the headline claim, not CPU.
pub const SWEEP_CPU_LIMIT: &str = "1.0";

/// Accepts "all", a single stack name, or a comma-separated list — the list
/// form matters because sweep-summary.json is written once at the end of a
/// single invocation; running stacks one-per-process would overwrite it down
/// to just the last stack instead of covering all of them.
pub fn resolve_stacks(stack_filter: &str) -> Vec<&'static str> {
    if stack_filter == "all" {
        return STACKS.to_vec();
    }
    stack_filter
        .split(',')
        .map(|name| {
            let name = name.trim();
            *STACKS
                .iter()
                .find(|s| **s == name)
                .unwrap_or_else(|| panic!("unknown stack '{name}', expected one of {STACKS:?}"))
        })
        .collect()
}

pub fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in k6 metrics"));
    let n = values.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

pub fn parse_duration_secs(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('h') {
        return num.parse::<u64>().map(|h| h * 3600).map_err(anyhow::Error::from);
    }
    if let Some(num) = s.strip_suffix('m') {
        return num.parse::<u64>().map(|m| m * 60).map_err(anyhow::Error::from);
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.parse::<u64>().map_err(anyhow::Error::from);
    }
    s.parse::<u64>().map_err(anyhow::Error::from)
}
