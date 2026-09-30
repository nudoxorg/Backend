#!/bin/sh
# Tracked source for the local .local/devenv/cargo-in convenience helper.
# Install with:
#   install -m 0755 .config/scripts/cargo-in.sh .local/devenv/cargo-in
set -eu

if [ "$#" -lt 2 ]; then
  echo "usage: cargo-in GROUP CARGO-ARGS..." >&2
  exit 64
fi

group="$1"
shift
case "$group" in
  sym|folio|open|qa|lead|index|journey|shell|owner|green) ;;
  *)
    echo "cargo-in: unknown group '$group'" >&2
    exit 64
    ;;
esac

workspace_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$workspace_root" ]; then
  echo "cargo-in: run inside a git worktree" >&2
  exit 72
fi

development="$workspace_root/.local/devenv/development.sh"
if [ ! -f "$development" ]; then
  echo "cargo-in: development shell is not installed for this worktree" >&2
  exit 72
fi
caller_build_dir="${CARGO_BUILD_BUILD_DIR:-}"
caller_target_dir="${CARGO_TARGET_DIR:-}"
caller_build_jobs="${CARGO_BUILD_JOBS:-}"
# shellcheck disable=SC1090
. "$development"

# This is a role root. The Cargo wrapper derives a stamped, leased child from
# it. Leave CARGO_TARGET_DIR unset by default so artifacts stay in this
# worktree; a caller-provided target root is separately worktree-namespaced.
export CARGO_BUILD_BUILD_DIR="${caller_build_dir:-$workspace_root/.local/build/$group}"
if [ -n "$caller_target_dir" ]; then export CARGO_TARGET_DIR="$caller_target_dir"; fi
export CARGO_BUILD_JOBS="${caller_build_jobs:-${CARGO_BUILD_JOBS:-3}}"
exec cargo "$@"
