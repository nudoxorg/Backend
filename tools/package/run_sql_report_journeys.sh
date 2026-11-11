#!/usr/bin/env bash
# Development-mode SQL report regression: isolated owner/CLI/MCP, no accounts.
# Existing build outputs are reused; every worker identity is measured after strip.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cd "$repo"
target=${CARGO_TARGET_DIR:?set a private target directory}
output=${1:?provide a new output directory}
mkdir -p "$output"
git rev-parse HEAD
rustc -Vv
cargo build --locked --offline --manifest-path frontends/sqlite/Cargo.toml --bin sqlite-worker
mkdir "$output/bundle"
cp "$target/debug/sqlite-worker" "$output/bundle/backend-sqlite-worker"
strip "$output/bundle/backend-sqlite-worker"
anchor=$(cargo run --locked --offline -q -p backend-compile --example sql_worker_identity -- "$output/bundle/backend-sqlite-worker")
test ${#anchor} -eq 64
export NUDOX_BUILD_SQLITE_WORKER_ID=$anchor
export NUDOX_TEST_SQLITE_WORKER="$output/bundle/backend-sqlite-worker"
cargo test -p backend-library --locked --offline sql_analysis_progress_round_trips_and_rejects_empty_or_overflow -- --nocapture
cargo test -p backend-present --locked --offline status_exposes_selected_sql_analysis_at_every_detail -- --nocapture
cargo test --manifest-path frontends/sqlite/Cargo.toml --locked --offline excluded_upsert_without_update_where_has_valid_native_outline -- --nocapture
for test in shared_upsert_syntax_does_not_select_postgres_or_poison_project_consensus sqlite_project_indexes_publishes_queries_reindexes_and_survives_cold_reopen unsupported_statement_and_unparseable_file_are_reported_as_gaps_never_as_empty flat_sql_upsert_and_renamed_objects_keep_precise_owner_edges; do
    cargo test -p backend-local-service --lib --locked --offline "$test" -- --nocapture --test-threads=1
done
cargo build --locked --offline -p backend-locald -p backend-cli -p backend-mcp
for name in backend-locald backend-cli backend-mcp; do
    cp "$target/debug/$name" "$output/bundle/$name"
done
sha256sum "$output/bundle/"*
python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/python" --python
python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/flat" --python --flat
python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-worker"
python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-no-worker" --without-worker
