#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=dylint/shipping-workspaces.sh
source "$project_dir/tools/dylint/shipping-workspaces.sh"

if [[ -n "${NUDOX_STABLE_TOOLCHAIN:-}" ]]; then
  cargo_bin="$NUDOX_STABLE_TOOLCHAIN/bin/cargo"
else
  cargo_bin="$(command -v cargo)"
fi

# These predate the single-crates-root rule. Keep the gate green while making the migration debt
# explicit; no new exception may be added without a Sol-owned roadmap row.
legacy_layout_exceptions=(
  "$project_dir/planes/index/adapters/nudox-index-qdrant/Cargo.toml"
  "$project_dir/planes/index/adapters/nudox-index-tantivy/Cargo.toml"
  "$project_dir/planes/application/gpui_shell/Cargo.toml"
)

is_legacy_exception() {
  local manifest="$1"
  local exception
  for exception in "${legacy_layout_exceptions[@]}"; do
    if [[ "$manifest" == "$exception" ]]; then
      return 0
    fi
  done
  return 1
}

for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  workspace_manifest="$project_dir/$relative_manifest"
  workspace_root="$(dirname "$workspace_manifest")"
  while IFS= read -r package_manifest; do
    if [[ "$package_manifest" == "$workspace_manifest" || "$package_manifest" == "$workspace_root"/crates/*/Cargo.toml ]]; then
      continue
    fi
    if is_legacy_exception "$package_manifest"; then
      continue
    fi
    echo "shipping crate must live at the workspace root or its single crates/ directory: $package_manifest" >&2
    exit 1
  done < <(
    "$cargo_bin" metadata \
      --manifest-path "$workspace_manifest" \
      --format-version 1 \
      --no-deps \
      --locked \
      --offline |
      jq -r '.packages[].manifest_path'
  )
done

scenario_modules="$(rg -n --glob 'src/scenario.rs' '.' "${shipping_source_roots[@]/#/$project_dir/}" || true)"
if [[ -n "$scenario_modules" ]]; then
  printf '%s\n' "$scenario_modules" >&2
  echo 'integration scenarios belong in a top-level tests/ tree, not shipping src/scenario.rs' >&2
  exit 1
fi
