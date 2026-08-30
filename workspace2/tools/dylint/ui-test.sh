#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../.." && pwd)"

if [[ "${NUDOX_DYLINT_NIX_ENV:-}" != 1 ]]; then
  exec nix develop "$project_dir#quality" -c env NUDOX_DYLINT_NIX_ENV=1 "$0" "$@"
fi

lint_workspace="$project_dir/tools/dylint"
# shellcheck source=../../pinned-toolchains.sh
source "$project_dir/tools/pinned-toolchains.sh"

# Native build scripts can preserve read-only modes from inputs copied out of
# the Nix store. Cargo may legitimately rerun those scripts after an input
# change, so make only their generated output trees owner-writable before a
# rebuild replaces cached headers or objects.
if [[ -d "$CARGO_TARGET_DIR/debug/build" ]]; then
  find "$CARGO_TARGET_DIR/debug/build" \
    -path '*/out/*' \
    -type f \
    -exec chmod u+w {} +
fi

(
  cd "$lint_workspace"
  dylint_cargo fmt --all -- --check
  # Dylint generates a temporary Cargo package while starting its UI driver.
  # Do not let a caller-provided temporary directory inherit an unrelated
  # ancestor Cargo workspace.
  (
    unset TMPDIR
    dylint_cargo test --locked --offline
  )
)
