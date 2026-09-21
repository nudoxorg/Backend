#!/usr/bin/env bash
set -euo pipefail

workspace="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
target="$workspace/.local/live-target"
mkdir -p "$target"

# Keep the live lane explicit and reproducible. luna-tools supplies the
# repository's pinned agent command surface; cargo is requested explicitly so
# this script never relies on an ambient developer shell. Do not replace this
# with `nix develop`: the evidence lane must show exactly which tools it ran.
exec nix shell "$workspace#luna-tools" nixpkgs#cargo nixpkgs#curl --command \
  env CARGO_TARGET_DIR="$target" \
      NUDOX_LIVE_REGISTRY=1 \
      cargo test -p backend-journeys --test live_registry -- --ignored --nocapture "$@"
