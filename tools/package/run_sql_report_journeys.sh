#!/usr/bin/env bash
# Development-mode SQL report regression: isolated owner/CLI/MCP, no accounts.
# Existing build outputs are reused; every worker identity is measured after strip.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cd "$repo"
target=${CARGO_TARGET_DIR:?set a private target directory}
output=${1:?provide a new output directory}
mkdir "$output"
output=$(cd "$output" && pwd -P)
target=$(cd "$target" && pwd -P)
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
cargo test -p backend-extension-trustfall --lib --locked --offline --no-run
cargo test --manifest-path frontends/sqlite/Cargo.toml --test outline --locked --offline --no-run
cargo test -p backend-local-service --lib --locked --offline --no-run
# The recorder pins this exact runner, source, toolchain and resulting private
# single-link copies. The worker identity is fixed before this build.
cargo_path=$(command -v cargo)
copy_path=$(command -v cp)
runner=$output/cargo-copy-runner
printf '#!/usr/bin/env bash\nset -euo pipefail\n%q "$@"\n' "$cargo_path" > "$runner"
for name in backend-locald backend-cli backend-mcp; do
    printf '%q %q %q\n' "$copy_path" "$target/debug/$name" "$output/bundle/$name" >> "$runner"
done
chmod 700 "$runner"
receipt=$output/build-receipt.json
worker_hash=$(sha256sum "$output/bundle/backend-sqlite-worker")
python3 "$repo/tests/journeys/scripts/record-runtime-build-manifest.py" capture \
    --source-checkout "$repo" --runner "$runner" --cargo "$cargo_path" \
    --rustc "$(command -v "${RUSTC:-rustc}")" \
    --artifact "backend-locald=$output/bundle/backend-locald" \
    --artifact "backend-cli=$output/bundle/backend-cli" \
    --artifact "backend-mcp=$output/bundle/backend-mcp" \
    --receipt-output "$receipt" --manifest-output "$output/build-manifest.json" \
    -- "$runner" build --locked --offline -p backend-locald -p backend-cli -p backend-mcp
test "$(sha256sum "$output/bundle/backend-sqlite-worker")" = "$worker_hash"
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
check sql-native-evidence cargo test -p backend-extension-trustfall --lib --locked --offline sql_native_evidence_binds_projection_and_host_basis -- --nocapture
check status-wire cargo test -p backend-library --lib --locked --offline sql_analysis_progress_round_trips_and_rejects_empty_or_overflow -- --nocapture
check status-render cargo test -p backend-present --lib --locked --offline status_exposes_selected_sql_analysis_at_every_detail -- --nocapture
check native-upsert cargo test --manifest-path frontends/sqlite/Cargo.toml --test outline --locked --offline excluded_upsert_without_update_where_has_valid_native_outline -- --nocapture
check alias-gap cargo test --manifest-path frontends/sqlite/Cargo.toml --test outline --locked --offline alias_shadowing_omission_is_partial_not_a_complete_negative -- --nocapture
run_listed_test() {
    local package=$1 filter=$2 list="$output/test-list-$2.txt"
    cargo test -p "$package" --lib --locked --offline "$filter" -- --list > "$list"
    cat "$list"
    grep -q ': test$' "$list" || return 1
    cargo test -p "$package" --lib --locked --offline "$filter" -- --nocapture --test-threads=1
}
for test in sql_stage_scope_rejects_missing_duplicate_and_cancelled_members opaque_peer_never_turns_a_possible_column_into_an_exact_binding omitted_ambiguous_candidates_are_disclosed creating_fact_identity_separates_epochs statement_identity_does_not_depend_on_line_availability shared_upsert_syntax_does_not_select_postgres_or_poison_project_consensus sqlite_project_indexes_publishes_queries_reindexes_and_survives_cold_reopen unsupported_statement_and_unparseable_file_are_reported_as_gaps_never_as_empty flat_sql_upsert_and_renamed_objects_keep_precise_owner_edges sql_impact_distinguishes_drop_recreate_epochs many_sql_facts_publish_discover_and_reopen_with_bounded_catalog bounded_catalog_tests sql_catalog::snapshot_tests builtin::view_build::sql::tests row_admission failed_latest_sql_attempt_leaves_names_gapped; do
    check "$test" run_listed_test backend-local-service "$test"
done
check corpus-admission run_listed_test backend-extension-trustfall corpus_admission
check sql-query-evidence run_listed_test backend-extension-trustfall sql_native_evidence_binds_projection_and_host_basis
sha256sum "$output/bundle/"*
common=(--build-receipt "$receipt" --source-checkout "$repo")
check public-python python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/python" "${common[@]}" --python
check public-flat python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/flat" "${common[@]}" --python --flat
check public-sql-only python3 "$here/sql_public_journey.py" --bundle "$output/bundle" --output "$output/sql-only" "${common[@]}" --sql-only
check minimal-worker python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-worker" "${common[@]}"
check minimal-no-worker python3 "$here/sql_minimal_publication.py" --bundle "$output/bundle" --output "$output/minimal-no-worker" "${common[@]}" --without-worker
mkdir "$output/no-worker-bundle"
for name in backend-locald backend-cli backend-mcp; do
    cp "$output/bundle/$name" "$output/no-worker-bundle/$name"
done
check public-no-worker python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker" "${common[@]}" --python --expect-no-worker
check public-no-worker-flat python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker-flat" "${common[@]}" --python --flat --expect-no-worker
check public-no-worker-sql-only python3 "$here/sql_public_journey.py" --bundle "$output/no-worker-bundle" --output "$output/no-worker-sql-only" "${common[@]}" --sql-only --expect-no-worker
if [[ $(sha256sum "$output/bundle/backend-sqlite-worker") != "$worker_hash" ]]; then
    printf 'CHECK FAIL: SQLite worker changed during journeys\n' >&2
    failed=1
fi
exit "$failed"
