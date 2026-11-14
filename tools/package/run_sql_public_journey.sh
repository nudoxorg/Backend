#!/usr/bin/env bash
# Build the SQLite worker, measure its final bytes, build the owner, CLI and MCP
# with that identity compiled in, lay them out as a bundle, and run the public
# SQL journey against it (see sql_public_journey.py).
#
#   run_sql_public_journey.sh [BUNDLE_DIR] [OUTPUT_DIR]
#
# Development mode: debug builds skip the account gate. Offline; uses the
# caller's CARGO_TARGET_DIR / CARGO_HOME / toolchain. The worker is built with
# debug info stripped because the checked launch hashes it on every use.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
bundle=${1:-$(mktemp -d)/bundle}
output=${2:-$(mktemp -d)/journey}
target=${CARGO_TARGET_DIR:-$repo/target}
worker_target="$target/sqlite-frontend-worker"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_STRIP=symbols
cargo build --locked --offline --manifest-path "$repo/frontends/sqlite/Cargo.toml" --bin sqlite-worker --target-dir "$worker_target"
worker="$worker_target/debug/sqlite-worker"
unset CARGO_PROFILE_DEV_DEBUG CARGO_PROFILE_DEV_STRIP
anchor=$(cargo run --locked --offline -q -p backend-compile --example sql_worker_identity -- "$(cd "$(dirname "$worker")" && pwd)/sqlite-worker")
test ${#anchor} -eq 64
export NUDOX_BUILD_SQLITE_WORKER_ID=$anchor
cargo build --locked --offline -p backend-locald -p backend-cli -p backend-mcp
mkdir -p "$bundle"
for name in backend-locald backend-cli backend-mcp; do cp "$target/debug/$name" "$bundle/$name"; done
cp "$worker" "$bundle/backend-sqlite-worker"
chmod 755 "$bundle"/*
python3 "$here/sql_public_journey.py" --bundle "$bundle" --output "$output"
