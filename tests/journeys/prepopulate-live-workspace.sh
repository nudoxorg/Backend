#!/usr/bin/env bash
set -euo pipefail

workspace="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
root="${1:-$workspace/.local/live-workspace}"
lane="${2:-rust}"
target="$workspace/.local/live-target"
mkdir -p "$target"

# This command leaves a durable, real-registry package under `$root/$lane` and
# a `gui-handoff.json` receipt that the real GPUI harness can open. The
# registry lane owns product/model evidence; rendered PNGs belong to the GPUI
# harness and are never fabricated here. It deliberately uses `nix shell`; the
# release lane must not inherit an ambient toolchain or evaluate the
# development shell.
nix shell "$workspace#luna-tools" nixpkgs#cargo nixpkgs#curl --command \
  env CARGO_TARGET_DIR="$target" \
      NUDOX_LIVE_REGISTRY=1 \
      NUDOX_LIVE_ECOSYSTEMS="$lane" \
      NUDOX_LIVE_WORKSPACE_ROOT="$root" \
      NUDOX_LIVE_RESET_WORKSPACE=1 \
      NUDOX_LIVE_KEEP_WORKSPACE=1 \
      NUDOX_LIVE_ARTIFACT_DIR="$root/artifacts" \
      cargo test -p backend-journeys --test live_registry -- --ignored --nocapture \
      pinned_native_registries_ingest_through_cli_mcp_and_desktop

printf 'live workspace ready: %s/%s\n' "$root" "$lane"
printf 'GUI receipt: %s/artifacts/%s/receipt.json\n' "$root" "$lane"
printf 'GUI handoff: %s/artifacts/%s/gui-handoff.json\n' "$root" "$lane"
