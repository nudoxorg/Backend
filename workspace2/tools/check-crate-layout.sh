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

workspace_manifest="$project_dir/$shipping_workspace_manifest"
while IFS= read -r package_manifest; do
  if [[ "$package_manifest" == "$project_dir/$shipping_source_root"/*/Cargo.toml ]]; then
    continue
  fi
  echo "shipping crate must live in the single crates/ directory: $package_manifest" >&2
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

scenario_modules="$(rg -n --glob 'src/scenario.rs' '.' "$project_dir/$shipping_source_root" || true)"
if [[ -n "$scenario_modules" ]]; then
  printf '%s\n' "$scenario_modules" >&2
  echo 'integration scenarios belong in a top-level tests/ tree, not shipping src/scenario.rs' >&2
  exit 1
fi
