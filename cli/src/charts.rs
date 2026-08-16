//! Builds Chart.js configuration objects — all rendering, scaling, ticks,
//! tooltips, and legends are Chart.js's job (vendored in cli/assets/, see
//! that directory's README). This module only assembles the data/config
//! JSON each chart needs; it does not draw anything itself.
//!
//! Canvas text/fills can't inherit CSS custom properties the way DOM/SVG
//! can — Chart.js draws with the Canvas 2D API, not CSS. So colors are
//! passed here as *keys* (family name, status name, "ink") rather than hex
//! strings, serialized as `"__COLOR:key__"` placeholders and spliced into
//! real (unquoted) JS references — `FAMILY_COLORS.rust`, etc. — against
//! color tables set up once in THEME_INIT_SCRIPT after a runtime
//! light/dark check. This keeps every hex value in one place (THEME_INIT_SCRIPT)
//! instead of duplicated per chart call site.

use serde_json::{json, Value};

/// A color key: a family name ("rust"/"jvm"/"js"), a status name
/// ("good"/"critical"), or "ink" for primary text drawn on canvas.
fn color_placeholder(key: &str) -> String {
    format!("__COLOR:{key}__")
}

fn splice_colors(json_str: &str) -> String {
    let mut out = String::with_capacity(json_str.len());
    let mut rest = json_str;
    while let Some(start) = rest.find("\"__COLOR:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 9..]; // skip `"__COLOR:`
        let end = after.find("__\"").expect("well-formed color placeholder");
        let key = &after[..end];
        let js_ref = match key {
            "good" => "STATUS_COLORS.good".to_string(),
            "critical" => "STATUS_COLORS.critical".to_string(),
            "ink" => "INK_COLOR".to_string(),
            "neutral" => "NEUTRAL_COLOR".to_string(),
            family => format!("FAMILY_COLORS.{family}"),
        };
        out.push_str(&js_ref);
        rest = &after[end + 3..]; // skip `__"`
    }
    out.push_str(rest);
    out
}

pub struct LineSeries {
    pub labels: Vec<String>,
    pub values: Vec<f64>,
    /// Per-point color *key* (see module docs), same length as `values` —
    /// used to mark a canary failure or an SLA breach in the status-critical
    /// color without a second series (keeps this single-series, no legend).
    pub point_color_keys: Vec<String>,
    pub line_color_key: String,
}

/// Returns a `new Chart(...)` statement targeting `canvas_id`. Callers
/// collect these into one script block at the end of the page, after every
/// canvas already exists in the DOM.
pub fn line_chart_init(canvas_id: &str, title: &str, x_label: &str, y_label: &str, series: &LineSeries) -> String {
    let point_colors: Vec<String> = series.point_color_keys.iter().map(|k| color_placeholder(k)).collect();
    let config = json!({
        "type": "line",
        "data": {
            "labels": series.labels,
            "datasets": [{
                "data": series.values,
                "borderColor": color_placeholder(&series.line_color_key),
                "backgroundColor": color_placeholder(&series.line_color_key),
                "pointBackgroundColor": point_colors,
                "pointBorderColor": "transparent",
                "borderWidth": 2,
                "pointRadius": 4,
                "pointHoverRadius": 6,
                "tension": 0,
                "fill": false,
            }],
        },
        "options": base_options(title, Some(x_label), Some(y_label)),
    });
    let config_str = splice_colors(&serde_json::to_string(&config).unwrap());
    format!("new Chart(document.getElementById({}), {config_str});", js_str(canvas_id))
}

pub struct Bar {
    pub label: String,
    pub value: f64,
    /// Color key (family name) — see module docs.
    pub color_key: String,
    pub value_label: String,
    /// Full-detail text for the hover tooltip, when it's too long to force
    /// onto the bar itself — see `provider_bar_chart_init`. `None` reuses
    /// `value_label` (the plain `bar_chart_init` path doesn't read this
    /// field at all, so it's a no-op there).
    pub tooltip_label: Option<String>,
}

fn bar_series_json(bars: &[Bar]) -> Value {
    let labels: Vec<&str> = bars.iter().map(|b| b.label.as_str()).collect();
    let values: Vec<f64> = bars.iter().map(|b| b.value).collect();
    let value_labels: Vec<&str> = bars.iter().map(|b| b.value_label.as_str()).collect();
    let tooltip_labels: Vec<&str> = bars.iter().map(|b| b.tooltip_label.as_deref().unwrap_or(&b.value_label)).collect();
    let colors: Vec<String> = bars.iter().map(|b| color_placeholder(&b.color_key)).collect();
    json!({ "labels": labels, "values": values, "valueLabels": value_labels, "tooltipLabels": tooltip_labels, "colors": colors })
}

/// One named alternative dataset for a provider-switchable chart — see
/// `provider_bar_chart_init`.
pub struct ProviderVariant {
    pub key: String,
    pub bars: Vec<Bar>,
}

/// A bar chart with a provider switcher: renders `variants[0]` (or whichever
/// key matches `default_key`) initially, and registers every variant's data
/// under `PROVIDER_CHART_DATA[canvas_id]` so the page-wide `setProvider(key)`
/// function (see `PROVIDER_SWITCH_SCRIPT`) can swap it later — one shared
/// control can drive several charts at once, since providers are a
/// pricing-formula choice, not a per-chart concept.
pub fn provider_bar_chart_init(canvas_id: &str, title: &str, x_label: &str, variants: &[ProviderVariant], default_key: &str) -> String {
    let default_bars = &variants.iter().find(|v| v.key == default_key).unwrap_or(&variants[0]).bars;
    let max_value = variants.iter().flat_map(|v| v.bars.iter()).map(|b| b.value).fold(0.0_f64, f64::max).max(1.0);

    let mut options = base_options(title, Some(x_label), None);
    options["indexAxis"] = json!("y");
    options["scales"]["x"]["max"] = json!(max_value * 1.3);
    options["plugins"]["tooltip"] = json!({ "callbacks": { "label": "__TOOLTIP_LABEL__" } });

    let config = json!({
        "type": "bar",
        "data": {
            "labels": default_bars.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(),
            "datasets": [{
                "data": default_bars.iter().map(|b| b.value).collect::<Vec<_>>(),
                "backgroundColor": default_bars.iter().map(|b| color_placeholder(&b.color_key)).collect::<Vec<_>>(),
                "borderRadius": 4,
                "maxBarThickness": 22,
                "datalabels": {
                    "display": true,
                    "anchor": "end",
                    "align": "end",
                    "formatter": "__CURRENT_VALUE_LABEL__",
                    "color": color_placeholder("ink"),
                },
            }],
        },
        "options": options,
    });

    let mut data_registry = serde_json::Map::new();
    for variant in variants {
        data_registry.insert(variant.key.clone(), bar_series_json(&variant.bars));
    }
    data_registry.insert("current".to_string(), bar_series_json(default_bars));

    let registry_js = splice_colors(&serde_json::to_string(&Value::Object(data_registry)).unwrap());
    let config_str = splice_colors(&serde_json::to_string(&config).unwrap())
        .replace(
            "\"__CURRENT_VALUE_LABEL__\"",
            &format!("function(v,ctx){{return PROVIDER_CHART_DATA[{}].current.valueLabels[ctx.dataIndex];}}", js_str(canvas_id)),
        )
        .replace(
            "\"__TOOLTIP_LABEL__\"",
            &format!("function(ctx){{return PROVIDER_CHART_DATA[{}].current.tooltipLabels[ctx.dataIndex];}}", js_str(canvas_id)),
        );

    format!(
        "PROVIDER_CHART_DATA[{cid}] = {registry_js};\nCHART_INSTANCES[{cid}] = new Chart(document.getElementById({cid}), {config_str});",
        cid = js_str(canvas_id)
    )
}

/// One shared switcher for every chart registered via `provider_bar_chart_init`
/// — updates each chart's labels/values/colors from its registered variant
/// and re-renders. The datalabels formatter above reads `.current` at call
/// time rather than closing over a fixed array, so it stays correct after a
/// swap without needing its own update logic here.
pub const PROVIDER_SWITCH_SCRIPT: &str = r#"
function setProvider(providerKey) {
  Object.keys(PROVIDER_CHART_DATA).forEach(function (canvasId) {
    var chart = CHART_INSTANCES[canvasId];
    var registry = PROVIDER_CHART_DATA[canvasId];
    var variant = registry[providerKey];
    if (!chart || !variant) return;
    registry.current = variant;
    chart.data.labels = variant.labels;
    chart.data.datasets[0].data = variant.values;
    chart.data.datasets[0].backgroundColor = variant.colors;
    chart.update();
  });
  document.querySelectorAll('.provider-tab').forEach(function (btn) {
    btn.classList.toggle('active', btn.getAttribute('data-provider') === providerKey);
  });
}
"#;

/// Horizontal bar chart with always-visible value labels at the bar end
/// (chartjs-plugin-datalabels), per the report's "compare across projects
/// at a glance" requirement — the grid's headline chart. `log_scale` is for
/// data spanning a wide range (e.g. 12 MiB to 256 MiB) where a linear axis
/// would make the smallest bars invisible.
pub fn bar_chart_init(canvas_id: &str, title: &str, x_label: &str, bars: &[Bar], log_scale: bool) -> String {
    let labels: Vec<&str> = bars.iter().map(|b| b.label.as_str()).collect();
    let raw_values: Vec<f64> = bars.iter().map(|b| b.value).collect();
    let colors: Vec<String> = bars.iter().map(|b| color_placeholder(&b.color_key)).collect();
    let value_labels: Vec<&str> = bars.iter().map(|b| b.value_label.as_str()).collect();

    let mut options = base_options(title, Some(x_label), None);
    options["indexAxis"] = json!("y");
    let max_value = raw_values.iter().cloned().fold(0.0_f64, f64::max).max(1.0);

    // Chart.js's built-in `type: "logarithmic"` scale generates its own
    // tick set (10,20,30...90,100,150,200...) that's mathematically correct
    // but visually uneven — sub-ticks compress toward the top of each
    // decade. Tried overriding via the documented `afterBuildTicks` hook
    // (confirmed via the generated JS that Chart.js does call it) but
    // LogarithmicScale still re-derives its own ticks afterward, undocumented
    // in the minified bundle. More robust: pre-transform the data to log2
    // space and use a plain linear scale — guaranteed equal spacing per
    // power of 2 (matches this data's own doubling memory ladder, and the
    // classic evenly-spaced-by-decade log scale, e.g. the Wikipedia
    // logarithmic-scale diagram), no scale-type special-casing to fight.
    let values: Vec<f64> = if log_scale { raw_values.iter().map(|v| v.log2()).collect() } else { raw_values.clone() };

    if log_scale {
        let min_value = raw_values.iter().cloned().fold(f64::INFINITY, f64::min).max(1.0);
        let log_ticks = power_of_two_ticks(min_value, max_value);
        options["scales"]["x"]["min"] = json!(log_ticks.first().copied().unwrap_or(1.0).log2());
        options["scales"]["x"]["max"] = json!(log_ticks.last().copied().unwrap_or(max_value).log2());
        options["scales"]["x"]["ticks"] = json!({ "stepSize": 1, "callback": "__LOG_TICK_LABEL__" });
    } else {
        // An explicit (not "suggested") max, with headroom past the largest
        // bar — otherwise a bar near the auto-scaled max leaves no room for
        // its own end-label before the canvas edge, and the label clips.
        options["scales"]["x"]["max"] = json!(max_value * 1.25);
    }

    let config = json!({
        "type": "bar",
        "data": {
            "labels": labels,
            "datasets": [{
                "data": values,
                "backgroundColor": colors,
                "borderRadius": 4,
                "maxBarThickness": 22,
                "datalabels": {
                    "display": true,
                    "anchor": "end",
                    "align": "end",
                    "formatter": "__VALUE_LABELS__",
                    "color": color_placeholder("ink"),
                },
            }],
        },
        "options": options,
    });

    // serde_json can't express a JS function as a value — splice a real one
    // in after serializing. Still just wiring data into the library's own
    // documented formatter/callback shapes, not drawing anything ourselves.
    let value_labels_json = serde_json::to_string(&value_labels).unwrap();
    let mut config_str = splice_colors(&serde_json::to_string(&config).unwrap())
        .replace("\"__VALUE_LABELS__\"", &format!("function(v,ctx){{return {value_labels_json}[ctx.dataIndex];}}"));
    if log_scale {
        config_str = config_str.replace("\"__LOG_TICK_LABEL__\"", "function(v){return Math.round(Math.pow(2,v));}");
    }

    format!("new Chart(document.getElementById({}), {config_str});", js_str(canvas_id))
}

/// Clean power-of-2 ticks spanning one step below the data minimum to one
/// step above the maximum, e.g. [8,16,32,64,128,256,512] for data in
/// [12,256] — matches this benchmark's own doubling memory ladder and, on a
/// true log scale, lands every tick at equal visual spacing (see
/// `bar_chart_init`'s log_scale path).
fn power_of_two_ticks(min_value: f64, max_value: f64) -> Vec<f64> {
    let mut start = 1.0_f64;
    while start > min_value {
        start /= 2.0;
    }
    while start * 2.0 <= min_value {
        start *= 2.0;
    }
    let mut ticks = vec![start];
    let mut v = start;
    while v <= max_value {
        v *= 2.0;
        ticks.push(v);
    }
    ticks
}

pub struct StackedCostBar {
    pub label: String,
    pub cpu_floor_usd: f64,
    pub memory_usd: f64,
    /// Color key for the memory segment (family name) — see module docs.
    /// The CPU-floor segment always uses the neutral color, since it's
    /// identical across every stack and isn't the thing being compared.
    pub color_key: String,
}

/// Stacked horizontal bar: the fixed 1-vCPU always-on floor (identical for
/// every stack under Cloud Run's instance-based billing, confirmed against
/// Cloud Run's own docs — see CLAUDE.md progress log) versus the
/// memory-driven cost that actually varies by stack. Deliberately no
/// datalabels here: the memory segment is often just a few pixels wide next
/// to the ~$88 floor, and a label that doesn't fit belongs in the tooltip/
/// table, not forced onto (or clipped by) a sliver of a bar — see
/// marks-and-anatomy's rule on labels that don't fit.
pub fn stacked_cost_chart_init(canvas_id: &str, title: &str, x_label: &str, bars: &[StackedCostBar]) -> String {
    let labels: Vec<&str> = bars.iter().map(|b| b.label.as_str()).collect();
    let cpu_values: Vec<f64> = bars.iter().map(|b| b.cpu_floor_usd).collect();
    let mem_values: Vec<f64> = bars.iter().map(|b| b.memory_usd).collect();
    let mem_colors: Vec<String> = bars.iter().map(|b| color_placeholder(&b.color_key)).collect();

    let mut options = base_options(title, Some(x_label), None);
    options["indexAxis"] = json!("y");
    options["plugins"]["legend"]["display"] = json!(true);
    options["scales"]["x"]["stacked"] = json!(true);
    options["scales"]["y"]["stacked"] = json!(true);

    let config = json!({
        "type": "bar",
        "data": {
            "labels": labels,
            "datasets": [
                {
                    "label": "1 vCPU floor (fixed, always-on billing)",
                    "data": cpu_values,
                    "backgroundColor": color_placeholder("neutral"),
                    "maxBarThickness": 22,
                },
                {
                    "label": "Memory-driven cost (varies by stack)",
                    "data": mem_values,
                    "backgroundColor": mem_colors,
                    "maxBarThickness": 22,
                },
            ],
        },
        "options": options,
    });

    let config_str = splice_colors(&serde_json::to_string(&config).unwrap());
    format!("new Chart(document.getElementById({}), {config_str});", js_str(canvas_id))
}

fn base_options(title: &str, x_label: Option<&str>, y_label: Option<&str>) -> Value {
    let mut scales = json!({});
    if let Some(x) = x_label {
        scales["x"] = json!({ "title": { "display": true, "text": x } });
    }
    if let Some(y) = y_label {
        scales["y"] = json!({ "beginAtZero": true, "title": { "display": true, "text": y } });
    }
    json!({
        "responsive": true,
        "plugins": {
            "legend": { "display": false },
            "title": { "display": true, "text": title },
            // chartjs-plugin-datalabels is registered globally and defaults
            // to labeling every point — fine for the bar chart's sparse
            // per-bar value labels, illegible on a many-point line chart.
            // Off by default here; bar_chart_init opts back in explicitly.
            "datalabels": { "display": false },
        },
        "scales": scales,
    })
}

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// Sets Chart.js's global text/gridline defaults from the page's actual
/// color scheme, and defines the FAMILY_COLORS/STATUS_COLORS/INK_COLOR
/// tables every chart config references (see `splice_colors`). All hex
/// values here match the validated palette (light: run
/// `validate_palette.js "#2a78d6,#eb6834,#1baf7a" --mode light --pairs all`;
/// dark: same script with the dark hexes and `--surface "#1a1a19"` —
/// both passed all checks, see CLAUDE.md progress log). Must run once,
/// before any `new Chart(...)` call.
pub const THEME_INIT_SCRIPT: &str = r#"
var CHART_INSTANCES = {};
var PROVIDER_CHART_DATA = {};
Chart.register(ChartDataLabels);
var isDarkMode = window.matchMedia('(prefers-color-scheme: dark)').matches;
var FAMILY_COLORS = isDarkMode
  ? { rust: '#3987e5', jvm: '#d95926', js: '#199e70' }
  : { rust: '#2a78d6', jvm: '#eb6834', js: '#1baf7a' };
var STATUS_COLORS = { good: '#0ca30c', critical: '#d03b3b' };
var INK_COLOR = isDarkMode ? '#ffffff' : '#0b0b0b';
var NEUTRAL_COLOR = isDarkMode ? '#52514e' : '#c3c2b7';
Chart.defaults.color = isDarkMode ? '#c3c2b7' : '#52514e';
Chart.defaults.borderColor = isDarkMode ? '#2c2c2a' : '#e1e0d9';
Chart.defaults.font.family = "system-ui, -apple-system, 'Segoe UI', sans-serif";
"#;
