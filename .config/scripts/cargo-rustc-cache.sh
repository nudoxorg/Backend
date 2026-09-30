#!/bin/sh
# Shared cache for ordinary third-party library compilations only.
# Cargo worktree crates, build scripts, proc macros, vendored trees, and
# unfamiliar rustc invocations stay private to their caller's build graph.
# sccache hashes rustc inputs/arguments; this gate does not copy Cargo outputs
# or claim provenance for existing build directories.
set -eu

if [ "$#" -lt 2 ]; then
  echo "nudox rustc cache: expected rustc path and compiler arguments" >&2
  exit 64
fi

compiler="$1"
shift
cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
if [ ! -d "$cargo_home" ]; then
  exec "$compiler" "$@"
fi
cargo_home="$(CDPATH= cd -P "$cargo_home" 2>/dev/null && pwd -P || true)"
if [ -z "$cargo_home" ]; then
  exec "$compiler" "$@"
fi

crate_type=""
crate_type_count=0
crate_name=""
source=""
source_count=0
next_value=""
for argument in "$@"; do
  if [ -n "$next_value" ]; then
    case "$next_value" in
      crate-type)
        crate_type="$argument"
        crate_type_count="$((crate_type_count + 1))"
        ;;
      crate-name) crate_name="$argument" ;;
    esac
    next_value=""
    continue
  fi
  case "$argument" in
    --crate-type)
      next_value=crate-type
      ;;
    --crate-type=*)
      crate_type="${argument#*=}"
      crate_type_count="$((crate_type_count + 1))"
      ;;
    --crate-name)
      next_value=crate-name
      ;;
    --crate-name=*)
      crate_name="${argument#*=}"
      ;;
    --out-dir|--edition|--emit|--target|--sysroot|--cfg|--check-cfg|--error-format|--json|-C|-L|-Z|-o)
      next_value=other
      ;;
    *.rs)
      if [ -f "$argument" ]; then
        source="$argument"
        source_count="$((source_count + 1))"
      fi
      ;;
  esac
done

if [ "$crate_type_count" -eq 1 ] \
  && { [ "$crate_type" = lib ] || [ "$crate_type" = rlib ]; } \
  && [ "$source_count" -eq 1 ] \
  && [ "$crate_name" != build_script_build ] \
  && [ -n "$source" ]; then
  case "$source" in
    /*) ;;
    *) source="$PWD/$source" ;;
  esac
  source_dir="${source%/*}"
  source_file="${source##*/}"
  source_dir="$(CDPATH= cd -P "$source_dir" 2>/dev/null && pwd -P || true)"
  source="$source_dir/$source_file"
  case "$source" in
    "$cargo_home"/registry/src/*|"$cargo_home"/git/checkouts/*)
      exec @sccache@ "$compiler" "$@"
      ;;
  esac
fi

exec "$compiler" "$@"
