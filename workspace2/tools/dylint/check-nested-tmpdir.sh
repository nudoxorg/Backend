#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/../.." && pwd)"
# Reproduce the ancestor-workspace collision from an intentionally nested
# disposable directory beneath the worktree parent.
temporary_parent="$(cd "$project_dir/../.." && pwd)"
temporary_root="$(mktemp -d "$temporary_parent/nudox-dylint-tmpdir.XXXXXX")"
trap 'rm -rf -- "$temporary_root"' EXIT

nested_tmpdir="$temporary_root/nested/tmp"
mkdir -p "$nested_tmpdir" "$temporary_root/cargo-home" "$temporary_root/dylint-drivers" "$temporary_root/target"

TMPDIR="$nested_tmpdir" \
CARGO_HOME="$temporary_root/cargo-home" \
CARGO_TARGET_DIR="$temporary_root/target" \
DYLINT_DRIVER_PATH="$temporary_root/dylint-drivers" \
  "$project_dir/tools/dylint/ui-test.sh"
