#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
cd "$project_dir"

run_tests() {
  local profile="$1"
  if cargo nextest --version >/dev/null 2>&1; then
    nix shell nixpkgs#clang -c cargo nextest run --workspace --all-targets --profile "$profile"
  else
    nix shell nixpkgs#clang -c cargo test --workspace --all-targets
  fi
}

case "${1:-}" in
  quick)
    cargo fmt --all -- --check
    run_tests default
    ;;
  pr)
    "$project_dir/tools/quality.sh"
    ;;
  nightly)
    "$project_dir/tools/quality.sh"
    nix shell nixpkgs#clang -c cargo test --workspace --all-targets --release
    "$project_dir/layout-lab/run-miri-foundation.sh"
    "$project_dir/layout-lab/run-miri-runtime.sh"
    ;;
  *)
    echo 'usage: tools/test-tier.sh {quick|pr|nightly}' >&2
    exit 2
    ;;
esac
