#!/usr/bin/env bash
# Runs the full official benchmark pipeline in order: sweep (memory floor +
# soak confirmation), capacity (fixed-size throughput ceiling), price-sweep
# (cpu x memory frontier for the cost charts). See CLAUDE.md's "Core
# measurement" and "Load testing & metrics" sections for the methodology
# behind each stage.
#
# Prerequisites:
#   - `slashbench dry-run --stack all` has been run for real, so
#     results/dry-run.json holds a trustworthy recommended_target_load_rps.
#     This script reads that value rather than hardcoding one, so it always
#     uses whatever the most recent dry-run actually locked in.
#   - The environment is already configured for wherever this runs, exactly
#     like every other slashbench command: DOCKER_HOST (+
#     SLASHBENCH_POSTGRES_DOCKER_HOST / SLASHBENCH_POSTGRES_HOST if Postgres
#     lives on its own host, per CLAUDE.md's Aug 19 entry), SLASHBENCH_BASE_URL,
#     SLASHBENCH_SKIP_BUILD as needed. This script sets none of these itself —
#     it inherits whatever's already exported in the calling shell.
#
# Usage:
#   scripts/official-run.sh [--soak-total-duration 10m]
# Run it with nohup/in a background session if driving it remotely over SSH.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SOAK_TOTAL_DURATION="10m"
if [ "${1:-}" = "--soak-total-duration" ]; then
  SOAK_TOTAL_DURATION="$2"
fi

BIN=./cli/target/release/slashbench

if [ ! -f results/dry-run.json ]; then
  echo "results/dry-run.json not found — run 'slashbench dry-run --stack all' first to lock a real target load." >&2
  exit 1
fi

TARGET_RATE=$(python3 -c "import json; print(json.load(open('results/dry-run.json'))['recommended_target_load_rps'])")
echo "=== Using target_rate=${TARGET_RATE}req/s from results/dry-run.json ==="

echo "=== [1/3] sweep --stack all --target-rate ${TARGET_RATE} --soak-total-duration ${SOAK_TOTAL_DURATION} ==="
"$BIN" sweep --stack all --target-rate "${TARGET_RATE}" --soak-total-duration "${SOAK_TOTAL_DURATION}"

# capacity needs one fixed memory size every stack can comfortably run at —
# the largest soak-confirmed footprint across all six, so the comparison
# stays fair (CLAUDE.md progress log, Aug 16: "same box for everyone").
MAX_MEM=$(python3 -c "
import json
with open('results/sweep-summary.json') as f:
    data = json.load(f)
vals = [r['soak_confirmed_mb'] for r in data['results'] if r.get('soak_confirmed_mb') is not None]
print(max(vals) if vals else '')
")

if [ -z "${MAX_MEM}" ]; then
  echo "No stack has a soak_confirmed_mb in results/sweep-summary.json — cannot proceed to capacity/price-sweep." >&2
  exit 1
fi
echo "=== Largest soak-confirmed footprint across all stacks: ${MAX_MEM} MiB -> using as capacity's fixed size ==="

echo "=== [2/3] capacity --stack all --mem-mb ${MAX_MEM} --cpu 1.0 ==="
"$BIN" capacity --stack all --mem-mb "${MAX_MEM}" --cpu 1.0

echo "=== [3/3] price-sweep --stack all --target-rate ${TARGET_RATE} ==="
"$BIN" price-sweep --stack all --target-rate "${TARGET_RATE}"

echo "=== OFFICIAL RUN COMPLETE ==="
