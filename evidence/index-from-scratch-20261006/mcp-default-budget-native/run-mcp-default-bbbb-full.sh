#!/bin/bash
set -euo pipefail
source /Users/rmccrar6/nudox-functional-corpus-20261006/sol-fresh-remote-runtime/artifacts/fcb-native-environment.sh
cd /Users/rmccrar6/codex-worktrees/sol61-index-native-test-20261006
test "$(git rev-parse HEAD)" = bbbb06ee62e02ceb6dd6559d72b95508a0ac0f94
test -z "$(git status --porcelain --untracked-files=no)"
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 NUDOX_CARGO_BUILD_SLOTS=6 NUDOX_CARGO_WORKTREE_WAIT_MS=0 NUDOX_CARGO_SLOT_WAIT_MS=0
export CARGO_BUILD_BUILD_DIR="$PWD/.local/build/point-metadata-test" CARGO_TARGET_DIR="$PWD/.local/target"
export SCCACHE_DIR=/Users/rmccrar6/codex-worktrees/sol-fresh-fcb-mac-build-20261006/.local/sccache-fcb-attempt02
unset SCCACHE_SERVER_UDS
ulimit -n 8192
exec "$PWD/.local/fcb-managed-runner/cargo" test --locked --offline -j4 -p backend-mcp --lib -- --nocapture
