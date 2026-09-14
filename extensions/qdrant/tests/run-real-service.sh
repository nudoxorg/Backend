#!/usr/bin/env bash
# Runs the live Qdrant contract against one isolated local service.
# Exercises restart and outage behavior with a persistent temporary store.
# Enters the pinned development shell when native dependencies are absent.
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../../../.." && pwd)"

if [[ "${SERVER_QDRANT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "$project_dir#development" -c env \
    SERVER_QDRANT_NIX_ENV=1 \
    "$0" "$@"
fi

qdrant_test_dir="$(mktemp -d /tmp/server-qdrant-service.XXXXXX)"
qdrant_http_port="${QDRANT_TEST_HTTP_PORT:-$((20000 + RANDOM % 20000))}"
qdrant_grpc_port="${QDRANT_TEST_GRPC_PORT:-$((qdrant_http_port + 1))}"
recovery_collection="${QDRANT_TEST_COLLECTION:-server_restart_$$}"
qdrant_pid=''

cleanup() {
  stop_qdrant
  find "$qdrant_test_dir" -depth -delete
}
trap cleanup EXIT

qdrant_url="http://127.0.0.1:$qdrant_http_port"

start_qdrant() {
  if curl --fail --silent --max-time 1 "$qdrant_url/readyz" >/dev/null 2>&1; then
    echo "refusing to test a pre-existing Qdrant service at $qdrant_url" >&2
    return 1
  fi
  (
    cd "$qdrant_test_dir"
    exec env \
      QDRANT__SERVICE__HTTP_PORT="$qdrant_http_port" \
      QDRANT__SERVICE__GRPC_PORT="$qdrant_grpc_port" \
      QDRANT__STORAGE__STORAGE_PATH="$qdrant_test_dir/storage" \
      qdrant --disable-telemetry
  ) >>"$qdrant_test_dir/qdrant.log" 2>&1 &
  qdrant_pid="$!"

  for _ in $(seq 1 100); do
    if curl --fail --silent --max-time 1 "$qdrant_url/readyz" >/dev/null; then
      if kill -0 "$qdrant_pid" 2>/dev/null; then
        return 0
      fi
      echo "a foreign Qdrant service became ready after the launched child exited" >&2
      return 1
    fi
    if ! kill -0 "$qdrant_pid" 2>/dev/null; then
      break
    fi
    sleep 0.1
  done
  sed -n '1,240p' "$qdrant_test_dir/qdrant.log" >&2
  echo 'Qdrant did not become ready' >&2
  return 1
}

stop_qdrant() {
  if [[ -z "$qdrant_pid" ]]; then
    return 0
  fi
  if kill -0 "$qdrant_pid" 2>/dev/null; then
    kill "$qdrant_pid"
    for _ in $(seq 1 100); do
      if ! kill -0 "$qdrant_pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    if kill -0 "$qdrant_pid" 2>/dev/null; then
      kill -KILL "$qdrant_pid"
    fi
  fi
  wait "$qdrant_pid" 2>/dev/null || true
  qdrant_pid=''
}

run_ignored() {
  local test_name="$1"
  QDRANT_URL="$qdrant_url" \
  QDRANT_TEST_COLLECTION="$recovery_collection" \
    cargo test \
      --manifest-path "$project_dir/Cargo.toml" \
      -p backend-extension-qdrant \
      --test real_service \
      --locked \
      --offline \
      "$test_name" \
      -- \
      --ignored \
      --exact \
      --nocapture
}

run_retrieval_facade() {
  QDRANT_URL="$qdrant_url" \
    cargo test \
      --manifest-path "$project_dir/Cargo.toml" \
      -p backend-engine \
      --test sealed_boundary_public \
      --locked \
      --offline \
      sealed_boundary_public_journey_classifies_retrieval_terminals \
      -- \
      --exact \
      --nocapture
}

start_qdrant
run_ignored real_qdrant_service_metric_matrix_isolates_authority_and_stabilizes_ties
run_retrieval_facade
run_ignored real_qdrant_service_prepare_restart_fixture
stop_qdrant
run_ignored real_qdrant_service_reports_transport_during_launcher_outage
start_qdrant
run_ignored real_qdrant_service_verifies_restart_fixture_and_cleans_up
