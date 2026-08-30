#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=dylint/shipping-workspaces.sh
source "$project_dir/tools/dylint/shipping-workspaces.sh"

if [[ "${NUDOX_QUALITY_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "path:$project_dir#quality" -c env \
    NUDOX_QUALITY_NIX_ENV=1 \
    NUDOX_DYLINT_NIX_ENV=1 \
    "$0" "$@"
fi

# shellcheck source=pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"

"$project_dir/tools/check-crate-layout.sh"
"$project_dir/tools/dylint/run.sh"
for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  manifest="$project_dir/$relative_manifest"
  stable_cargo fmt --manifest-path "$manifest" --all -- --check
  stable_cargo test --manifest-path "$manifest" --workspace --all-targets --locked --offline
  stable_cargo clippy --manifest-path "$manifest" --workspace --all-targets --locked --offline -- \
    -D warnings \
    -D clippy::undocumented_unsafe_blocks
  stable_cargo clippy --manifest-path "$manifest" --workspace --all-targets --all-features --locked --offline -- \
    -D warnings \
    -D clippy::undocumented_unsafe_blocks
  stable_cargo doc --manifest-path "$manifest" --workspace --no-deps --all-features --locked --offline
done
stable_cargo test \
  --manifest-path "$project_dir/Cargo.toml" \
  -p nudox-runtime \
  --features loom-model \
  --lib \
  --locked \
  --offline

# Repository-level custody check: unsafe is denied by default, and local exceptions
# must remain inside the named, independently reviewed proof modules.
absolute_source_roots=()
for relative_root in "${shipping_source_roots[@]}"; do
  absolute_source_roots+=("$project_dir/$relative_root")
done
unsafe_sites="$(rg -n --glob '*.rs' '\bunsafe\s+(fn|impl|trait)|unsafe\s*\{' "${absolute_source_roots[@]}" || true)"
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

# Repository-level dependency check: source syntax cannot prove the resolved
# shipping graph. Resolve normal edges independently for every inventoried Cargo
# workspace so nested workspaces cannot fall outside the root graph silently.
for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  if [[ "$relative_manifest" == "planes/index/Cargo.toml" || "$relative_manifest" == "planes/application/Cargo.toml" ]]; then
    continue
  fi
  normal_tree="$(
    stable_cargo tree \
      --manifest-path "$project_dir/$relative_manifest" \
      --workspace \
      --edges normal \
      --prefix none \
      --locked \
      --offline
  )"
  if printf '%s\n' "$normal_tree" | rg '^(serde|serde_json|tokio|async-trait|futures) v'; then
    echo "forbidden normal dependency resolved in $relative_manifest" >&2
    exit 1
  fi
done

# The application aggregate intentionally contains JSON-RPC and GPUI adapters. Keep those edges
# out of the concrete protocol-neutral behavior owner instead of pretending the whole application
# graph is portable.
application_manifest="$project_dir/planes/application/Cargo.toml"
application_core_tree="$(
  stable_cargo tree \
    --manifest-path "$application_manifest" \
    --package wave-application-core \
    --edges normal \
    --prefix none \
    --locked \
    --offline
)"
if printf '%s\n' "$application_core_tree" | rg '^(serde|serde_json|tokio|async-trait|futures|gpui|tracing|opentelemetry) v'; then
  echo 'adapter dependency resolved in portable application core' >&2
  exit 1
fi
stable_cargo test \
  --manifest-path "$application_manifest" \
  --workspace \
  --all-targets \
  --all-features \
  --locked \
  --offline

# The index plane deliberately permits runtime/query-engine SDKs only below its
# nested adapters. Every portable package under planes/index/crates is checked
# independently so adding one server adapter cannot make the aggregate
# workspace tree look portable.
index_manifest="$project_dir/planes/index/Cargo.toml"
for portable_manifest in "$project_dir"/planes/index/crates/*/Cargo.toml; do
  portable_package="$(rg -m 1 '^name = "[^"]+"$' "$portable_manifest" | sed -n 's/^name = "\([^"]*\)"$/\1/p')"
  if [[ -z "$portable_package" ]]; then
    echo "cannot resolve portable package name from $portable_manifest" >&2
    exit 1
  fi
  normal_tree="$(
    stable_cargo tree \
      --manifest-path "$index_manifest" \
      --package "$portable_package" \
      --edges normal \
      --prefix none \
      --locked \
      --offline
  )"
  if printf '%s\n' "$normal_tree" | rg '^(serde|serde_json|tokio|async-trait|futures|trustfall|qdrant-client|tonic|tracing|opentelemetry) v'; then
    echo "adapter dependency resolved in portable index package $portable_package" >&2
    exit 1
  fi
done

"$project_dir/planes/application/scripts/check-client-budget.sh"
