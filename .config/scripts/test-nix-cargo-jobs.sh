#!/usr/bin/env bash
set -euo pipefail

helper="${1:?usage: nix-cargo-jobs.sh PATH-TO-JOB-CAP-HELPER}"

expect_cap() {
  local input="$1"
  local expected="$2"
  local cargo_input="${3-__unset__}"
  local actual

  actual="$(
    NIX_BUILD_CORES="$input" "$BASH" -c '
      source "$1"
      if [[ "$2" == __unset__ ]]; then
        unset CARGO_BUILD_JOBS
      else
        CARGO_BUILD_JOBS="$2"
      fi
      nudox_cap_nix_build_cores || exit $?
      printf "%s|%s" "$NIX_BUILD_CORES" "$CARGO_BUILD_JOBS"
    ' _ "$helper" "$cargo_input" 2>/dev/null
  )"
  if [[ "$actual" != "$expected|$expected" ]]; then
    printf 'NIX_BUILD_CORES=%s CARGO_BUILD_JOBS=%s produced %s; expected %s|%s\n' \
      "$input" "$cargo_input" "$actual" "$expected" "$expected" >&2
    exit 1
  fi
}

expect_rejected() {
  local input="$1"
  if NIX_BUILD_CORES="$input" "$BASH" -c '
    source "$1"
    unset CARGO_BUILD_JOBS
    nudox_cap_nix_build_cores
  ' _ "$helper" >/dev/null 2>&1; then
    printf 'malformed NIX_BUILD_CORES=%s was accepted\n' "$input" >&2
    exit 1
  fi
}

expect_cap 1 1
expect_cap 2 2
expect_cap 3 3
expect_cap 4 4
expect_cap 20 4
expect_cap 0 4
expect_cap 0002 2
expect_cap 0000 4
expect_cap 999999999999999999999999999999 4
expect_cap 4 2 2
expect_cap 2 2 4
expect_cap 20 1 1
expect_cap 8 4 20
expect_rejected ''
expect_rejected malformed
expect_rejected -2
if "$BASH" -c '
  source "$1"
  unset NIX_BUILD_CORES CARGO_BUILD_JOBS
  nudox_cap_nix_build_cores
' _ "$helper" >/dev/null 2>&1; then
  printf '%s\n' 'unset NIX_BUILD_CORES was accepted' >&2
  exit 1
fi
if NIX_BUILD_CORES=4 "$BASH" -c '
  source "$1"
  CARGO_BUILD_JOBS=malformed
  nudox_cap_nix_build_cores
' _ "$helper" >/dev/null 2>&1; then
  printf '%s\n' 'malformed CARGO_BUILD_JOBS was accepted' >&2
  exit 1
fi

printf '%s\n' 'validated 18 NIX_BUILD_CORES/CARGO_BUILD_JOBS cases'
