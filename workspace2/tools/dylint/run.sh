#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ "${NUDOX_DYLINT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "path:$project_dir#quality" -c env NUDOX_DYLINT_NIX_ENV=1 "$0" "$@"
fi

lint_workspace="$project_dir/tools/dylint"
lint_target_dir="${CARGO_TARGET_DIR:-$project_dir/target/dylint}"
export CARGO_TARGET_DIR="$lint_target_dir"
# Keep Dylint's generated compiler driver with the caller's target tree. The
# default under `$HOME/.dylint_drivers` makes a quality run depend on mutable
# user state and can silently reuse a driver built for another source tree.
dylint_driver_path="${DYLINT_DRIVER_PATH:-$lint_target_dir/drivers}"
mkdir -p "$dylint_driver_path"
export DYLINT_DRIVER_PATH="$dylint_driver_path"
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

# Keep ordinary compiler warnings fatal and make every architectural law non-waivable in shipping
# code.  The explicit force inventory prevents a crate-local `allow` from silently weakening a law.
export DYLINT_RUSTFLAGS="-Aunknown-lints -Dwarnings \
  -Fnudox_dynamic_dispatch \
  -Fnudox_dynamic_json_construction \
  -Fnudox_enum_static_str_projection \
  -Fnudox_erased_map_err \
  -Fnudox_poison_sync_primitive \
  -Fnudox_redundant_public_accessor \
  -Fnudox_stringly_state_field"

dylint_cargo dylint \
  --no-deps \
  --no-metadata \
  --lib-path "$lint_library" \
  --manifest-path "$project_dir/$shipping_workspace_manifest" \
  --workspace \
  -- \
  --locked \
  --offline \
  --all-features \
  --all-targets
