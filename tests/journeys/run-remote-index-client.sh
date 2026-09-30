#!/usr/bin/env bash
set -euo pipefail

backend_cli=${BACKEND_CLI:-backend-cli}
locald_bin=${BACKEND_LOCALD_BIN:-backend-locald}
command -v "$backend_cli" >/dev/null 2>&1 || {
  printf '%s\n' "set BACKEND_CLI to the built backend CLI" >&2
  exit 69
}
command -v "$locald_bin" >/dev/null 2>&1 || {
  printf '%s\n' "set BACKEND_LOCALD_BIN to the built backend-locald binary" >&2
  exit 69
}

rustc_path=${NUDOX_RUSTC:-$(command -v rustc || true)}
cargo_path=${NUDOX_CARGO:-$(command -v cargo || true)}
cargo_home=${NUDOX_CARGO_HOME:-${CARGO_HOME:-${HOME:-}/.cargo}}
if [[ -z "$rustc_path" || ! -x "$rustc_path" ]]; then
  printf "%s\n" "set NUDOX_RUSTC to an executable Rust compiler for the indexed fixture" >&2
  exit 69
fi
if [[ -z "$cargo_path" || ! -x "$cargo_path" ]]; then
  printf "%s\n" "set NUDOX_CARGO to an executable Cargo for the indexed fixture" >&2
  exit 69
fi
if [[ ! -d "$cargo_home" ]]; then
  printf "%s\n" "set NUDOX_CARGO_HOME to an existing Cargo home for the indexed fixture" >&2
  exit 69
fi
rustc_path=$(cd "$(dirname "$rustc_path")" && pwd -P)/$(basename "$rustc_path")
cargo_path=$(cd "$(dirname "$cargo_path")" && pwd -P)/$(basename "$cargo_path")
cargo_home=$(cd "$cargo_home" && pwd -P)

root=$(mktemp -d "${REMOTE_INDEX_TMPDIR:-/tmp}/backend-remote-index.XXXXXX")
root=$(cd "$root" && pwd -P)
owner_data=$root/owner
client_data=$root/client
fixture=$root/fixture
endpoint=$owner_data/locald.sock
client_key=$client_data/remote-index-client.v1
capability_owner=$owner_data/product-read.cap
capability_client=$client_data/product-read.cap
semantic_capability_owner=$owner_data/semantic-read.cap
semantic_capability_client=$client_data/semantic-read.cap
semantic_store=$client_data/semantic-store
marker=remote_index_journey_marker
locald_pid=

cleanup() {
  if [[ -n "$locald_pid" ]]; then
    kill "$locald_pid" 2>/dev/null || true
    wait "$locald_pid" 2>/dev/null || true
  fi
  rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$owner_data" "$client_data" "$fixture/src"
chmod 700 "$owner_data" "$client_data"
cat >"$fixture/Cargo.toml" <<'EOF'
[package]
name = "remote-index-fixture"
version = "0.1.0"
edition = "2024"
EOF
cat >"$fixture/src/lib.rs" <<'EOF'
/// Stable text used to verify a real indexed product query across Iroh.
pub fn remote_index_journey_marker() -> u32 {
    7
}
EOF
shasum -a 256 "$fixture/Cargo.toml" "$fixture/src/lib.rs"

port=${REMOTE_INDEX_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}
address=127.0.0.1:$port
rss_limit_kb=${REMOTE_INDEX_RSS_LIMIT_KB:-2097152}

"$backend_cli" --workspace "$owner_data" --project "$fixture" \
  cluster owner init --bind "$address" --advertise "$address" >/dev/null

start_locald() {
  (
    cd "$fixture"
    NUDOX_RUSTC="$rustc_path" NUDOX_CARGO="$cargo_path" \
      NUDOX_CARGO_HOME="$cargo_home" exec "$locald_bin" \
      --workspace "$owner_data" --endpoint "$endpoint" \
      --idle-timeout-ms 0
  ) >"$root/locald.log" 2>&1 &
  locald_pid=$!
  for _ in $(seq 1 200); do
    if [[ -S "$endpoint" ]]; then
      if "$backend_cli" --workspace "$owner_data" --project "$fixture" \
        --endpoint "$endpoint" health >/dev/null 2>&1; then
        return 0
      fi
    fi
    if ! kill -0 "$locald_pid" 2>/dev/null; then
      printf '%s\n' "backend-locald exited before becoming ready" >&2
      tail -n 80 "$root/locald.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  printf '%s\n' "backend-locald did not become ready" >&2
  exit 1
}

start_locald
if ! "$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" --format json index_start "$fixture" \
  >"$root/index-start.json" 2>"$root/index-start.err"; then
  printf '%s\n' "typed index_start failed before a readiness ticket was issued" >&2
  cat "$root/index-start.err" >&2
  tail -n 80 "$root/locald.log" >&2
  exit 1
fi
index_ticket=$(python3 - "$root/index-start.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    answer = json.load(f)
job = answer.get("index_job")
if not isinstance(job, dict) or job.get("kind") != "started":
    raise SystemExit("index_start omitted its typed owner job projection")
start = job.get("value")
if not isinstance(start, dict) or start.get("state") not in ("started", "terminal"):
    raise SystemExit("index_start returned an unrecognized typed state")
data = start.get("data")
if not isinstance(data, dict) or not isinstance(data.get("ticket"), dict):
    raise SystemExit("index_start omitted the exact owner-issued ticket")
print(json.dumps(data["ticket"], separators=(",", ":"), sort_keys=True))
PY
)
indexed=false
after_sequence=0
last_index_attempt=0
for attempt in $(seq 1 180); do
  last_index_attempt=$attempt
  if ! "$backend_cli" --workspace "$owner_data" --project "$fixture" \
    --endpoint "$endpoint" --format json index_progress "$index_ticket" \
    --after-sequence "$after_sequence" >"$root/index-progress.json" \
    2>"$root/index-progress.err"; then
    printf 'typed index_progress failed on attempt %s; refusing to retry a protocol or authority error\n' \
      "$attempt" >&2
    cat "$root/index-progress.err" >&2
    exit 1
  fi
  python3 - "$root/index-progress.json" "$index_ticket" \
    >"$root/index-progress-state" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    answer = json.load(f)
expected_ticket = json.loads(sys.argv[2])
job = answer.get("index_job")
if not isinstance(job, dict) or job.get("kind") != "progress":
    print("invalid")
    raise SystemExit(0)
observation = job.get("value")
if not isinstance(observation, dict):
    print("invalid")
elif observation.get("state") == "pending":
    page = observation.get("detail")
    if not isinstance(page, dict) or page.get("ticket") != expected_ticket:
        print("invalid")
    else:
        print("pending", page.get("next_sequence", 0), sep="\t")
elif observation.get("state") == "terminal":
    terminal = observation.get("detail")
    if not isinstance(terminal, dict) or terminal.get("ticket") != expected_ticket:
        print("invalid")
    elif terminal.get("outcome", {}).get("state") == "published":
        print("published")
    else:
        print("terminal-failed")
elif observation.get("state") == "unknown":
    print("unknown")
else:
    print("invalid")
PY
  IFS=$'\t' read -r progress_state next_sequence <"$root/index-progress-state"
  case "$progress_state" in
    pending)
      after_sequence=$next_sequence
      sleep 1
      ;;
    published)
      indexed=true
      break
      ;;
    *)
      printf 'owner index job did not publish (state %s, attempt %s); typed receipt follows\n' \
        "$progress_state" "$attempt" >&2
      cat "$root/index-progress.json" >&2
      exit 1
      ;;
  esac
done
if [[ "$indexed" != true ]]; then
  printf 'owner index job remained pending after %s typed progress attempts; last cursor %s\n' \
    "$last_index_attempt" "$after_sequence" >&2
  cat "$root/index-progress.json" >&2
  exit 1
fi
if ! local_result=$("$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" --format markdown search "$marker" --limit 20 \
  2>"$root/local-search.err"); then
  printf '%s\n' "the published job did not make the fixture marker queryable locally" >&2
  cat "$root/local-search.err" >&2
  exit 1
fi
if [[ "$local_result" != *"$marker"* ]]; then
  printf '%s\n' "the published job omitted the fixture marker from the local product query" >&2
  printf '%s\n' "$local_result" >&2
  exit 1
fi
if ! "$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" --format json semantic-versions "$fixture" \
  >"$root/semantic-versions.json" 2>"$root/semantic-versions.err"; then
  printf '%s\n' "typed semantic-versions could not read the selected fixture publication" >&2
  cat "$root/semantic-versions.err" >&2
  exit 1
fi
semantic_coordinate=$(python3 - "$root/semantic-versions.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    answer = json.load(f)
records = answer.get("records")
if not isinstance(records, list):
    raise SystemExit("semantic-versions omitted its bounded record page")
selected = [record for record in records if "selected" in record.get("tags", [])]
if len(selected) != 1:
    raise SystemExit(f"fixture expected one selected Rust semantic coordinate; got {len(selected)}")
coordinate = selected[0].get("title")
if not isinstance(coordinate, str) or not coordinate.startswith("pkg:cargo/"):
    raise SystemExit("selected fixture coordinate is not the exact Cargo target")
if "complete" not in selected[0].get("tags", []):
    raise SystemExit("selected fixture semantic generation is not complete")
print(coordinate)
PY
)
printf 'using exact selected semantic coordinate %s\n' "$semantic_coordinate"
if ! "$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" --format markdown search "$marker" --limit 20 \
  >"$root/local-search-after-semantic-versions.out" \
  2>"$root/local-search-after-semantic-versions.err"; then
  printf '%s\n' "local product revision/query lost proof after semantic-versions" >&2
  cat "$root/local-search-after-semantic-versions.err" >&2
  exit 1
fi
if ! grep -q "$marker" "$root/local-search-after-semantic-versions.out"; then
  printf '%s\n' "fixture marker disappeared after semantic-versions" >&2
  cat "$root/local-search-after-semantic-versions.out" >&2
  exit 1
fi

owner_peer=$("$backend_cli" --workspace "$owner_data" --project "$fixture" \
  cluster owner show --format json | python3 -c 'import json,sys; print(json.load(sys.stdin)["endpoint"])')
client_output=$("$backend_cli" --workspace "$client_data" \
  cluster client init --key-file "$client_key")
client_peer=$(printf '%s\n' "$client_output" | sed -n \
  's/^Created private remote-index client identity \([[:xdigit:]]*\)\.$/\1/p')
if [[ -z "$client_peer" ]]; then
  printf '%s\n' "could not read the public client peer ID" >&2
  exit 1
fi

"$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" cluster owner grant product create --client-peer "$client_peer" \
  --capability-file "$capability_owner" --operations search >/dev/null
cp "$capability_owner" "$capability_client"
chmod 600 "$capability_client"
if ! "$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" cluster owner grant semantic create --client-peer "$client_peer" \
  --capability-file "$semantic_capability_owner" --package "$fixture" \
  --coordinate "$semantic_coordinate" --profile rust-2024 \
  >"$root/semantic-grant.out" 2>"$root/semantic-grant.err"; then
  printf '%s\n' "semantic catalog grant failed on attempt 1 after the exact index job published; not retrying an untyped failure" >&2
  cat "$root/semantic-grant.err" >&2
  tail -n 80 "$root/locald.log" >&2
  exit 1
fi
cp "$semantic_capability_owner" "$semantic_capability_client"
chmod 600 "$semantic_capability_client"
connect_report=$("$backend_cli" --workspace "$client_data" \
  cluster client connect --key-file "$client_key" \
  --owner-peer "$owner_peer" --owner-address "$address" \
  --capability-file "$capability_client")
if [[ "$connect_report" != *"Authorized product root:"* \
  || "$connect_report" != *"Operations: search"* ]]; then
  printf '%s\n' "client connect did not report the signed query scope" >&2
  exit 1
fi
catalog_report=$("$backend_cli" --workspace "$client_data" \
  cluster client semantic-catalog --key-file "$client_key" \
  --owner-peer "$owner_peer" --owner-address "$address" \
  --capability-file "$semantic_capability_client")
if [[ "$catalog_report" != *"Remote semantic catalog admitted."* ]]; then
  printf '%s\n' "the external client did not admit the selected semantic catalog" >&2
  exit 1
fi

run_remote_query() {
  local query_pid owner_rss client_rss sampled_total peak_total samples result
  "$backend_cli" --workspace "$client_data" \
    cluster client query --key-file "$client_key" \
    --owner-peer "$owner_peer" --owner-address "$address" \
    --capability-file "$capability_client" --operation search \
    --value "$marker" --limit 20 >"$root/remote-query.out" 2>"$root/remote-query.err" &
  query_pid=$!
  peak_total=0
  samples=0
  while kill -0 "$query_pid" 2>/dev/null; do
    owner_rss=$(ps -o rss= -p "$locald_pid" 2>/dev/null | tr -d '[:space:]' || true)
    client_rss=$(ps -o rss= -p "$query_pid" 2>/dev/null | tr -d '[:space:]' || true)
    if [[ "$owner_rss" =~ ^[0-9]+$ && "$client_rss" =~ ^[0-9]+$ ]]; then
      samples=$((samples + 1))
      sampled_total=$((owner_rss + client_rss))
      if (( sampled_total > peak_total )); then
        peak_total=$sampled_total
      fi
    fi
    sleep 0.01
  done
  if ! wait "$query_pid"; then
    printf '%s\n' "the external remote query failed" >&2
    exit 1
  fi
  result=$(cat "$root/remote-query.out")
  if [[ "$result" != *"$marker"* ]]; then
    printf '%s\n' "the external client did not receive the indexed fixture marker" >&2
    exit 1
  fi
  if (( samples == 0 )); then
    printf '%s\n' "could not sample owner and client RSS during the query" >&2
    exit 1
  fi
  if (( peak_total > rss_limit_kb )); then
    printf 'sampled owner plus client RSS exceeded %s KiB: %s KiB\n' \
      "$rss_limit_kb" "$peak_total" >&2
    exit 1
  fi
  printf 'sampled owner plus client RSS: %s KiB (limit %s KiB)\n' \
    "$peak_total" "$rss_limit_kb"
}

run_remote_query
kill "$locald_pid"
wait "$locald_pid" 2>/dev/null || true
locald_pid=
for _ in $(seq 1 100); do
  [[ ! -S "$endpoint" ]] && break
  sleep 0.1
done
rm -f "$endpoint"
start_locald
run_remote_query

"$backend_cli" --workspace "$client_data" --format json semantic-hydrate \
  --package "$fixture" --coordinate "$semantic_coordinate" --profile rust-2024 \
  --image-ordinal 0 --plane core --store "$semantic_store" \
  --key-file "$client_key" --owner-peer "$owner_peer" \
  --owner-address "$address" --capability-file "$semantic_capability_client" \
  >"$root/semantic-hydrate.json"
python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    result = json.load(f)
if result["rangeRequests"] == 0 or result["transferredBytes"] == 0 or result["totalSegments"] == 0:
    raise SystemExit("remote semantic hydration returned no verified ranges")
print(f"remote semantic ranges: {result['rangeRequests']}; bytes: {result['transferredBytes']}")
' "$root/semantic-hydrate.json"

"$backend_cli" --workspace "$owner_data" --format json \
  cluster owner grant list >"$root/grants.json"
python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    grants = json.load(f)
if len(grants) != 2:
    raise SystemExit("expected one product grant and one semantic grant")
for label, grant in (("product", next(g for g in grants if g["product"])),
                     ("semantic", next(g for g in grants if g["semantic"]))):
    if grant["responseBytes"] == 0 or grant["responseBytes"] > grant["byteBudget"]:
        raise SystemExit(f"{label} response-byte accounting is missing or over budget")
    print(f"persisted {label} response bytes: {grant['responseBytes']}/{grant['byteBudget']}")
' "$root/grants.json"

grant_id=$(python3 -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    grants = json.load(f)
    print(next(g["grantId"] for g in grants if g["product"]))
' "$root/grants.json")
"$backend_cli" --workspace "$owner_data" cluster owner grant revoke \
  --grant-id "$grant_id" >/dev/null
kill "$locald_pid"
wait "$locald_pid" 2>/dev/null || true
locald_pid=
rm -f "$endpoint"
start_locald
if "$backend_cli" --workspace "$client_data" \
  cluster client query --key-file "$client_key" \
  --owner-peer "$owner_peer" --owner-address "$address" \
  --capability-file "$capability_client" --operation search \
  --value "$marker" --limit 20 >"$root/revoked-query.out" 2>"$root/revoked-query.err"; then
  printf '%s\n' "the restarted owner accepted a revoked grant" >&2
  exit 1
fi
if ! grep -q "remote grant was revoked by its owner" "$root/revoked-query.err"; then
  printf '%s\n' "the owner did not return its typed revoked-grant response" >&2
  exit 1
fi

printf '%s\n' "remote index journey passed (product query, semantic hydration, cold restart, byte meter, and persistent revoke)"
