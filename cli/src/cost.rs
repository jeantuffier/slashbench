//! Multi-provider serverless-container cost model. Verify all figures
//! against each provider's own pricing page before publishing — pricing
//! changes over time, and these are named constants specifically so
//! updating them later is a one-line fix, not a hunt through report.rs.
//!
//! **GCP's 1-vCPU floor is GCP-specific, not an industry norm** — confirmed
//! via each provider's own docs (see CLAUDE.md progress log, Aug 16), not
//! assumed. AWS Fargate, Azure Container Apps, and Scaleway Containers all
//! support fractional CPU as their *only* billing model (0.25, 0.25, and
//! 0.07 vCPU minimums respectively) — there's no GCP-style split between an
//! "instance-based" tier with a floor and a "request-based" tier without
//! one. Only GCP has that split, which is why `GCP_INSTANCE_BASED` exists
//! as its own entry distinct from `GCP_REQUEST_BASED` — for every other
//! provider, the single `Provider` entry already reflects their one real
//! billing mode.

pub struct Provider {
    /// Short slug used as the chart-switcher key and HTML `data-provider`
    /// attribute — kept on the provider itself so report.rs doesn't need a
    /// second, parallel (key, label) list that could drift out of sync.
    pub key: &'static str,
    pub name: &'static str,
    pub price_per_vcpu_second: f64,
    pub price_per_gib_second: f64,
    pub price_per_million_requests: f64,
    /// The minimum vCPU this provider's billing model allows — 1.0 for GCP's
    /// instance-based tier is the outlier; every other entry here is well
    /// under 1.0, confirmed against that provider's own docs.
    pub min_vcpu: f64,
}

/// GCP Cloud Run, instance-based (always-on) tier. Source:
/// <https://cloud.google.com/run/pricing>. The 1-vCPU minimum is confirmed
/// via <https://docs.cloud.google.com/run/docs/configuring/services/cpu>
/// ("fractional CPU requires request-based billing").
pub const GCP_INSTANCE_BASED: Provider = Provider {
    key: "gcp_instance",
    name: "GCP Cloud Run (instance-based)",
    price_per_vcpu_second: 0.0000336,
    price_per_gib_second: 0.0000035,
    price_per_million_requests: 0.40,
    min_vcpu: 1.0,
};

/// GCP Cloud Run, request-based tier — same source as above. Allows
/// fractional CPU down to 0.08 vCPU, in 0.001 increments.
pub const GCP_REQUEST_BASED: Provider = Provider {
    key: "gcp_request",
    name: "GCP Cloud Run (request-based)",
    price_per_vcpu_second: 0.000024,
    price_per_gib_second: 0.0000025,
    price_per_million_requests: 0.40,
    min_vcpu: 0.08,
};

/// AWS Fargate (ECS/EKS — identical pricing either way), Linux/x86,
/// us-east-1. Source: <https://aws.amazon.com/fargate/pricing/>. No
/// per-request fee; no separate always-on/request-based split — this one
/// per-second model is Fargate's only billing mode, min 0.25 vCPU.
pub const AWS_FARGATE: Provider =
    Provider { key: "aws_fargate", name: "AWS Fargate", price_per_vcpu_second: 0.000011244, price_per_gib_second: 0.000001235, price_per_million_requests: 0.0, min_vcpu: 0.25 };

/// Azure Container Apps, Consumption plan, active rate. Source (rates):
/// <https://azure.microsoft.com/en-us/pricing/details/container-apps/>;
/// source (CPU floor): Microsoft Learn's "vCPU and memory allocation
/// requirements" table, min 0.25 vCPU. Same Consumption plan handles both
/// scale-to-zero and always-warm (at a reduced "idle" rate) — no separate
/// tier with a different CPU floor.
pub const AZURE_CONTAINER_APPS: Provider = Provider {
    key: "azure_container_apps",
    name: "Azure Container Apps",
    price_per_vcpu_second: 0.000024,
    price_per_gib_second: 0.000003,
    price_per_million_requests: 0.40,
    min_vcpu: 0.25,
};

/// Scaleway Serverless Containers, Paris region. CPU floor from the
/// Containers Limitations doc's resource table (70-6000 mvCPU), i.e. a 0.07
/// vCPU minimum — the lowest of any provider checked. No per-request fee.
/// Per-unit rates verified against a real Scaleway invoice (May 2026 billing
/// period, "Mighty Bookshelf" project, 4 container resources), not just the
/// public pricing page: 135,562 vcpu-seconds billed at €1.36 confirms
/// €0.00001/vCPU-s; 169,453 GB-seconds billed at €0.17 gives €0.000001/GB-s
/// — the public page's figure had been misread as €0.000002/GB-s (2x too
/// high) before this invoice caught it. Converted here at an illustrative
/// ~1.08 USD/EUR — update this rate before publishing, it drifts. (The
/// invoice also nets out a monthly free-tier credit that fully offset both
/// charges to €0 that month; like every other provider here, this model
/// prices usage beyond the free tier, matching the GCP table's own framing.)
const EUR_TO_USD: f64 = 1.08;
pub const SCALEWAY_CONTAINERS: Provider = Provider {
    key: "scaleway_containers",
    name: "Scaleway Containers",
    price_per_vcpu_second: 0.00001 * EUR_TO_USD,
    price_per_gib_second: 0.000001 * EUR_TO_USD,
    price_per_million_requests: 0.0,
    min_vcpu: 0.07,
};

pub const ALL_PROVIDERS: &[&Provider] = &[&GCP_INSTANCE_BASED, &GCP_REQUEST_BASED, &AWS_FARGATE, &AZURE_CONTAINER_APPS, &SCALEWAY_CONTAINERS];

// 730 hours/month is the standard "average month" convention used by cloud
// cost calculators (30.42 days), not a plain 30-day month.
pub const AVG_SECONDS_PER_MONTH: f64 = 730.0 * 3600.0;

/// The fixed vCPU-allocation cost — identical for every stack tested at the
/// same vCPU count, real money nonetheless.
pub fn cpu_floor_cost_usd(provider: &Provider, vcpu: f64) -> f64 {
    vcpu * provider.price_per_vcpu_second * AVG_SECONDS_PER_MONTH
}

/// The memory-driven cost only — the number that actually varies by stack.
pub fn memory_only_cost_usd(provider: &Provider, mem_mb: u32) -> f64 {
    let gib = mem_mb as f64 / 1024.0;
    gib * provider.price_per_gib_second * AVG_SECONDS_PER_MONTH
}

/// Total monthly compute cost (vCPU + memory allocation) for a given
/// provider — deliberately excludes per-request cost. At a shared target
/// load, request cost is identical across every stack and would just be a
/// constant added to each number, shrinking the visible relative
/// difference. See `monthly_cost_with_requests_usd` for the full bill.
pub fn monthly_compute_cost_usd(provider: &Provider, vcpu: f64, mem_mb: u32) -> f64 {
    cpu_floor_cost_usd(provider, vcpu) + memory_only_cost_usd(provider, mem_mb)
}

/// The full bill, including request cost, for anyone who wants it — not
/// used in the comparison charts for the reason above, but the formula
/// should still be available and correct.
pub fn monthly_cost_with_requests_usd(provider: &Provider, vcpu: f64, mem_mb: u32, requests_per_month: f64) -> f64 {
    monthly_compute_cost_usd(provider, vcpu, mem_mb) + (requests_per_month / 1_000_000.0) * provider.price_per_million_requests
}
