#!/usr/bin/env bash
# Stage the product and run the CSS acceptance journey against it.
#   run-css-journey.sh [STAGE_DIR] [PROFILE]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
stage=${1:-$(mktemp -d "${TMPDIR:-/tmp}/nudox-css-stage.XXXXXX")}
profile=${2:-debug}
"$here/stage-css-journey.sh" "$stage" "$profile" | tee "$stage.log"
case "$(uname -s)" in
  Darwin) bin="$stage/Nudox.app/Contents/MacOS" ;;
  *) bin="$stage/bin" ;;
esac
cd "$repo"
flags=(--locked --offline)
if [ "$profile" = release ]; then flags+=(--release); fi
# A busy shared disk can stall the owner's durable publication on fsync for minutes;
# NUDOX_CSS_JOURNEY_TMPDIR (for example a tmpfs) keeps the journey's state off it.
TMPDIR="${NUDOX_CSS_JOURNEY_TMPDIR:-${TMPDIR:-/tmp}}" NUDOX_CSS_JOURNEY_BIN="$bin" cargo test "${flags[@]}" -p backend-journeys \
  --no-default-features --features css-journey --test css_impact_journey -- --nocapture --test-threads=1
