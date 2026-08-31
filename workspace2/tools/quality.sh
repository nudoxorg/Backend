#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=dylint/shipping-workspaces.sh
source "$project_dir/tools/dylint/shipping-workspaces.sh"

quality_invocation=("$@")
quality_mode="${1:-closure}"
if (($# != 0)); then
  shift
fi
case "$quality_mode" in
  focused | capability)
    if (($# == 0)); then
      echo "usage: $0 $quality_mode <cargo-package>..." >&2
      exit 2
    fi
    package_flags=()
    for package_name in "$@"; do
      package_flags+=(--package "$package_name")
    done
    ;;
  closure)
    if (($# != 0)); then
      echo "usage: $0 closure" >&2
      exit 2
    fi
    package_flags=(--workspace)
    ;;
  *)
    echo "usage: $0 {focused <cargo-package>...|capability <cargo-package>...|closure}" >&2
    exit 2
    ;;
esac

if [[ "${NUDOX_QUALITY_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "path:$project_dir#quality" -c env \
    NUDOX_QUALITY_NIX_ENV=1 \
    NUDOX_DYLINT_NIX_ENV=1 \
    "$0" "${quality_invocation[@]}"
fi

# shellcheck source=pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"

root_manifest="$project_dir/$shipping_workspace_manifest"
stable_cargo fmt --manifest-path "$root_manifest" "${package_flags[@]}" -- --check
stable_cargo test \
  --manifest-path "$root_manifest" \
  "${package_flags[@]}" \
  --all-targets \
  --locked \
  --offline
stable_cargo clippy \
  --manifest-path "$root_manifest" \
  "${package_flags[@]}" \
  --all-targets \
  --all-features \
  --locked \
  --offline \
  -- \
  -D warnings \
  -D clippy::undocumented_unsafe_blocks

if [[ "$quality_mode" == focused ]]; then
  exit 0
fi

"$project_dir/tools/check-crate-layout.sh"
"$project_dir/tools/dylint/run.sh" "${package_flags[@]}"
stable_cargo doc \
  --manifest-path "$root_manifest" \
  "${package_flags[@]}" \
  --no-deps \
  --all-features \
  --locked \
  --offline

if [[ "$quality_mode" == capability ]]; then
  exit 0
fi

stable_cargo test \
  --manifest-path "$root_manifest" \
  -p nudox-runtime \
  --features loom-model \
  --lib \
  --locked \
  --offline
stable_cargo test \
  --manifest-path "$root_manifest" \
  -p nudox-index-graph-vector \
  --features loom-model \
  --lib \
  --locked \
  --offline

# Repository-level custody check: unsafe is denied by default, and local exceptions
# must remain inside the named, independently reviewed proof modules.
unsafe_sites="$(rg -n --glob '*.rs' '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' "$project_dir/$shipping_source_root" || true)"
unexpected_unsafe="$unsafe_sites"
for reviewed_unsafe in "${reviewed_unsafe_modules[@]}"; do
  unexpected_unsafe="$(printf '%s\n' "$unexpected_unsafe" | rg -v -F "$project_dir/$reviewed_unsafe:" || true)"
done
if [[ -n "$unexpected_unsafe" ]]; then
  printf '%s\n' "$unexpected_unsafe" >&2
  echo 'handwritten unsafe exists outside the reviewed proof boundaries' >&2
  exit 1
fi
for reviewed_unsafe in "${reviewed_unsafe_modules[@]}"; do
  reviewed_path="$project_dir/$reviewed_unsafe"
  if rg -q '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' "$reviewed_path" && ! rg -q 'SAFETY:' "$reviewed_path"; then
    echo "$reviewed_path requires local written SAFETY proofs" >&2
    exit 1
  fi
done

# Resolve every package marked portable (and every unmarked new package) by
# root metadata. Adapter identity is declared with `package.metadata.nudox.role
# = "adapter"` in the owning manifest, so an adapter cannot make a portable
# graph look clean merely by sharing a workspace. Capture metadata before
# iterating so a failed metadata command cannot be hidden by process
# substitution's asynchronous exit status.
metadata_json="$(
  stable_cargo metadata \
    --manifest-path "$root_manifest" \
    --format-version 1 \
    --no-deps \
    --locked \
    --offline
)"
portable_packages="$(
  jq -r '.packages[] | select((.metadata.nudox.role? // "portable") != "adapter") | .name' \
    <<<"$metadata_json"
)"
if [[ -z "$portable_packages" ]]; then
  echo 'root metadata returned no portable packages' >&2
  exit 1
fi
while IFS= read -r portable_package; do
  normal_tree="$(
    stable_cargo tree \
      --manifest-path "$root_manifest" \
      --package "$portable_package" \
      --edges normal \
      --prefix none \
      --locked \
      --offline
  )"
  if printf '%s\n' "$normal_tree" | rg '^(serde|serde_json|tokio|async-trait|futures|trustfall|qdrant-client|tonic|gpui|tracing|opentelemetry) v'; then
    echo "adapter dependency resolved in portable package $portable_package" >&2
    exit 1
  fi
done <<<"$portable_packages"

"$project_dir/tools/check-client-budget.sh"
