#!/usr/bin/env bash

# Bound Nixpkgs Rust product hooks before cargoBuildHook expands -j
# "$NIX_BUILD_CORES". Source this from the derivation's preBuild hook.
nudox_cap_cargo_jobs_value() {
  local requested="$1"
  local variable="$2"

  case "$requested" in
    ""|*[!0123456789]*)
      printf 'nudox cargo: invalid %s; refusing an unbounded build\n' "$variable" >&2
      return 64
      ;;
  esac

  # Strip leading zeroes as text so oversized input cannot overflow shell
  # arithmetic. Nix uses zero to request all available cores, so it is capped.
  while [[ "$requested" == 0* && "$requested" != 0 ]]; do
    requested="${requested#0}"
  done

  case "$requested" in
    0) NUDOX_CAPPED_CARGO_JOBS=4 ;;
    1|2|3|4) NUDOX_CAPPED_CARGO_JOBS="$requested" ;;
    *) NUDOX_CAPPED_CARGO_JOBS=4 ;;
  esac
}

nudox_cap_nix_build_cores() {
  local requested_nix_cores="${NIX_BUILD_CORES-}"
  local requested_cargo_jobs="${CARGO_BUILD_JOBS-${NIX_BUILD_CORES-}}"
  local NUDOX_CAPPED_CARGO_JOBS
  local capped_nix_cores
  local capped_cargo_jobs
  local capped

  nudox_cap_cargo_jobs_value "$requested_nix_cores" NIX_BUILD_CORES || return $?
  capped_nix_cores="$NUDOX_CAPPED_CARGO_JOBS"
  nudox_cap_cargo_jobs_value "$requested_cargo_jobs" CARGO_BUILD_JOBS || return $?
  capped_cargo_jobs="$NUDOX_CAPPED_CARGO_JOBS"

  # Keep any caller-specified smaller Cargo bound as well as Nix's smaller
  # per-derivation request; the Cargo hook uses an explicit -j flag.
  if [[ "$capped_cargo_jobs" -lt "$capped_nix_cores" ]]; then
    capped="$capped_cargo_jobs"
  else
    capped="$capped_nix_cores"
  fi

  if [[ "$requested_nix_cores" != "$capped" || "$requested_cargo_jobs" != "$capped" ]]; then
    printf 'nudox cargo: bounded product Cargo jobs to %s\n' "$capped" >&2
  fi
  export NIX_BUILD_CORES="$capped" CARGO_BUILD_JOBS="$capped"
}
