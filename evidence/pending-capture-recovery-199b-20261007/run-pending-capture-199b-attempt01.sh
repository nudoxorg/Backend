#!/bin/bash
set -euo pipefail
umask 077
source /Users/rmccrar6/nudox-functional-corpus-20261006/sol-fresh-remote-runtime/artifacts/fcb-native-environment.sh
cd /Users/rmccrar6/codex-worktrees/sol61-pending-capture-recovery-20261007
test "$(git rev-parse HEAD)" = 199bef18b011e41493a3a973f8765015b29ab412
test -z "$(git status --porcelain --untracked-files=no)"
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 NUDOX_CARGO_BUILD_SLOTS=6 NUDOX_CARGO_WORKTREE_WAIT_MS=0 NUDOX_CARGO_SLOT_WAIT_MS=0
export CARGO_BUILD_BUILD_DIR="$PWD/.local/build/pending-capture-native" CARGO_TARGET_DIR="$PWD/.local/target"
export SCCACHE_DIR=/Users/rmccrar6/codex-worktrees/sol-fresh-fcb-mac-build-20261006/.local/sccache-fcb-attempt02
unset SCCACHE_SERVER_UDS
ulimit -n 8192
exec "$PWD/.local/fcb-managed-runner/cargo" test --locked --offline -j4 -p backend-local-service --lib capture_recovery_ -- --nocapture --test-threads=1
