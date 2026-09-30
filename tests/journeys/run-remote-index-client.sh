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

root=$(mktemp -d "${TMPDIR:-/tmp}/backend-remote-index.XXXXXX")
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
semantic_coordinate=pkg:cargo/remote-index-fixture@0.1.0
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
pub fn remote_index_journey_marker() -> &'static str {
    "remote_index_journey_marker"
}
EOF

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
    exec "$locald_bin" --workspace "$owner_data" --endpoint "$endpoint" \
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
      exit 1
    fi
    sleep 0.1
  done
  printf '%s\n' "backend-locald did not become ready" >&2
  exit 1
}

start_locald
"$backend_cli" --workspace "$owner_data" --project "$fixture" \
  --endpoint "$endpoint" index "$fixture" >/dev/null

indexed=false
for _ in $(seq 1 180); do
  if local_result=$("$backend_cli" --workspace "$owner_data" --project "$fixture" \
    --endpoint "$endpoint" --format markdown search "$marker" --limit 20 2>/dev/null); then
    if [[ "$local_result" == *"$marker"* ]]; then
      indexed=true
      break
    fi
  fi
  sleep 1
done
if [[ "$indexed" != true ]]; then
  printf '%s\n' "the real locald index did not publish the fixture marker" >&2
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
  cluster owner grant product create --client-peer "$client_peer" \
  --capability-file "$capability_owner" --operations search >/dev/null
cp "$capability_owner" "$capability_client"
chmod 600 "$capability_client"
"$backend_cli" --workspace "$owner_data" --project "$fixture" \
  cluster owner grant semantic create --client-peer "$client_peer" \
  --capability-file "$semantic_capability_owner" --package "$fixture" \
  --coordinate "$semantic_coordinate" --profile rust-2024 >/dev/null
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
