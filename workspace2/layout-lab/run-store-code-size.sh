#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
target_dir="$lab_root/target/release"
raw="$lab_root/raw/store-code-size-$(rustc -vV | awk '/host:/{print $2}').tsv"

nix shell nixpkgs#clang -c cargo build --manifest-path "$lab_root/Cargo.toml" --release \
  --bin store-size-00 --bin store-size-12 --bin store-size-default --bin store-size-remote
{
  printf 'format\tnudox-layout-store-code-size-v1\n'
  printf 'columns\tbinary\tfile_bytes\tmach_text_bytes\n'
  for binary in store-size-00 store-size-12 store-size-default store-size-remote; do
    bytes=$(/usr/bin/stat -f '%z' "$target_dir/$binary")
    text=$(/usr/bin/size -m "$target_dir/$binary" | awk '/Section __text:/ { print $3; exit }')
    printf '%s\t%s\t%s\n' "$binary" "$bytes" "$text"
  done
} > "$raw"
