#!/usr/bin/env bash
# Truncate and reseed the items table via the running postgres compose service.
# Run this before benchmarking each stack so every run starts from the same dataset.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
docker compose exec -T postgres psql -U slashbench -d slashbench < db/reset.sql
