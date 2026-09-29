#!/usr/bin/env bash
set -euo pipefail
umask 077

# This is a real-network integration journey. It always creates a new owner
# under .local/live-turso and never opens the user's normal Nudox state.
if [[ "${NUDOX_LIVE_TURSO_BACKUP:-0}" != "1" ]]; then
  printf '%s\n' 'Set NUDOX_LIVE_TURSO_BACKUP=1 to opt into public registry network access.' >&2
  exit 64
fi

workspace="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_root="$workspace/.local/live-turso/$run_id"
owner="$run_root/owner"
artifacts="$run_root/artifacts"
restore_owner="$artifacts/restore/owner-vacuum"
mkdir -m 700 -p "$owner" "$artifacts" "$artifacts/restore"
mkdir -m 700 -p "$owner/compiler"

bin_dir="${NUDOX_LIVE_BIN_DIR:-$workspace/target/debug}"
locald="${NUDOX_LIVE_LOCALD_BIN:-$bin_dir/backend-locald}"
cli="${NUDOX_LIVE_CLI_BIN:-$bin_dir/backend-cli}"
turso="${NUDOX_TURSO_SQLITE_BIN:-$(command -v tursodb || true)}"
if [[ -z "$turso" ]]; then
  printf '%s\n' 'Turso CLI is required; put tursodb on PATH or set NUDOX_TURSO_SQLITE_BIN.' >&2
  exit 69
fi
for executable in "$locald" "$cli" "$turso"; do
  if [[ -z "$executable" || ! -x "$executable" ]]; then
    printf 'Required executable is missing or not executable: %s\n' "$executable" >&2
    exit 69
  fi
done

# The journey owns the exact daemon PID that it later stops before restoring.
# A CLI readiness probe must never compose a replacement daemon while the
# explicit owner is still starting (or after it fails). A nonexistent override
# disables that fallback without affecting attachment to the live socket.
export BACKEND_LOCALD_BIN="$run_root/no-autostart-locald"

# The pinned Nix shell puts its compiler wrappers on PATH but does not export
# Nudox's toolchain variables. Pin those variables to the selected executables
# explicitly so a child Cargo invocation sees the same toolchain.
rustc_path="${NUDOX_RUSTC:-$(command -v rustc || true)}"
cargo_path="${NUDOX_CARGO:-$(command -v cargo || true)}"
for tool_path in "$rustc_path" "$cargo_path"; do
  if [[ -z "$tool_path" || "$tool_path" != /* || ! -x "$tool_path" ]]; then
    printf 'Could not resolve an absolute executable toolchain path (%s); run inside nix shell .#luna-tools or set NUDOX_RUSTC/NUDOX_CARGO explicitly.\n' \
        "$tool_path" >&2
    exit 64
  fi
done
rust_sysroot="${NUDOX_RUST_SYSROOT:-}"
if [[ -z "$rust_sysroot" ]]; then
  if ! rust_sysroot="$("$rustc_path" --print sysroot 2>/dev/null)"; then
    printf 'Could not query the selected rustc sysroot; set NUDOX_RUST_SYSROOT explicitly.\n' >&2
    exit 64
  fi
fi
if [[ "$rust_sysroot" != /* || ! -d "$rust_sysroot" ]]; then
  printf 'NUDOX_RUST_SYSROOT is not an existing absolute directory: %s\n' "$rust_sysroot" >&2
  exit 64
fi
export NUDOX_RUSTC="$rustc_path"
export NUDOX_CARGO="$cargo_path"
export NUDOX_RUST_SYSROOT="$rust_sysroot"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export NUDOX_CARGO_HOME="${NUDOX_CARGO_HOME:-$CARGO_HOME}"
export NUDOX_CARGO_ROOT="${NUDOX_CARGO_ROOT:-$CARGO_HOME/registry/src}"

pages="${NUDOX_LIVE_DISCOVERY_MAX_PAGES:-4}"
serde_purl='pkg:cargo/serde@1.0.228'
serde_dependent_purl="${NUDOX_LIVE_SERDE_DEPENDENT_PURL:-pkg:cargo/serde_json@1.0.145}"
case "$serde_dependent_purl" in
  pkg:cargo/serde_json@1.0.145)
    serde_dependent_name='serde_json'
    serde_dependent_version='1.0.145'
    serde_dependent_slug='serde-json'
    ;;
  pkg:cargo/serde_bytes@0.11.19)
    serde_dependent_name='serde_bytes'
    serde_dependent_version='0.11.19'
    serde_dependent_slug='serde-bytes'
    ;;
  *)
    printf 'Unsupported pinned serde dependent: %s\n' "$serde_dependent_purl" >&2
    printf '%s\n' 'Supported pins: pkg:cargo/serde_json@1.0.145 and pkg:cargo/serde_bytes@0.11.19.' >&2
    exit 64
    ;;
esac
turso_flags=(--experimental-multiprocess-wal)
daemon_pid=''
endpoint=''

stop_daemon() {
  local pid="${1:-}"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

cleanup() {
  stop_daemon "$daemon_pid"
}
trap cleanup EXIT INT TERM

start_daemon() {
  local state="$1"
  local socket="$2"
  local log="$3"
  env CARGO_HOME="$CARGO_HOME" \
      NUDOX_CARGO_HOME="$NUDOX_CARGO_HOME" \
      NUDOX_CARGO_ROOT="$NUDOX_CARGO_ROOT" \
      NUDOX_RUSTC="$NUDOX_RUSTC" \
      NUDOX_CARGO="$NUDOX_CARGO" \
      NUDOX_RUST_SYSROOT="$NUDOX_RUST_SYSROOT" \
      "$locald" --workspace "$state" --endpoint "$socket" \
      --profile builtin --registry-discovery-max-pages "$pages" --idle-timeout-ms 0 \
      >"$log" 2>&1 &
  daemon_pid=$!
  endpoint="$socket"
  printf '%s\n' "$daemon_pid" >"$log.pid"
}

wait_ready() {
  local state="$1"
  local socket="$2"
  local output="$3"
  local log="$4"
  local attempt
  for ((attempt = 0; attempt < 90; attempt++)); do
    if ! kill -0 "$daemon_pid" 2>/dev/null; then
      cat "$log" >&2 || true
      return 1
    fi
    if [[ -S "$socket" ]] && "$cli" --workspace "$state" --endpoint "$socket" --json health >"$output" 2>&1; then
      return 0
    fi
    sleep 1
  done
  cat "$output" >&2 || true
  return 1
}

sql_list() {
  local database="$1"
  local sql="$2"
  "$turso" -m list "${turso_flags[@]}" "$database" "$sql"
}

projection_metadata() {
  sql_list "$1" "SELECT schema_version||'|'||hex(root)||'|'||row_count||'|'||hex(row_digest) FROM backend_projection_meta; SELECT edge_count||'|'||hex(root)||'|'||hex(facts_witness) FROM backend_projection_package_graph_meta; SELECT count(*) FROM backend_projection_rows; SELECT count(*) FROM backend_projection_package_edges; SELECT count(*) FROM backend_projection_package_edges WHERE source='$serde_purl'; SELECT count(*) FROM backend_projection_package_edges WHERE source='$serde_dependent_purl'; SELECT count(*) FROM backend_projection_package_edges WHERE target_name='serde';"
}

index_authority_metadata() {
  sql_list "$1" "SELECT schema_version FROM backend_index_authority_meta; SELECT count(*) FROM backend_index_authority_attempts; SELECT count(*) FROM backend_index_authority_observations; SELECT count(*) FROM backend_index_authority_scopes; SELECT count(*) FROM backend_index_authority_projection_watermarks;"
}

index_authority_dump() {
  "$turso" "${turso_flags[@]}" "$1" .dump
}

index_authority_schema_objects() {
  sql_list "$1" "SELECT type||'|'||name||'|'||tbl_name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name;"
}

# VACUUM INTO may rewrite equivalent CREATE TABLE formatting in .dump. Compare
# the canonical multiset of row inserts and the schema object identities
# separately, rather than treating textual SQL whitespace as database drift.
index_authority_rows_sha() {
  LC_ALL=C grep '^INSERT INTO ' "$1" | LC_ALL=C sort | shasum -a 256 | awk '{print $1}'
}

# Compare edge contents as well as graph metadata. The facts witness is the
# logical authority, while this digest makes accidental loss or row mutation
# visible in the cold restore fixture itself.
package_graph_rows_sha() {
  sql_list "$1" "SELECT hex(edge_id)||'|'||source||'|'||source_authority_kind||'|'||hex(source_authority_id)||'|'||target_ecosystem||'|'||target_name||'|'||requirement||'|'||coalesce(resolved,'')||'|'||scope||'|'||optional||'|'||authority||'|'||hex(frontier)||'|'||hex(provenance)||'|'||hex(facts_version) FROM backend_projection_package_edges ORDER BY edge_id;" \
    | LC_ALL=C sort | shasum -a 256 | awk '{print $1}'
}

assert_projection_counts() {
  local database="$1"
  local counts rows_expected rows_actual edges_expected edges_actual
  counts="$(sql_list "$database" "SELECT row_count||'|'||(SELECT count(*) FROM backend_projection_rows)||'|'||(SELECT edge_count FROM backend_projection_package_graph_meta)||'|'||(SELECT count(*) FROM backend_projection_package_edges) FROM backend_projection_meta;")"
  IFS='|' read -r rows_expected rows_actual edges_expected edges_actual <<<"$counts"
  if [[ ! "$rows_expected" =~ ^[0-9]+$ || ! "$rows_actual" =~ ^[0-9]+$ \
      || ! "$edges_expected" =~ ^[0-9]+$ || ! "$edges_actual" =~ ^[0-9]+$ \
      || "$rows_expected" != "$rows_actual" || "$edges_expected" != "$edges_actual" ]]; then
    printf 'Projection metadata/count mismatch in %s: %s\n' "$database" "$counts" >&2
    return 1
  fi
  printf '%s\n' "$counts"
}

assert_projection_fts_index_absent() {
  local database="$1"
  local index_count
  index_count="$(sql_list "$database" "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='backend_projection_rows_fts';")"
  if [[ "$index_count" != "0" ]]; then
    printf 'Expected backend_projection_rows_fts to be absent in %s, got %s.\n' \
        "$database" "$index_count" >&2
    return 1
  fi
  printf '%s\n' "$index_count"
}

assert_integrity_ok() {
  local database="$1"
  local output_file="$2"
  local result
  if ! sql_list "$database" "PRAGMA integrity_check;" >"$output_file" 2>&1; then
    printf 'PRAGMA integrity_check failed to run for %s; see %s.\n' \
        "$database" "$output_file" >&2
    return 1
  fi
  result="$(tr -d '\r' <"$output_file" | sed '/^[[:space:]]*$/d')"
  if [[ "$result" != "ok" ]]; then
    printf 'Expected PRAGMA integrity_check=ok for %s, got: %s\n' \
        "$database" "$result" >&2
    return 1
  fi
  printf '%s\n' "$result"
}

capture_add() {
  local purl="$1"
  local name="$2"
  local began ended result
  began="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if "$cli" --workspace "$owner" --endpoint "$endpoint" --json add "$purl" \
      >"$artifacts/add-$name.json" 2>&1; then
    result=0
  else
    result=$?
  fi
  ended="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '%s\t%s\t%s\t%s\n' "$name" "$purl" "$began/$ended" "$result" \
      >>"$artifacts/add-status.tsv"
  if ((result != 0)); then
    printf 'Pinned package add failed (exit %s); evidence is in %s.\n' \
        "$result" "$artifacts/add-$name.json" >&2
    return "$result"
  fi
}

capture_physical_backup() {
  local database="$1"
  local snapshot="$2"
  local label="$3"
  local began ended
  began="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '%s\n' "$began" >"$artifacts/$label-vacuum-started-utc.txt"
  "$turso" "${turso_flags[@]}" "$database" "VACUUM INTO '$snapshot'"
  ended="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '%s\n' "$ended" >"$artifacts/$label-vacuum-finished-utc.txt"
}

remove_copied_sqlite_sidecars() {
  local state="$1"
  local database suffix candidate
  for database in projection.turso index-authority.turso; do
    for suffix in -wal -shm -tshm; do
      candidate="$state/$database$suffix"
      if [[ -e "$candidate" ]]; then
        unlink "$candidate"
      fi
    done
  done
}

started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'name\tpurl\tstarted/finished UTC\texit\n' >"$artifacts/add-status.tsv"
printf '%s\n' "live ingest started: $started_utc" >"$artifacts/run.log"
printf '%s\n' "workspace=$workspace" "owner=$owner" "locald=$locald" "cli=$cli" \
  "turso=$turso" "profile=builtin" \
  "registry_discovery_max_pages=$pages" "CARGO_HOME=$CARGO_HOME" \
  "NUDOX_CARGO_HOME=$NUDOX_CARGO_HOME" "NUDOX_CARGO_ROOT=$NUDOX_CARGO_ROOT" \
  "NUDOX_RUSTC=$NUDOX_RUSTC" "NUDOX_CARGO=$NUDOX_CARGO" \
  "NUDOX_RUST_SYSROOT=$NUDOX_RUST_SYSROOT" "serde_dependent=$serde_dependent_purl" \
  >"$artifacts/environment.txt"
"$cli" --version >"$artifacts/cli-version.txt" 2>&1 || true
"$turso" --version >"$artifacts/turso-version.txt" 2>&1 || true
shasum -a 256 "$locald" >"$artifacts/locald-binary-sha256.txt"
shasum -a 256 "$cli" >"$artifacts/cli-binary-sha256.txt"

owner_socket="/tmp/nudox-live-turso-$run_id.sock"
start_daemon "$owner" "$owner_socket" "$artifacts/locald.log"
wait_ready "$owner" "$owner_socket" "$artifacts/health-before.json" "$artifacts/locald.log"

# These are public, exact pins. An add failure is retained as evidence and
# fails this journey: a cold owner must never hide a partially ingested source.
capture_add "$serde_purl" serde
capture_add "$serde_dependent_purl" "$serde_dependent_slug"

# A graph read refreshes the resident catalog and synchronizes its checked
# facts into Turso. Acquisition is asynchronous, so keep asking the real
# dependency surface until the persisted graph contains both pinned packages.
# The durable witness, rather than elapsed time alone, gates backup.
settled=0
for ((attempt = 0; attempt < 90; attempt++)); do
  "$cli" --workspace "$owner" --endpoint "$owner_socket" --json \
      dependencies "$serde_dependent_purl" \
      >"$artifacts/dependencies-settle.json" 2>&1 || true
  serde_edges="$(sql_list "$owner/projection.turso" "SELECT count(*) FROM backend_projection_package_edges WHERE source='$serde_purl';")"
  dependent_edges="$(sql_list "$owner/projection.turso" "SELECT count(*) FROM backend_projection_package_edges WHERE source='$serde_dependent_purl';")"
  reverse_edges="$(sql_list "$owner/projection.turso" "SELECT count(*) FROM backend_projection_package_edges WHERE target_name='serde';")"
  if [[ "$serde_edges" =~ ^[0-9]+$ && "$dependent_edges" =~ ^[0-9]+$ && "$reverse_edges" =~ ^[0-9]+$ ]] \
      && ((serde_edges > 0 && dependent_edges > 0 && reverse_edges > 0)); then
    settled=1
    break
  fi
  sleep 1
done
if ((settled == 0)); then
  printf 'Graph projection did not settle: serde=%s %s=%s reverse=%s\n' \
      "$serde_edges" "$serde_dependent_name" "$dependent_edges" "$reverse_edges" >&2
  printf '%s\n' "${serde_edges:-unknown}" "${dependent_edges:-unknown}" "${reverse_edges:-unknown}" \
      >"$artifacts/graph-settle-failure.txt"
  exit 1
fi

# These warm queries are part of the ingest gate. A cold query failure is only
# meaningful when the same selected owner served the result before backup.
"$cli" --workspace "$owner" --endpoint "$owner_socket" --json --limit 20 search serde \
    >"$artifacts/search-serde.json" 2>&1
"$cli" --workspace "$owner" --endpoint "$owner_socket" --json --limit 10 index-search serde \
    >"$artifacts/index-search-serde.json" 2>&1
"$cli" --workspace "$owner" --endpoint "$owner_socket" --json dependencies "$serde_dependent_purl" \
    >"$artifacts/dependencies-$serde_dependent_slug.json" 2>&1
"$cli" --workspace "$owner" --endpoint "$owner_socket" --json dependents "$serde_purl" \
    >"$artifacts/dependents-serde.json" 2>&1
grep -Fq "$serde_purl" "$artifacts/search-serde.json"
grep -Fq "$serde_purl" "$artifacts/index-search-serde.json"
grep -Fq 'serde ^1.0.220' "$artifacts/dependencies-$serde_dependent_slug.json"
grep -Fq "$serde_dependent_name $serde_dependent_version" "$artifacts/dependents-serde.json"

projection_snapshot="$artifacts/projection.turso.vacuum-into"
authority_snapshot="$artifacts/index-authority.turso.vacuum-into"
projection_metadata "$owner/projection.turso" >"$artifacts/projection-live-before-vacuum.txt"
index_authority_metadata "$owner/index-authority.turso" >"$artifacts/index-authority-live-before-vacuum.txt"
index_authority_dump "$owner/index-authority.turso" >"$artifacts/index-authority-live-before-vacuum.dump.sql"
index_authority_schema_objects "$owner/index-authority.turso" >"$artifacts/index-authority-live-before-vacuum.objects.txt"
capture_physical_backup "$owner/projection.turso" "$projection_snapshot" projection
capture_physical_backup "$owner/index-authority.turso" "$authority_snapshot" index-authority

# The VACUUM INTO outputs above are the online backups. Stop the writer before
# comparing all related owner state so that a restore cannot combine mismatched
# journals and databases.
stop_daemon "$daemon_pid"
daemon_pid=''
projection_metadata "$owner/projection.turso" >"$artifacts/projection-live-after-vacuum.txt"
projection_metadata "$projection_snapshot" >"$artifacts/projection-snapshot-metadata.txt"
index_authority_metadata "$owner/index-authority.turso" >"$artifacts/index-authority-live-after-vacuum.txt"
index_authority_metadata "$authority_snapshot" >"$artifacts/index-authority-snapshot-metadata.txt"
index_authority_dump "$owner/index-authority.turso" >"$artifacts/index-authority-live-after-vacuum.dump.sql"
index_authority_dump "$authority_snapshot" >"$artifacts/index-authority-snapshot.dump.sql"
index_authority_schema_objects "$owner/index-authority.turso" >"$artifacts/index-authority-live-after-vacuum.objects.txt"
index_authority_schema_objects "$authority_snapshot" >"$artifacts/index-authority-snapshot.objects.txt"
for metadata_file in projection-live-after-vacuum.txt projection-snapshot-metadata.txt; do
  if [[ "$(cat "$artifacts/projection-live-before-vacuum.txt")" != "$(cat "$artifacts/$metadata_file")" ]]; then
    printf 'Projection root/count/digest/facts witness drifted by %s; refusing mismatched owner restore.\n' \
        "$metadata_file" >&2
    exit 1
  fi
done
for metadata_file in index-authority-live-after-vacuum.txt index-authority-snapshot-metadata.txt; do
  if [[ "$(cat "$artifacts/index-authority-live-before-vacuum.txt")" != "$(cat "$artifacts/$metadata_file")" ]]; then
    printf 'Index-authority metadata drifted by %s; refusing mismatched owner restore.\n' \
        "$metadata_file" >&2
    exit 1
  fi
done
authority_before_sha="$(index_authority_rows_sha "$artifacts/index-authority-live-before-vacuum.dump.sql")"
authority_after_sha="$(index_authority_rows_sha "$artifacts/index-authority-live-after-vacuum.dump.sql")"
authority_snapshot_sha="$(index_authority_rows_sha "$artifacts/index-authority-snapshot.dump.sql")"
printf 'before=%s\nafter=%s\nsnapshot=%s\n' "$authority_before_sha" "$authority_after_sha" \
    "$authority_snapshot_sha" >"$artifacts/index-authority-dump-sha256.txt"
if [[ "$authority_before_sha" != "$authority_after_sha" || "$authority_before_sha" != "$authority_snapshot_sha" ]]; then
  printf '%s\n' 'Index-authority rows drifted across VACUUM INTO; refusing mismatched owner restore.' >&2
  exit 1
fi
if ! cmp -s "$artifacts/index-authority-live-before-vacuum.objects.txt" "$artifacts/index-authority-live-after-vacuum.objects.txt" \
    || ! cmp -s "$artifacts/index-authority-live-before-vacuum.objects.txt" "$artifacts/index-authority-snapshot.objects.txt"; then
  printf '%s\n' 'Index-authority schema objects drifted across VACUUM INTO; refusing mismatched owner restore.' >&2
  exit 1
fi
assert_projection_counts "$projection_snapshot" >"$artifacts/projection-snapshot-count-check.txt"
projection_schema="$(sql_list "$projection_snapshot" "SELECT schema_version FROM backend_projection_meta;")"
if [[ "$projection_schema" != "8" ]]; then
  printf 'Expected projection schema 8, got %s; run with schema8 locald/CLI binaries.\n' \
      "$projection_schema" >&2
  exit 1
fi
assert_projection_fts_index_absent "$owner/projection.turso" >"$artifacts/projection-live-fts-index-count.txt"
assert_projection_fts_index_absent "$projection_snapshot" >"$artifacts/projection-snapshot-fts-index-count.txt"
assert_integrity_ok "$owner/projection.turso" "$artifacts/projection-live-integrity-check.txt" >/dev/null
assert_integrity_ok "$projection_snapshot" "$artifacts/projection-integrity-check.txt" >/dev/null
assert_integrity_ok "$owner/index-authority.turso" "$artifacts/index-authority-live-integrity-check.txt" >/dev/null
assert_integrity_ok "$authority_snapshot" "$artifacts/index-authority-integrity-check.txt" >/dev/null
projection_sha="$(shasum -a 256 "$projection_snapshot" | awk '{print $1}')"
authority_sha="$(shasum -a 256 "$authority_snapshot" | awk '{print $1}')"
projection_bytes="$(wc -c <"$projection_snapshot" | tr -d ' ')"
authority_bytes="$(wc -c <"$authority_snapshot" | tr -d ' ')"
printf 'projection\t%s\t%s\t%s\n' "$projection_sha" "$projection_bytes" \
    "$(cat "$artifacts/projection-vacuum-finished-utc.txt")" >"$artifacts/backup-files.tsv"
printf 'index-authority\t%s\t%s\t%s\n' "$authority_sha" "$authority_bytes" \
    "$(cat "$artifacts/index-authority-vacuum-finished-utc.txt")" \
    >>"$artifacts/backup-files.tsv"

# The main Turso-created files are the backup. Sidecars created by later reads
# of those files are never copied into the restore workspace.
mkdir -m 700 "$restore_owner"
cp -R "$owner/." "$restore_owner/"
cp "$projection_snapshot" "$restore_owner/projection.turso"
cp "$authority_snapshot" "$restore_owner/index-authority.turso"
remove_copied_sqlite_sidecars "$restore_owner"

restore_projection_sha="$(shasum -a 256 "$restore_owner/projection.turso" | awk '{print $1}')"
restore_authority_sha="$(shasum -a 256 "$restore_owner/index-authority.turso" | awk '{print $1}')"
if [[ "$restore_projection_sha" != "$projection_sha" || "$restore_authority_sha" != "$authority_sha" ]]; then
  printf '%s\n' 'Restored DB main-file hashes do not match the physical backup files.' >&2
  exit 1
fi
if find "$restore_owner" -maxdepth 1 \( -name 'projection.turso-wal' -o -name 'projection.turso-shm' \
    -o -name 'projection.turso-tshm' -o -name 'index-authority.turso-wal' \
    -o -name 'index-authority.turso-shm' -o -name 'index-authority.turso-tshm' \) | grep -q .; then
  printf '%s\n' 'Restore workspace still has ambient SQLite sidecars before cold open.' >&2
  exit 1
fi
printf 'projection-main-only-sha256=%s\nindex-authority-main-only-sha256=%s\n' \
    "$restore_projection_sha" "$restore_authority_sha" >"$artifacts/restore-input-proof.txt"

restore_socket="/tmp/nudox-live-turso-restore-$run_id.sock"
start_daemon "$restore_owner" "$restore_socket" "$artifacts/restore/locald.log"
wait_ready "$restore_owner" "$restore_socket" "$artifacts/restore/health.json" "$artifacts/restore/locald.log"
snapshot_root="$(sql_list "$projection_snapshot" "SELECT hex(root) FROM backend_projection_meta;")"
snapshot_rows="$(sql_list "$projection_snapshot" "SELECT count(*) FROM backend_projection_rows;")"
restored_root="$(sql_list "$restore_owner/projection.turso" "SELECT hex(root) FROM backend_projection_meta;")"
restored_rows="$(sql_list "$restore_owner/projection.turso" "SELECT count(*) FROM backend_projection_rows;")"
snapshot_graph="$(sql_list "$projection_snapshot" "SELECT edge_count||'|'||hex(root)||'|'||hex(facts_witness) FROM backend_projection_package_graph_meta;")"
restored_graph="$(sql_list "$restore_owner/projection.turso" "SELECT edge_count||'|'||hex(root)||'|'||hex(facts_witness) FROM backend_projection_package_graph_meta;")"
projection_metadata "$restore_owner/projection.turso" >"$artifacts/restore/projection-metadata.txt"
if ! cmp -s "$artifacts/projection-snapshot-metadata.txt" "$artifacts/restore/projection-metadata.txt"; then
  printf '%s\n' 'Cold restore changed projection roots, row digest, edge witness, or table counts.' >&2
  exit 1
fi
snapshot_graph_rows_sha="$(package_graph_rows_sha "$projection_snapshot")"
restored_graph_rows_sha="$(package_graph_rows_sha "$restore_owner/projection.turso")"
printf 'snapshot=%s\nrestored=%s\n' "$snapshot_graph_rows_sha" "$restored_graph_rows_sha" \
    >"$artifacts/restore/package-graph-rows-sha256.txt"
if [[ "$restored_graph_rows_sha" != "$snapshot_graph_rows_sha" ]]; then
  printf '%s\n' 'Cold restore changed package graph edge rows.' >&2
  exit 1
fi
snapshot_root_lc="$(printf '%s' "$snapshot_root" | tr '[:upper:]' '[:lower:]')"
restored_root_lc="$(printf '%s' "$restored_root" | tr '[:upper:]' '[:lower:]')"
if [[ "$restored_root_lc" != "$snapshot_root_lc" || "$restored_rows" != "$snapshot_rows" \
    || "$restored_graph" != "$snapshot_graph" ]]; then
  printf '%s\n' 'Cold restore differs from physical snapshot root, row count, graph root, edge count, or facts witness.' >&2
  exit 1
fi
printf 'snapshot-root=%s\nsnapshot-rows=%s\nsnapshot-graph=%s\nrestored-root=%s\nrestored-rows=%s\nrestored-graph=%s\n' \
    "$snapshot_root" "$snapshot_rows" "$snapshot_graph" "$restored_root" "$restored_rows" \
    "$restored_graph" \
    >"$artifacts/restore/root-count-proof.txt"
index_authority_metadata "$restore_owner/index-authority.turso" \
    >"$artifacts/restore/index-authority-metadata.txt"
if [[ "$(cat "$artifacts/index-authority-snapshot-metadata.txt")" \
    != "$(cat "$artifacts/restore/index-authority-metadata.txt")" ]]; then
  printf '%s\n' 'Cold restore differs from index-authority snapshot schema or row counts.' >&2
  exit 1
fi
if ! grep -Fq "\"revision\":\"$snapshot_root_lc\"" \
    "$artifacts/restore/health.json"; then
  printf '%s\n' 'Cold-restored locald did not select the physical snapshot root.' >&2
  exit 1
fi
if ! grep -Fq "\"rows\":$snapshot_rows" "$artifacts/restore/health.json"; then
  printf '%s\n' 'Cold-restored locald row count did not match the physical snapshot.' >&2
  exit 1
fi

"$cli" --workspace "$restore_owner" --endpoint "$restore_socket" --json --limit 20 search serde \
    >"$artifacts/restore/search-serde.json" 2>&1
"$cli" --workspace "$restore_owner" --endpoint "$restore_socket" --json --limit 10 index-search serde \
    >"$artifacts/restore/index-search-serde.json" 2>&1
"$cli" --workspace "$restore_owner" --endpoint "$restore_socket" --json dependencies "$serde_dependent_purl" \
    >"$artifacts/restore/dependencies-$serde_dependent_slug.json" 2>&1
"$cli" --workspace "$restore_owner" --endpoint "$restore_socket" --json dependents "$serde_purl" \
    >"$artifacts/restore/dependents-serde.json" 2>&1
grep -Fq "$serde_purl" "$artifacts/restore/search-serde.json"
grep -Fq "$serde_purl" "$artifacts/restore/index-search-serde.json"
grep -Fq 'serde ^1.0.220' "$artifacts/restore/dependencies-$serde_dependent_slug.json"
grep -Fq "$serde_dependent_name $serde_dependent_version" "$artifacts/restore/dependents-serde.json"

"$workspace/tests/journeys/measure-live-turso-queries.py" \
    --cli "$cli" --turso "$turso" --workspace "$restore_owner" \
    --endpoint "$restore_socket" --projection "$restore_owner/projection.turso" \
    --serde-purl "$serde_purl" --dependent-purl "$serde_dependent_purl" \
    --output "$artifacts/restore/query-benchmark.json"

finished_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '%s\n' "live ingest started: $started_utc" "journey finished: $finished_utc" \
  "serde dependent: $serde_dependent_purl" \
  "projection schema: $projection_schema" \
  "projection snapshot sha256: $projection_sha" "projection snapshot bytes: $projection_bytes" \
  "index-authority snapshot sha256: $authority_sha" "index-authority snapshot bytes: $authority_bytes" \
  "projection root/count/witness and graph rows: $artifacts/projection-snapshot-metadata.txt" \
  "owner stopped immediately after both physical VACUUM INTO calls and matching state was verified" \
  "restore was opened from the two main VACUUM INTO files only" \
  "index-authority logical dump hashes: $artifacts/index-authority-dump-sha256.txt" \
  "cold-restored query benchmark: $artifacts/restore/query-benchmark.json" \
  >>"$artifacts/run.log"
printf 'Live Turso backup journey passed. Artifacts: %s\n' "$artifacts"
