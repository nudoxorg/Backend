#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../../../../.." && pwd)"

if [[ "${NUDOX_QDRANT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "$project_dir#quality" -c env \
    NUDOX_QDRANT_NIX_ENV=1 \
    "$0" "$@"
fi

# shellcheck source=../../../../../tools/pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"

qdrant_test_dir="$(mktemp -d /tmp/nudox-qdrant-service.XXXXXX)"
qdrant_http_port="${QDRANT_TEST_HTTP_PORT:-6339}"
qdrant_grpc_port="${QDRANT_TEST_GRPC_PORT:-6340}"
qdrant_pid=''

cleanup() {
  if [[ -n "$qdrant_pid" ]] && kill -0 "$qdrant_pid" 2>/dev/null; then
    kill "$qdrant_pid"
    wait "$qdrant_pid" || true
  fi
  find "$qdrant_test_dir" -depth -delete
}
trap cleanup EXIT

QDRANT__SERVICE__HTTP_PORT="$qdrant_http_port" \
QDRANT__SERVICE__GRPC_PORT="$qdrant_grpc_port" \
QDRANT__STORAGE__STORAGE_PATH="$qdrant_test_dir/storage" \
  qdrant --disable-telemetry >"$qdrant_test_dir/qdrant.log" 2>&1 &
qdrant_pid="$!"
qdrant_url="http://127.0.0.1:$qdrant_http_port"

ready=0
for _ in $(seq 1 100); do
  if curl --fail --silent "$qdrant_url/readyz" >/dev/null; then
    ready=1
    break
  fi
  if ! kill -0 "$qdrant_pid" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if ((ready == 0)); then
  sed -n '1,240p' "$qdrant_test_dir/qdrant.log" >&2
  echo 'Qdrant did not become ready' >&2
  exit 1
fi

QDRANT_URL="$qdrant_url" stable_cargo test \
  --manifest-path "$project_dir/planes/index/Cargo.toml" \
  -p nudox-index-qdrant \
  --test real_service \
  --locked \
  --offline \
  real_qdrant_service_upload_readback_filter_query_delete_and_recovery \
  -- \
  --ignored \
  --exact \
  --nocapture
