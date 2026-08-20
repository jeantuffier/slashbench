use crate::charts::{self, Bar, LineSeries, StackedCostBar};
use crate::common::{median, SLA_ERROR_RATE, SLA_P99_MS};
use crate::cost::{self, cpu_floor_cost_usd, memory_only_cost_usd, monthly_compute_cost_usd};
use anyhow::Result;
use askama::Template;
use clap::Args;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

const CHART_JS: &str = include_str!("../assets/chart.umd.min.js");
const CHART_JS_DATALABELS: &str = include_str!("../assets/chartjs-plugin-datalabels.min.js");
const REPORT_CSS: &str = include_str!("../templates/report.css");

const FAMILIES: &[(&str, &str, &[&str])] =
    &[("rust", "Rust", &["rocket", "actix-web"]), ("jvm", "JVM", &["spring-java", "spring-kotlin"]), ("js", "JS", &["node-hono", "bun-hono"])];

fn family_key_of(stack: &str) -> &'static str {
    FAMILIES.iter().find(|(_, _, stacks)| stacks.contains(&stack)).map(|(k, _, _)| *k).unwrap_or("unknown")
}

fn display_name(stack: &str) -> &'static str {
    match stack {
        "rocket" => "Rust + Rocket",
        "actix-web" => "Rust + Actix-web",
        "spring-java" => "Java + Spring Boot",
        "spring-kotlin" => "Kotlin + Spring Boot",
        "node-hono" => "Node + Hono",
        "bun-hono" => "Bun + Hono",
        _ => "Unknown stack",
    }
}

/// Three stacks (spring-java, spring-kotlin, node-hono) consistently failed
/// to survive a 10-minute sustained-load soak — not a memory-insufficiency
/// problem (spring-java still failed identically at 3x the memory), but poor
/// recovery from a shared, brief external disruption that Rocket/Actix-web
/// absorb near-instantly (see CLAUDE.md's Aug 20 entries for the full
/// investigation). bun-hono is a genuine borderline case: 1 of 3 independent
/// attempts passed. This distinguishes both from a generic "not confirmed
/// yet" label, which would wrongly imply the testing is simply incomplete.
fn soak_status(stack: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // (grid status label, grid soak-column label, stack-detail soak label)
    match stack {
        "spring-java" | "spring-kotlin" | "node-hono" => {
            Some(("resilience gap", "did not survive soak", "did not survive a sustained soak (see below)"))
        }
        "bun-hono" => Some(("borderline (1/3 soak)", "1 of 3 soak attempts passed", "borderline — passed 1 of 3 independent soak attempts")),
        _ => None,
    }
}

/// Generate the static HTML report from everything under results/ — reads
/// whatever's present (dry-run.json, sweep-summary.json, sweep.jsonl,
/// soak-{stack}.jsonl) and degrades gracefully for stacks not yet measured,
/// since this will be re-run repeatedly as more data comes in.
#[derive(Args)]
pub struct ReportArgs {
    #[arg(long, default_value = "report/index.html")]
    output: String,
}

#[derive(Template)]
#[template(path = "report.html")]
struct ReportTemplate {
    run_params: String,
    footprint_chart_id: Option<String>,
    cost_chart_id: Option<String>,
    price_chart_id: Option<String>,
    perf_chart_id: Option<String>,
    providers: Vec<ProviderOption>,
    grid_rows: Vec<GridRow>,
    families: Vec<FamilySection>,
    css: &'static str,
    chart_js: &'static str,
    chart_js_datalabels: &'static str,
    theme_init: &'static str,
    provider_switch_script: &'static str,
    chart_script: String,
}

struct ProviderOption {
    key: String,
    label: String,
}

struct GridRow {
    display_name: String,
    family_label: String,
    ceiling_label: String,
    burst_label: String,
    soak_label: String,
    cost_label: String,
    status_class: String,
    status_label: String,
}

struct FamilySection {
    label: String,
    stacks: Vec<StackDetail>,
}

struct StackDetail {
    display_name: String,
    burst_label: String,
    soak_label: String,
    ladder_chart_id: Option<String>,
    ladder_caption: String,
    soak_chart_id: Option<String>,
    soak_caption: String,
}

pub fn run(root: &Path, args: &ReportArgs) -> Result<()> {
    let dry_run = read_json(root, "results/dry-run.json");
    let sweep_summary = read_json(root, "results/sweep-summary.json");
    let sweep_raw = read_sweep_jsonl(root);

    let ceilings = dry_run_ceilings(&dry_run);
    let sweep_entries = sweep_summary_entries(&sweep_summary);
    let run_params = sweep_summary.as_ref().map(run_params_text).unwrap_or_else(|| "No sweep has been run yet.".to_string());

    let mut chart_script = String::new();
    let mut chart_counter = 0u32;
    let mut next_id = move || {
        chart_counter += 1;
        format!("chart-{chart_counter}")
    };

    // Headline chart: minimum viable footprint (MiB), sorted ascending, log
    // scale — this is the report's actual hero visual. The Cloud Run cost
    // model's instance-based tier has a hard 1-vCPU floor (confirmed against
    // Cloud Run's own docs, not assumed — see cost.rs), which swamps the
    // memory-driven dollar difference; the raw footprint is what actually
    // shows the 20x+ gap this benchmark exists to measure.
    let mut footprint_rows: Vec<(&str, u32, bool)> = Vec::new();
    for &(_, _, stacks) in FAMILIES {
        for &stack in stacks {
            let entry = sweep_entries.get(stack);
            let effective_mb = entry.and_then(|e| e.soak_confirmed_mb).or_else(|| entry.and_then(|e| e.burst_minimum_mb));
            if let Some(mb) = effective_mb {
                let provisional = entry.and_then(|e| e.soak_confirmed_mb).is_none();
                footprint_rows.push((stack, mb, provisional));
            }
        }
    }
    footprint_rows.sort_by_key(|(_, mb, _)| *mb);
    let footprint_chart_id = if footprint_rows.is_empty() {
        None
    } else {
        let bars: Vec<Bar> = footprint_rows
            .iter()
            .map(|(stack, mb, provisional)| {
                let (short_suffix, full_detail) = if !*provisional {
                    (String::new(), None)
                } else if *stack == "bun-hono" {
                    ("*".to_string(), Some(format!("{mb} MiB (burst only) — borderline: passed 1 of 3 independent 10-min soak attempts")))
                } else if soak_status(stack).is_some() {
                    ("*".to_string(), Some(format!("{mb} MiB (burst only) — did not survive a sustained 10-min soak; see the resilience finding below")))
                } else {
                    (" (provisional)".to_string(), None)
                };
                Bar {
                    label: display_name(stack).to_string(),
                    value: *mb as f64,
                    color_key: family_key_of(stack).to_string(),
                    value_label: format!("{mb} MiB{short_suffix}"),
                    tooltip_label: full_detail,
                }
            })
            .collect();
        let id = next_id();
        chart_script.push_str(&charts::bar_chart_init(&id, "Minimum viable footprint (log scale)", "MiB (log scale)", &bars, true));
        chart_script.push('\n');
        Some(id)
    };

    // Secondary chart: monthly Cloud Run cost, split into the fixed 1-vCPU
    // floor (identical for every stack) and the memory-driven cost that
    // actually varies — showing the combined total alone would be
    // misleading, since the floor dominates it (~$88 vs $0.10-$2).
    let mut cost_rows: Vec<(&str, u32, bool)> = footprint_rows.clone();
    cost_rows.sort_by_key(|(_, mb, _)| *mb);
    let cost_chart_id = if cost_rows.is_empty() {
        None
    } else {
        let bars: Vec<StackedCostBar> = cost_rows
            .iter()
            .map(|(stack, mb, _)| StackedCostBar {
                label: display_name(stack).to_string(),
                cpu_floor_usd: cpu_floor_cost_usd(&cost::GCP_INSTANCE_BASED, 1.0),
                memory_usd: memory_only_cost_usd(&cost::GCP_INSTANCE_BASED, *mb),
                color_key: family_key_of(stack).to_string(),
            })
            .collect();
        let id = next_id();
        chart_script.push_str(&charts::stacked_cost_chart_init(
            &id,
            "Monthly Cloud Run cost: fixed 1-vCPU floor vs memory-driven cost",
            "USD / month",
            &bars,
        ));
        chart_script.push('\n');
        Some(id)
    };

    // Price-oriented chart: cheapest (cpu, memory) combination meeting the
    // SLA at the shared target load, across five pricing options — fair
    // apples-to-apples since fractional CPU is normal everywhere except
    // GCP's instance-based tier (see cost.rs). Measured once (price-sweep),
    // priced five ways.
    let price_sweep_data = read_price_sweep(root);
    let price_chart_id = if price_sweep_data.is_empty() {
        None
    } else {
        let mut variants = Vec::new();
        for provider in cost::ALL_PROVIDERS {
            let mut bars = Vec::new();
            for &(_, _, stacks) in FAMILIES {
                for &stack in stacks {
                    if let Some(points) = price_sweep_data.get(stack) {
                        if let Some((cpu, mb, cheapest)) = cheapest_point_for_provider(points, provider) {
                            bars.push(Bar {
                                label: display_name(stack).to_string(),
                                value: cheapest,
                                color_key: family_key_of(stack).to_string(),
                                value_label: format!("${cheapest:.2}/mo"),
                                tooltip_label: Some(format!("${cheapest:.4}/mo ({cpu} vCPU, {mb} MiB)")),
                            });
                        }
                    }
                }
            }
            bars.sort_by(|a, b| a.value.partial_cmp(&b.value).unwrap());
            variants.push(charts::ProviderVariant { key: provider.key.to_string(), bars });
        }
        let id = next_id();
        chart_script.push_str(&charts::provider_bar_chart_init(
            &id,
            "Cheapest configuration meeting the SLA (fractional CPU allowed)",
            "USD / month",
            &variants,
            "gcp_request",
        ));
        chart_script.push('\n');
        Some(id)
    };

    // Perf-oriented chart: throughput per dollar at a FIXED resource
    // allocation — the complement to the price chart above. One
    // provider-agnostic measurement (capacity test), priced five ways.
    let capacity_data = read_capacity(root);
    let perf_chart_id = if let Some((fixed_cpu, fixed_mem, max_rps_by_stack)) = &capacity_data {
        let mut variants = Vec::new();
        for provider in cost::ALL_PROVIDERS {
            let fixed_cost = monthly_compute_cost_usd(provider, *fixed_cpu, *fixed_mem);
            let mut bars = Vec::new();
            for &(_, _, stacks) in FAMILIES {
                for &stack in stacks {
                    if let Some(Some(max_rps)) = max_rps_by_stack.get(stack) {
                        let per_dollar = *max_rps as f64 / fixed_cost;
                        bars.push(Bar {
                            label: display_name(stack).to_string(),
                            value: per_dollar,
                            color_key: family_key_of(stack).to_string(),
                            value_label: format!("{per_dollar:.1} req/s per $/mo"),
                            tooltip_label: Some(format!("{per_dollar:.1} req/s per $/mo ({max_rps} req/s)")),
                        });
                    }
                }
            }
            bars.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap());
            variants.push(charts::ProviderVariant { key: provider.key.to_string(), bars });
        }
        let id = next_id();
        chart_script.push_str(&charts::provider_bar_chart_init(
            &id,
            &format!("Throughput per dollar at a fixed {fixed_cpu} vCPU + {fixed_mem} MiB"),
            "req/s per USD/month",
            &variants,
            "gcp_request",
        ));
        chart_script.push('\n');
        Some(id)
    } else {
        None
    };

    // Comparison grid rows, and per-family detail sections.
    let mut grid_rows = Vec::new();
    let mut families = Vec::new();
    for &(_, family_label, stacks) in FAMILIES {
        let mut detail_stacks = Vec::new();
        for &stack in stacks {
            let entry = sweep_entries.get(stack);
            let burst_mb = entry.and_then(|e| e.burst_minimum_mb);
            let soak_mb = entry.and_then(|e| e.soak_confirmed_mb);
            let effective_mb = soak_mb.or(burst_mb);
            let cost = effective_mb.map(|mb| monthly_compute_cost_usd(&cost::GCP_INSTANCE_BASED, 1.0, mb));
            let provisional = cost.is_some() && soak_mb.is_none();

            let (status_class, status_label) = match (soak_mb, soak_status(stack)) {
                (Some(_), _) => ("status-good", "soak-confirmed"),
                (None, Some((label, _, _))) => ("status-warning", label),
                (None, None) if burst_mb.is_some() => ("status-warning", "burst only"),
                (None, None) => ("status-muted", "not measured"),
            };

            grid_rows.push(GridRow {
                display_name: display_name(stack).to_string(),
                family_label: family_label.to_string(),
                ceiling_label: ceilings.get(stack).copied().flatten().map(|c| format!("{c} req/s")).unwrap_or_else(|| "—".to_string()),
                burst_label: burst_mb.map(|m| format!("{m} MiB")).unwrap_or_else(|| "—".to_string()),
                soak_label: soak_mb
                    .map(|m| format!("{m} MiB"))
                    .unwrap_or_else(|| soak_status(stack).map(|(_, grid_label, _)| grid_label.to_string()).unwrap_or_else(|| "not confirmed".to_string())),
                cost_label: cost.map(|c| format!("${c:.2}{}", if provisional { "*" } else { "" })).unwrap_or_else(|| "—".to_string()),
                status_class: status_class.to_string(),
                status_label: status_label.to_string(),
            });

            let ladder_chart_id = sweep_raw.get(stack).filter(|rows| !rows.is_empty()).map(|rows| {
                let steps = aggregate_sweep_steps(rows);
                let series = LineSeries {
                    labels: steps.iter().map(|s| format!("{} MiB", s.mem_mb)).collect(),
                    values: steps.iter().map(|s| s.p99_ms).collect(),
                    point_color_keys: steps.iter().map(|s| if s.passed { "good".to_string() } else { "critical".to_string() }).collect(),
                    line_color_key: family_key_of(stack).to_string(),
                };
                let id = next_id();
                chart_script.push_str(&charts::line_chart_init(
                    &id,
                    &format!("{}: p99 latency vs memory ceiling (SLA: {SLA_P99_MS:.0}ms)", display_name(stack)),
                    "Memory ceiling",
                    "p99 latency (ms)",
                    &series,
                ));
                chart_script.push('\n');
                id
            });

            let soak_samples = read_soak_samples(root, stack);
            let soak_chart_id = if soak_samples.is_empty() {
                None
            } else {
                let series = LineSeries {
                    labels: soak_samples.iter().map(|s| format!("{}s", s.0)).collect(),
                    values: soak_samples.iter().map(|s| s.1).collect(),
                    point_color_keys: soak_samples.iter().map(|s| if s.2 { "good".to_string() } else { "critical".to_string() }).collect(),
                    line_color_key: family_key_of(stack).to_string(),
                };
                let id = next_id();
                chart_script.push_str(&charts::line_chart_init(&id, &format!("{}: memory over time (soak)", display_name(stack)), "Elapsed", "Memory (MiB)", &series));
                chart_script.push('\n');
                Some(id)
            };

            detail_stacks.push(StackDetail {
                display_name: display_name(stack).to_string(),
                burst_label: burst_mb.map(|m| format!("{m} MiB")).unwrap_or_else(|| "not yet measured".to_string()),
                soak_label: soak_mb
                    .map(|m| format!("{m} MiB"))
                    .unwrap_or_else(|| soak_status(stack).map(|(_, _, detail_label)| detail_label.to_string()).unwrap_or_else(|| "not yet confirmed".to_string())),
                ladder_chart_id,
                ladder_caption: format!("Green = met the {SLA_P99_MS:.0}ms / {:.0}% SLA at that ceiling; red = broke it.", SLA_ERROR_RATE * 100.0),
                soak_chart_id,
                soak_caption: "Red point = canary request failed (container likely dying).".to_string(),
            });
        }
        families.push(FamilySection { label: family_label.to_string(), stacks: detail_stacks });
    }

    let providers = cost::ALL_PROVIDERS.iter().map(|p| ProviderOption { key: p.key.to_string(), label: p.name.to_string() }).collect();

    let template = ReportTemplate {
        run_params,
        footprint_chart_id,
        cost_chart_id,
        price_chart_id,
        perf_chart_id,
        providers,
        grid_rows,
        families,
        css: REPORT_CSS,
        chart_js: CHART_JS,
        chart_js_datalabels: CHART_JS_DATALABELS,
        theme_init: charts::THEME_INIT_SCRIPT,
        provider_switch_script: charts::PROVIDER_SWITCH_SCRIPT,
        chart_script,
    };
    let html = template.render()?;

    let out_path = root.join(&args.output);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, html)?;
    println!("Report written to {}", out_path.display());

    Ok(())
}

struct SweepStepAgg {
    mem_mb: u32,
    p99_ms: f64,
    passed: bool,
}

fn aggregate_sweep_steps(rows: &[(u32, f64, f64)]) -> Vec<SweepStepAgg> {
    let mut by_mem: BTreeMap<u32, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for &(mem_mb, p99, err) in rows {
        let entry = by_mem.entry(mem_mb).or_default();
        entry.0.push(p99);
        entry.1.push(err);
    }
    let mut steps: Vec<SweepStepAgg> = by_mem
        .into_iter()
        .map(|(mem_mb, (p99s, errs))| {
            let p99 = median(p99s);
            let err = median(errs);
            SweepStepAgg { mem_mb, p99_ms: p99, passed: p99 <= SLA_P99_MS && err <= SLA_ERROR_RATE }
        })
        .collect();
    steps.sort_by(|a, b| b.mem_mb.cmp(&a.mem_mb));
    steps
}

fn read_soak_samples(root: &Path, stack: &str) -> Vec<(u64, f64, bool)> {
    let path = root.join(format!("results/soak-{stack}.jsonl"));
    let Ok(content) = std::fs::read_to_string(path) else { return Vec::new() };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["kind"].as_str() == Some("sample"))
        .filter_map(|v| Some((v["elapsed_secs"].as_u64()?, v["mem_mib"].as_f64()?, v["canary_ok"].as_bool().unwrap_or(true))))
        .collect()
}

struct SweepEntry {
    burst_minimum_mb: Option<u32>,
    soak_confirmed_mb: Option<u32>,
}

fn sweep_summary_entries(summary: &Option<Value>) -> BTreeMap<String, SweepEntry> {
    let mut map = BTreeMap::new();
    let Some(summary) = summary else { return map };
    let Some(results) = summary["results"].as_array() else { return map };
    for r in results {
        let Some(stack) = r["stack"].as_str() else { continue };
        map.insert(
            stack.to_string(),
            SweepEntry { burst_minimum_mb: r["burst_minimum_mb"].as_u64().map(|v| v as u32), soak_confirmed_mb: r["soak_confirmed_mb"].as_u64().map(|v| v as u32) },
        );
    }
    map
}

fn dry_run_ceilings(dry_run: &Option<Value>) -> BTreeMap<String, Option<u32>> {
    let mut map = BTreeMap::new();
    let Some(dry_run) = dry_run else { return map };
    let Some(ceilings) = dry_run["ceilings"].as_array() else { return map };
    for c in ceilings {
        if let Some(stack) = c["stack"].as_str() {
            map.insert(stack.to_string(), c["ceiling_rps"].as_u64().map(|v| v as u32));
        }
    }
    map
}

fn read_sweep_jsonl(root: &Path) -> BTreeMap<String, Vec<(u32, f64, f64)>> {
    let mut map: BTreeMap<String, Vec<(u32, f64, f64)>> = BTreeMap::new();
    let Ok(content) = std::fs::read_to_string(root.join("results/sweep.jsonl")) else { return map };
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let (Some(stack), Some(mem_mb), Some(p99), Some(err)) =
            (v["stack"].as_str(), v["mem_mb"].as_u64(), v["p99_ms"].as_f64(), v["error_rate"].as_f64())
        else {
            continue;
        };
        map.entry(stack.to_string()).or_default().push((mem_mb as u32, p99, err));
    }
    map
}

struct PricePoint {
    cpu: f64,
    min_mem_mb: Option<u32>,
}

fn read_price_sweep(root: &Path) -> BTreeMap<String, Vec<PricePoint>> {
    let mut map = BTreeMap::new();
    let Some(summary) = read_json(root, "results/price-sweep-summary.json") else { return map };
    let Some(results) = summary["results"].as_array() else { return map };
    for r in results {
        let Some(stack) = r["stack"].as_str() else { continue };
        let Some(points) = r["points"].as_array() else { continue };
        let pts: Vec<PricePoint> = points
            .iter()
            .filter_map(|p| {
                let cpu = p["cpu"].as_f64()?;
                Some(PricePoint { cpu, min_mem_mb: p["min_mem_mb"].as_u64().map(|v| v as u32) })
            })
            .collect();
        map.insert(stack.to_string(), pts);
    }
    map
}

/// The cheapest (cpu, mem) point a given provider's billing model can
/// actually use — filters out CPU levels below that provider's own minimum
/// (see cost.rs) before picking the minimum-cost point.
fn cheapest_point_for_provider(points: &[PricePoint], provider: &cost::Provider) -> Option<(f64, u32, f64)> {
    points
        .iter()
        .filter(|p| p.cpu >= provider.min_vcpu)
        .filter_map(|p| p.min_mem_mb.map(|mb| (p.cpu, mb)))
        .map(|(cpu, mb)| (cpu, mb, monthly_compute_cost_usd(provider, cpu, mb)))
        .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
}

fn read_capacity(root: &Path) -> Option<(f64, u32, BTreeMap<String, Option<u32>>)> {
    let summary = read_json(root, "results/capacity-summary.json")?;
    let cpu: f64 = summary["cpu"].as_str()?.parse().ok()?;
    let mem_mb = summary["mem_mb"].as_u64()? as u32;
    let mut map = BTreeMap::new();
    if let Some(results) = summary["results"].as_array() {
        for r in results {
            if let Some(stack) = r["stack"].as_str() {
                map.insert(stack.to_string(), r["max_rps"].as_u64().map(|v| v as u32));
            }
        }
    }
    Some((cpu, mem_mb, map))
}

fn read_json(root: &Path, rel: &str) -> Option<Value> {
    std::fs::read_to_string(root.join(rel)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn run_params_text(summary: &Value) -> String {
    let target_rate = summary["target_rate_rps"].as_u64().unwrap_or(0);
    let p99 = summary["sla"]["p99_ms"].as_f64().unwrap_or(SLA_P99_MS);
    let err = summary["sla"]["error_rate"].as_f64().unwrap_or(SLA_ERROR_RATE);
    let cpu = summary["cpu_limit"].as_str().unwrap_or("1.0");
    let repeats = summary["repeats"].as_u64().unwrap_or(0);
    let soak_passes = summary["soak_confirmation"]["required_consecutive_passes"].as_u64().unwrap_or(0);
    let soak_secs = summary["soak_confirmation"]["total_duration_secs"].as_u64().unwrap_or(0);
    format!(
        "target load {target_rate} req/s · SLA p99 < {p99:.0}ms, error rate < {:.0}% · CPU fixed at {cpu} vCPU · {repeats} burst repeats per step · soak requires {soak_passes} consecutive passes of {soak_secs}s each",
        err * 100.0
    )
}
