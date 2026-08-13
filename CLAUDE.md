# Slashbench

Reproducible cloud-cost benchmark behind the JavaZone 2026 talk **"Rust will slash your backend costs"** (Oslo, Sept 2–3, 2026).

Full rendered plan (same content as below, nicer to read): https://claude.ai/code/artifact/a9063189-6028-4c7b-9684-40b0e1dd7ac8

## Why this project exists

Two blog posts inspired the talk (aismith.dev "rust-green-backend-slashes-cloud-bills" and worldwithouteng.com "i-saved-87-percent-on-compute-costs-by-switching-languages"). Neither survives scrutiny: the first has no methodology at all (invented scenarios, no frameworks named, no benchmark); the second generalizes an 87% saving from one IO-bound service migrating Rails→Actix-web, explicitly untested on CPU-bound workloads. This project replaces both with something reproducible that others can re-run and check.

**Hypothesis:** Rust backends use less memory than JVM/JS backends serving the same workload, and that difference translates into a measurably smaller cloud bill.

## Timeline

- **Today's planning date:** Aug 12, 2026
- **Benchmark build deadline:** Aug 23, 2026 (~30h budget)
- **Aug 24–31:** write the talk from real results
- **Sept 2–3, 2026:** JavaZone 2026

| Date | Task | Hours | Deliverable |
|---|---|---|---|
| Wed Aug 13 | Lock spec: endpoints, schema, SLA thresholds, VM spec. Scaffold repo, docker-compose skeleton, seed script. | 4 | Repo skeleton |
| Thu Aug 14 | Rust + Rocket (reference implementation) | 3 | 1 stack running |
| Fri Aug 15 | Rust + Actix-web (port) · Java + Spring Boot (start) | 3 | 2 stacks running |
| Sat Aug 16 | Java + Spring Boot (finish) · Kotlin + Spring Boot (port) · Node + Hono (start) | 3.5 | 4 stacks running |
| Sun Aug 17 | Node + Hono (finish) · Bun + Hono (port) · dry-run all 6 uncapped, **lock the shared target load** at 60% of the weakest stack's ceiling | 3 | All 6 stacks running, target load pinned |
| Mon Aug 18 | CLI orchestrator: container lifecycle + sweep controller | 3 | Automated sweep, one stack end-to-end |
| Tue Aug 19 | CLI orchestrator: k6 invocation + docker-stats collection | 3 | Full metrics pipeline |
| Wed Aug 20 | Report generator: aggregation, cost calc, charts | 4 | First end-to-end report (any stack) |
| Thu Aug 21 | Provision GCE VMs, dry run all 6 stacks, fix bugs | 3 | Clean dry-run pass |
| Fri Aug 22 | Official runs: sweep + soak × 6 stacks (mostly unattended) · sync raw results off the VM into the repo **after each stack finishes**, not at the end | 2 active | Final dataset, committed |
| Sat Aug 23 | Write methodology/limitations section, polish README, tag a GitHub release with the dataset, tear down VMs, publish repo | 3 | **Benchmark done — deadline** |

## Experimental design

Six stacks, three families. Each family holds one variable constant and varies the other, producing two "control" results before the headline cross-family comparison.

| Family | Stack | Constant | Varies | Tests |
|---|---|---|---|---|
| JVM | Java + Spring Boot | Framework (Spring Boot) | Source language | Java vs Kotlin on same runtime (expect: barely) |
| JVM | Kotlin + Spring Boot | | | |
| JS | Node.js + Hono | Framework (Hono, runs on both) | Runtime engine | V8/libuv vs JavaScriptCore/Bun (expect: measurable) |
| JS | Bun + Hono | | | |
| Rust | Rust + Rocket | Language (compiled) | Framework | Framework choice when compiled (expect: barely) |
| Rust | Rust + Actix-web | | | |

**Why Hono for both JS runtimes:** using Express for Node and Elysia for Bun would confound runtime with framework. Hono has first-class adapters for both (`@hono/node-server`, native Bun support), so runtime is the only variable that changes.

**Anticipate in Q&A:** "what about GraalVM native image / Quarkus / Micronaut?" — the JVM ecosystem's actual answer to memory bloat. Out of scope for the 11-day build; decide whether to name it as a caveat up front (stronger) or hold for Q&A.

## Benchmark application spec

Same functionality, schema, and seed data, implemented six times. Deliberately simple — this is a memory/runtime-overhead benchmark, not a framework-feature bakeoff.

| Endpoint | Behavior |
|---|---|
| `POST /items` | Validate, insert a row, return created item as JSON (write path + serialization) |
| `GET /items/:id` | Point read by primary key, JSON (read path + connection pool under concurrency) |
| `GET /items?page=&limit=` | Paginated list query, JSON array (query planning + larger payload serialization) |

Single shared Postgres instance, identical schema and seed script, reseeded between stacks.

**DB access tier — decided:** raw drivers only, no ORM, matched across all six so language/runtime overhead isn't confounded with ORM overhead:
- Java/Kotlin: `JdbcTemplate` / Spring Data JDBC (no Hibernate)
- Node/Bun: `postgres` (porsager/postgres.js) — same driver both runtimes
- Rust: `sqlx` — same crate under Rocket and Actix-web

Trade-off accepted: doesn't reflect a typical Hibernate-based Spring app in production — worth one framing sentence in the talk.

## Fairness & environment rules

- All six built from the same `docker-compose.yml` pattern, one profile per stack.
- Run **one stack at a time** against a freshly reseeded DB.
- Load generator (k6) runs on a **separate** machine from the service under test — otherwise k6's own CPU pollutes the container's measured stats, and loopback hides real latency.
- Fixed-spec cloud VMs, not a laptop — two `e2-standard-4` GCE VMs (one "target," one "load generator"), so anyone re-running this gets the same playing field.
- Randomize run order across repeats to avoid host-drift bias (thermal creep, disk cache state).

## Core measurement: minimum viable footprint

Cloud Run instance-based billing charges for CPU/memory **allocated** to a container for as long as it runs, not what it uses internally. The number that drives the bill is: **the smallest instance size each stack can be provisioned at while still meeting an SLA under the same load.**

Sweep procedure (automated by the CLI):

1. Dry-run **all six** stacks uncapped to find each one's max sustainable throughput. Set the shared target load at ~60% of the **weakest** stack's ceiling — Rocket will very likely be the fastest of the six, so it cannot be the anchor even though it's built first. This step happens once all six are running (~Aug 17), not right after Rocket alone.
2. SLA (placeholder, confirm after step 1): p99 latency < 200ms and error rate < 1%, sustained 5 min at target load.
3. Fix CPU generously (1 vCPU) — memory is the headline claim, not CPU. Step memory down (512→384→256→192→128→96→64 MiB…) via `docker run --memory` until the SLA breaks. Last passing value = that stack's minimum viable footprint.
4. At that minimum footprint, run one ~1 hour soak test — surfaces JVM GC pause growth or slow leaks, makes for a strong "memory over time" slide against Rust's flat line.

Each sweep step repeated **3×** minimum; report median with IQR, not a single sample.

## Load testing & metrics

- **k6**: one script hits all three endpoints in a realistic mix (e.g. 70% reads / 30% writes), reused unmodified across every stack.
- **docker stats / cgroup v2**: CPU% and RSS sampled every second per run.
- Captured per run: achieved RPS, p50/p95/p99 latency, error rate, mean & peak CPU%, mean & peak RSS, cold-start time, and (from the sweep) minimum viable memory footprint at the SLA.

## Cost model

GCP Cloud Run, instance-based tier (always-on, matches "backend service that must stay warm" rather than scale-to-zero):

| Unit | Price (beyond free tier) |
|---|---|
| per vCPU-second | $0.0000336 |
| per GiB-second | $0.0000035 |
| per million requests | $0.40 |

```
monthly_cost = (vCPU_allocated × 0.0000336 + GiB_allocated × 0.0000035) × seconds_per_month
             + (requests_per_month ÷ 1_000_000) × 0.40
```

**Verify before publishing** — cloud pricing changes. Keep these in a versioned `pricing.json` with a "last verified" date and a link to https://cloud.google.com/run/pricing, not hardcoded in the report generator.

## CLI tool architecture

One command runs everything end to end: `slashbench run --stack all --sweep --soak`. Written in Rust (fitting, given the subject).

```
CLI: slashbench run
  → reseed Postgres
  → start one stack's container at current memory ceiling
  → warm-up window
  → k6 load test from load-gen VM
  → sample docker stats (CPU/RSS, 1s interval)
  → SLA met? yes → step memory ceiling down, repeat
             no  → record minimum viable footprint for this stack
  → soak test at minimum footprint
  → next stack
  → aggregate results → cost model → report
```

Report output: raw JSON/CSV per run (so others can recompute with their own provider's pricing) plus a generated Markdown/HTML report with the footprint chart, the memory-over-time soak chart, and the methodology/limitations section inline.

## Known limitations to state up front in the report

- Single-machine measurement, not a distributed production topology.
- JVM steady-state throughput improves after JIT warm-up — the soak test's early window will look worse for Java/Kotlin than its later window. Report both windows, don't average them away.
- Six implementations by one person — skill/effort asymmetry across stacks is a real confound. Mitigated by using each ecosystem's standard idioms (not hand-tuned tricks) and by open-sourcing the code so others can flag an unfair implementation.
- Instance-based billing (always-on) is one Cloud Run billing mode, not the only one — request-based (scale-to-zero) billing tells a different, concurrency-dependent story. Named as a deliberate scope choice, not an oversight.

## Decisions log

- **DB access tier:** raw drivers (`JdbcTemplate`, `postgres.js`, `sqlx`), decided Aug 12, 2026.
- **SLA/target-load numbers:** stay placeholders (p99<200ms, error<1%) until Aug 17 dry-run data exists; the *method* is fixed now — anchor to the weakest of all six stacks, never to Rocket alone.
- **Repo visibility:** public on GitHub from day one, decided Aug 12, 2026 — nothing withheld during the build.

## Data safety & backup

The code is the easy part — commit and push to GitHub daily and it's already redundant. The actual risk is the **sweep/soak dataset**, generated on disposable GCE VMs intended to be torn down right after the deadline.

- CLI orchestrator writes results incrementally (append-only JSON Lines per completed sweep step), not one buffered write at the end — a crashed VM mid-sweep then costs the last step, not the whole day's runs.
- Sync results out of the VM into the git repo **after each stack finishes** (Aug 22), not after all six.
- Before terminating the VMs on Aug 23, tag a GitHub release with the final raw dataset attached — the durable, timestamped copy the talk's numbers are built on.
- Repo is public from day one anyway, so no secrets-in-git concern beyond not hardcoding the GCE project ID / billing account into committed config.

## Progress log

- **Aug 13, 2026:** Repo scaffolded — `docker-compose.yml`, `db/init.sql` + `db/seed.sql` (100k deterministic rows) + `db/reset.sql`, `scripts/reseed.sh`. Rust/Rocket reference implementation done at `services/rocket/` (sqlx raw driver, all 3 endpoints), builds via multi-stage Dockerfile, verified with `docker compose --profile rocket up -d --build` + curl against all three endpoints (201/200/404 all correct, pagination + total count correct). Idle container RSS ~2.3 MiB (Postgres ~41 MiB) — not a real result yet, just a sanity check; the actual data point is the sweep-under-load minimum footprint (§4), not idle memory. Git repo initialized locally, not yet pushed to GitHub, no commit made yet.
- **Aug 13, 2026 (later same day):** Ported to Rust + Actix-web at `services/actix-web/` — same schema, same raw `sqlx` queries as Rocket, added as an `actix-web` profile in `docker-compose.yml`. Verified against all three endpoints, identical JSON shapes/status codes to Rocket. Idle RSS ~8.6 MiB vs Rocket's ~2.3 MiB — an early signal that framework choice isn't perfectly negligible even within Rust, but this is idle-only on n=1, not the sweep-under-load result the report will actually use; don't read into it yet.
- Empty stub directories exist for the remaining four stacks: `services/spring-java/`, `services/spring-kotlin/`, `services/node-hono/`, `services/bun-hono/`.

## Open next step

Java + Spring Boot as the second reference implementation (JVM family, §1). After that: Kotlin + Spring Boot port.
