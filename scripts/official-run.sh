#!/usr/bin/env bash
# Runs the full official benchmark pipeline in order: dry-run (locks the
# shared target load), sweep (memory floor + soak confirmation), capacity
# (fixed-size throughput ceiling), price-sweep (cpu x memory frontier for
# the cost charts), report (renders the HTML report and archives this
# run's results/ into a dated zip under archives/ — see report.rs). See
# CLAUDE.md's "Core measurement" and "Load testing & metrics" sections for
# the methodology behind each stage.
#
# Prerequisites:
#   - The environment is already configured for wherever this runs, exactly
#     like every other slashbench command: DOCKER_HOST (+
#     SLASHBENCH_POSTGRES_DOCKER_HOST / SLASHBENCH_POSTGRES_HOST if Postgres
#     lives on its own host, per CLAUDE.md's Aug 19 entry), SLASHBENCH_BASE_URL,
#     SLASHBENCH_SKIP_BUILD as needed. This script sets none of these itself —
#     it inherits whatever's already exported in the calling shell.
#
# Usage:
#   scripts/official-run.sh [--soak-total-duration 15m] [--repeats 1] [--soak-repeats 1]
#   --repeats applies to dry-run/sweep/capacity/price-sweep's burst
#   measurements; --soak-repeats applies to sweep's soak-confirmation loop.
# Run it with nohup/in a background session if driving it remotely over SSH.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SOAK_TOTAL_DURATION="15m"
REPEATS=1
SOAK_REPEATS=1
while [ $# -gt 0 ]; do
  case "$1" in
    --soak-total-duration) SOAK_TOTAL_DURATION="$2"; shift 2 ;;
    --repeats) REPEATS="$2"; shift 2 ;;
    --soak-repeats) SOAK_REPEATS="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

BIN=./cli/target/release/slashbench

echo "=== [1/5] dry-run --stack all --repeats ${REPEATS} ==="
"$BIN" dry-run --stack all --repeats "${REPEATS}"

TARGET_RATE=$(python3 -c "import json; print(json.load(open('results/dry-run.json'))['recommended_target_load_rps'])")
echo "=== Locked target_rate=${TARGET_RATE}req/s from results/dry-run.json ==="

echo "=== [2/5] sweep --stack all --target-rate ${TARGET_RATE} --repeats ${REPEATS} --soak-repeats ${SOAK_REPEATS} --soak-total-duration ${SOAK_TOTAL_DURATION} ==="
"$BIN" sweep --stack all --target-rate "${TARGET_RATE}" --repeats "${REPEATS}" --soak-repeats "${SOAK_REPEATS}" --soak-total-duration "${SOAK_TOTAL_DURATION}"

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

echo "=== [3/5] capacity --stack all --mem-mb ${MAX_MEM} --cpu 1.0 --repeats ${REPEATS} ==="
"$BIN" capacity --stack all --mem-mb "${MAX_MEM}" --cpu 1.0 --repeats "${REPEATS}"

echo "=== [4/5] price-sweep --stack all --target-rate ${TARGET_RATE} --repeats ${REPEATS} ==="
"$BIN" price-sweep --stack all --target-rate "${TARGET_RATE}" --repeats "${REPEATS}"

echo "=== [5/5] report ==="
"$BIN" report

echo "=== OFFICIAL RUN COMPLETE ==="
