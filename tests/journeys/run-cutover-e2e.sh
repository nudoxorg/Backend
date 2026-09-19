#!/bin/sh
set -eu

workspace=${BACKEND_WORKSPACE_SNAPSHOT:-$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)}
cargo=${BACKEND_STABLE_CARGO:-cargo}
mode=${1:-all}

case "$mode" in
  all|prepare|execute) ;;
  *)
    printf '%s\n' "usage: $0 [prepare|execute]" >&2
    exit 64
    ;;
esac

# Build the journey package's own process wrappers. This makes every black-box
# test hermetic: no binary is discovered through a workspace target-directory
# convention or an earlier build step.
if [ "$mode" != execute ]; then
  "$cargo" build --manifest-path "$workspace/tests/journeys/Cargo.toml" \
    --bins
  "$cargo" test --manifest-path "$workspace/tests/journeys/Cargo.toml" \
    --lib --test cutover_e2e --no-run
  "$cargo" test --manifest-path "$workspace/Cargo.toml" \
    -p backend-laws --test cutover_regressions --no-run
fi

if [ "$mode" = prepare ]; then
  exit 0
fi

"$cargo" test --manifest-path "$workspace/tests/journeys/Cargo.toml" \
  --lib -- --nocapture
"$cargo" test --manifest-path "$workspace/tests/journeys/Cargo.toml" \
  --test cutover_e2e -- --nocapture
"$cargo" test --manifest-path "$workspace/Cargo.toml" \
  -p backend-laws --test cutover_regressions -- --nocapture
