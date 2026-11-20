#!/usr/bin/env bash
# Stage the real owner, CLI, MCP server and Stylo worker the way the product
# lays them out, with the owner built against the worker's final bytes.
#
#   stage-css-journey.sh STAGE_DIR [PROFILE]
#
# PROFILE is a cargo profile directory name: debug (default) or release.
# Output layout:
#   Linux:  STAGE_DIR/bin/{backend-locald,backend-cli,backend-mcp,backend-stylo-worker}
#           STAGE_DIR/lib/libgcc_s.so.1   (the worker's one runtime dependency)
#   macOS:  STAGE_DIR/Nudox.app/Contents/MacOS/{backend-locald,backend-cli,backend-mcp,backend-stylo-worker}
#
# The owner embeds the worker identity at compile time (NUDOX_BUILD_STYLO_WORKER_ID),
# so the worker is built and measured first. Nothing here signs anything: on macOS
# the binaries carry only the linker's ad hoc signature, which is why the journey
# is development-mode evidence and not a release proof. Prints the PREFIX to export.
set -euo pipefail

stage=${1:?usage: stage-css-journey.sh STAGE_DIR [debug|release]}
profile=${2:-debug}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
cd "$repo"

flags=(--locked --offline)
if [ "$profile" = release ]; then flags+=(--release); fi
target=${CARGO_TARGET_DIR:-$repo/target}
out="$target/$profile"

# 1. The worker, built alone so no other crate's feature set changes its bytes.
cargo build "${flags[@]}" -p backend-frontend-stylo --bin backend-stylo-worker
worker="$out/backend-stylo-worker"

# 2. Lay out the closure and measure it.
case "$(uname -s)" in
  Linux)
    bin="$stage/bin"; lib="$stage/lib"
    rm -rf "$stage"; mkdir -p "$bin" "$lib"
    cp "$worker" "$bin/backend-stylo-worker"
    libgcc=$(ldd "$bin/backend-stylo-worker" | awk '/libgcc_s\.so\.1/ {print $3; exit}')
    if [ -z "${libgcc:-}" ] || [ ! -f "$libgcc" ]; then
      echo "stage-css-journey: the worker has no libgcc_s.so.1 dependency to stage" >&2; exit 2
    fi
    cp -L "$libgcc" "$lib/libgcc_s.so.1"
    chmod 0600 "$lib/libgcc_s.so.1"
    cargo build "${flags[@]}" -p backend-compile --example stylo_identity_observer
    observed=$("$out/examples/stylo_identity_observer" "$bin/backend-stylo-worker" "$lib/libgcc_s.so.1")
    ;;
  Darwin)
    bin="$stage/Nudox.app/Contents/MacOS"
    rm -rf "$stage"; mkdir -p "$bin"
    cp "$worker" "$bin/backend-stylo-worker"
    cargo build "${flags[@]}" -p backend-compile --example macos_stylo_identity_observer
    observed=$("$out/examples/macos_stylo_identity_observer" "$bin/backend-stylo-worker")
    ;;
  *) echo "stage-css-journey: unsupported platform $(uname -s)" >&2; exit 2 ;;
esac
anchor=$(printf '%s' "$observed" | sed -n 's/.*"identity":"\([0-9a-f]\{64\}\)".*/\1/p')
if [ -z "$anchor" ]; then echo "stage-css-journey: no worker identity in: $observed" >&2; exit 2; fi
echo "worker anchor $anchor"

# 3. The owner, CLI and MCP server, built with that anchor.
NUDOX_BUILD_STYLO_WORKER_ID=$anchor \
  cargo build "${flags[@]}" -p backend-locald -p backend-cli -p backend-mcp
for name in backend-locald backend-cli backend-mcp; do
  cp "$out/$name" "$bin/$name"
done
# The worker must still be the measured bytes.
case "$(uname -s)" in
  Linux)  recheck=$("$out/examples/stylo_identity_observer" "$bin/backend-stylo-worker" "$lib/libgcc_s.so.1") ;;
  Darwin) recheck=$("$out/examples/macos_stylo_identity_observer" "$bin/backend-stylo-worker") ;;
esac
[ "$recheck" = "$observed" ] || { echo "stage-css-journey: worker bytes changed during staging" >&2; exit 2; }
echo "NUDOX_CSS_JOURNEY_BIN=$bin"
