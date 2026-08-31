#!/usr/bin/env bash
# Reproducibly account for the portable CLI's release artifact and direct dependency graph.
set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"
manifest="$project_dir/Cargo.toml"
budget_bytes=$((50 * 1024 * 1024))
first_target=$(mktemp -d)
second_target=$(mktemp -d)
cleanup() {
  rm -rf "$first_target" "$second_target"
}
trap cleanup EXIT

build_client() {
  local target_dir=$1
  CARGO_TARGET_DIR="$target_dir" stable_cargo build \
    --locked \
    --offline \
    --release \
    --manifest-path "$manifest" \
    --package wave-application-cli
}

build_client "$first_target"
build_client "$second_target"

first_binary="$first_target/release/wave-application-cli"
second_binary="$second_target/release/wave-application-cli"
first_size=$(wc -c < "$first_binary")
second_size=$(wc -c < "$second_binary")
first_hash=$(shasum -a 256 "$first_binary" | awk '{print $1}')
second_hash=$(shasum -a 256 "$second_binary" | awk '{print $1}')
if command -v llvm-size >/dev/null; then
  section_bytes=$(llvm-size -B "$first_binary")
elif command -v size >/dev/null; then
  section_bytes=$(size -m "$first_binary")
else
  section_bytes="unavailable: neither llvm-size nor size is on PATH"
fi

if (( first_size > budget_bytes || second_size > budget_bytes )); then
  echo "client release binary exceeds 50 MiB: first=$first_size second=$second_size" >&2
  exit 1
fi
if [[ "$first_hash" != "$second_hash" ]]; then
  echo "client release binary is not reproducible: first=$first_hash second=$second_hash" >&2
  exit 1
fi

dependency_tree=$(stable_cargo tree \
  --locked \
  --offline \
  --manifest-path "$manifest" \
  --package wave-application-cli \
  --edges normal)
if rg --quiet '(^|[[:space:]])(tokio|reqwest|opentelemetry)([[:space:]]| v|$)' <<<"$dependency_tree"; then
  echo "client graph contains a server/runtime SDK edge" >&2
  exit 1
fi

"$first_binary" generate 900 rust parse client-budget-self-check 'fn client_budget_self_check() {}' >/dev/null
printf 'client_bytes=%s\nclient_sha256=%s\nclient_sections:\n%s\ndependency_tree:\n%s\n' \
  "$first_size" "$first_hash" "$section_bytes" "$dependency_tree"
