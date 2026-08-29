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

run_nested_workspaces() {
  while IFS= read -r manifest; do
    local nested_dir
    nested_dir="$(dirname "$manifest")"
    nix shell nixpkgs#clang -c cargo test --manifest-path "$manifest" --workspace --all-targets
    nix shell nixpkgs#clang -c cargo clippy --manifest-path "$manifest" --workspace --all-targets -- -D warnings
    nix shell nixpkgs#clang -c cargo clippy --manifest-path "$manifest" --workspace --all-targets --all-features -- -D warnings
    nix shell nixpkgs#clang -c cargo doc --manifest-path "$manifest" --workspace --no-deps --all-features
    cargo fmt --manifest-path "$manifest" --all -- --check
    test -d "$nested_dir"
  done < <(find adapters harness -mindepth 2 -maxdepth 2 -name Cargo.toml -print 2>/dev/null | sort)
}

case "${1:-}" in
  quick)
    cargo fmt --all -- --check
    run_tests default
    ;;
  pr)
    "$project_dir/tools/quality.sh"
    run_nested_workspaces
    ;;
  nightly)
    "$project_dir/tools/quality.sh"
    run_nested_workspaces
    nix shell nixpkgs#clang -c cargo test --workspace --all-targets --release
    "$project_dir/layout-lab/run-miri-foundation.sh"
    "$project_dir/layout-lab/run-miri-runtime.sh"
    ;;
  *)
    echo 'usage: tools/test-tier.sh {quick|pr|nightly}' >&2
    exit 2
    ;;
esac
