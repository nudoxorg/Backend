#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output_root="$lab_root/raw"
/bin/mkdir -p "$output_root"

{
    printf 'collector\tstable-size-align-offset-inventory\n'
    printf 'rustc\t%s\n' "$(rustc --version)"
    nix shell nixpkgs#clang -c cargo run --quiet --manifest-path "$lab_root/Cargo.toml"
} > "$output_root/baseline-aarch64-apple-darwin.tsv"
