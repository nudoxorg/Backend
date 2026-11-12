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
"${RUSTC:-rustc}" -Vv
cargo build --locked --offline --manifest-path frontends/sqlite/Cargo.toml --bin sqlite-worker
mkdir "$output/bundle"
cp "$target/debug/sqlite-worker" "$output/bundle/backend-sqlite-worker"
strip "$output/bundle/backend-sqlite-worker"
anchor=$(cargo run --locked --offline -q -p backend-compile --example sql_worker_identity -- "$output/bundle/backend-sqlite-worker")
test ${#anchor} -eq 64
export NUDOX_BUILD_SQLITE_WORKER_ID=$anchor
export NUDOX_TEST_SQLITE_WORKER="$output/bundle/backend-sqlite-worker"
# Compile every focused test target and public binary first. Compilation is a
# prerequisite, so a compiler failure exits before independent behavior checks.
cargo test -p backend-library --lib --locked --offline --no-run
cargo test -p backend-present --lib --locked --offline --no-run
cargo test --manifest-path frontends/sqlite/Cargo.toml --test outline --locked --offline --no-run
cargo test -p backend-local-service --lib --locked --offline --no-run
cargo build --locked --offline -p backend-locald -p backend-cli -p backend-mcp
# Independent checks keep running after a behavioral failure so one regression
# does not hide the worker/no-worker and cold-reopen outcomes. Build failure
# still stops the run, and the final exit remains nonzero for any failed check.
failed=0
check() {
    local label=$1
    shift
    if "$@"; then
        printf 'CHECK PASS: %s\n' "$label"
    else
        local status=$?
        printf 'CHECK FAIL: %s (exit %s)\n' "$label" "$status" >&2
        failed=1
    fi
}
check status-wire cargo test -p backend-library --lib --locked --offline sql_analysis_progress_round_trips_and_rejects_empty_or_overflow -- --nocapture
check status-render cargo test -p backend-present --lib --locked --offline status_exposes_selected_sql_analysis_at_every_detail -- --nocapture
check native-upsert cargo test --manifest-path frontends/sqlite/Cargo.toml --test outline --locked --offline excluded_upsert_without_update_where_has_valid_native_outline -- --nocapture
for test in shared_upsert_syntax_does_not_select_postgres_or_poison_project_consensus sqlite_project_indexes_publishes_queries_reindexes_and_survives_cold_reopen unsupported_statement_and_unparseable_file_are_reported_as_gaps_never_as_empty flat_sql_upsert_and_renamed_objects_keep_precise_owner_edges; do
    check "$test" cargo test -p backend-local-service --lib --locked --offline "$test" -- --nocapture --test-threads=1
done
for name in backend-locald backend-cli backend-mcp; do
    cp "$target/debug/$name" "$output/bundle/$name"
done
sha256sum "$output/bundle/"*
check public-sql-only python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/sql-only" --sql-only
check public-python python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/python" --python
check public-flat python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/flat" --python --flat
check minimal-worker python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-worker"
check minimal-no-worker python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-no-worker" --without-worker
mkdir "$output/no-worker-bundle"
for name in backend-locald backend-cli backend-mcp; do
    cp "$output/bundle/$name" "$output/no-worker-bundle/$name"
done
check public-no-worker python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker" --python --expect-no-worker
check public-no-worker-sql-only python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker-sql-only" --sql-only --expect-no-worker
check public-no-worker-flat python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker-flat" --python --flat --expect-no-worker
exit "$failed"
