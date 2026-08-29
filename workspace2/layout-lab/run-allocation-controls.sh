#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
raw="$lab_root/raw/allocation-counter-store-$(rustc -vV | awk '/host:/{print $2}').tsv"
nix shell nixpkgs#clang -c cargo run --manifest-path "$lab_root/Cargo.toml" --bin allocation-control > "$raw"
