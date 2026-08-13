# Slashbench

A reproducible benchmark testing whether Rust backends actually cost less to run in the cloud than JVM/JS backends serving the same workload — built for the JavaZone 2026 talk "Rust will slash your backend costs."

Full methodology, experimental design, and timeline: see [`CLAUDE.md`](./CLAUDE.md).

## Status

Reference implementation (Rust + Rocket) is up and passing manual smoke tests. Five more stacks, the sweep/soak CLI orchestrator, and the report generator are in progress — see `CLAUDE.md` for the build plan.

## Running a stack locally

```bash
docker compose --profile rocket up -d --build
curl "http://localhost:8080/items?page=1&limit=3"
```

Reseed the dataset (100k rows) between runs:

```bash
./scripts/reseed.sh
```
