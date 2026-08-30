#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ "${NUDOX_DYLINT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "path:$project_dir#quality" -c env NUDOX_DYLINT_NIX_ENV=1 "$0" "$@"
fi

lint_workspace="$project_dir/tools/dylint"
lint_target_dir="${CARGO_TARGET_DIR:-$project_dir/target/dylint}"
export CARGO_TARGET_DIR="$lint_target_dir"
# shellcheck source=shipping-workspaces.sh
source "$lint_workspace/shipping-workspaces.sh"
# shellcheck source=../../pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"

"$lint_workspace/ui-test.sh"

shopt -s nullglob
lint_libraries=("$lint_target_dir"/debug/*"nudox_semantic_lints@$dylint_toolchain-"*)
if ((${#lint_libraries[@]} != 1)); then
  echo 'Dylint UI gate did not produce the pinned semantic lint library' >&2
  exit 1
fi
lint_library="${lint_libraries[0]}"

export DYLINT_RUSTFLAGS="-Dnudox_erased_map_err -Dnudox_stringly_state_field -Dnudox_redundant_public_accessor -Dnudox_dynamic_dispatch"

for relative_manifest in "${shipping_workspace_manifests[@]}"; do
  dylint_cargo dylint \
    --no-deps \
    --no-metadata \
    --lib-path "$lint_library" \
    --manifest-path "$project_dir/$relative_manifest" \
    --workspace \
    -- \
    --locked \
    --offline \
    --lib \
    --bins
done
