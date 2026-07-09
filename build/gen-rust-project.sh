#!/usr/bin/env bash
# Generate rust-project.json for rust-analyzer from Buck2 targets.
#
# Run from the repo root:
#   bash build/gen-rust-project.sh
#
# On first run Buck2 materialises source archives (slow); subsequent runs hit cache.
# rust-project.json is gitignored — regenerate after adding/removing deps.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BUCK2="${BUCK2:-buck2}"

echo "[ra] Querying Buck2 target graph..." >&2
BXL_JSON=$("$BUCK2" bxl \
    prelude//rust/rust-analyzer:resolve_deps.bxl:resolve_targets \
    -- --targets //workspace/... 2>&1 | tail -1)

echo "[ra] Converting to rust-project.json..." >&2
python3 "$ROOT/build/gen-rust-project.py" "$BXL_JSON" > "$ROOT/rust-project.json"

echo "[ra] Done → rust-project.json" >&2
