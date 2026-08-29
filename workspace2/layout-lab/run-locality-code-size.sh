#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
target_dir="$lab_root/target/release"
raw="$lab_root/raw/locality-code-size-$(rustc -vV | awk '/host:/{print $2}').tsv"

nix shell nixpkgs#clang -c cargo build --manifest-path "$lab_root/Cargo.toml" --release \
  --bin locality-size-current --bin locality-size-fused --bin locality-size-split16 \
  --bin locality-size-packed13 --bin locality-size-packedowner
{
  printf 'format\tnudox-layout-locality-code-size-v1\n'
  printf 'columns\tbinary\tfile_bytes\tmach_text_bytes\n'
  for binary in locality-size-current locality-size-fused locality-size-split16 locality-size-packed13 locality-size-packedowner; do
    bytes=$(/usr/bin/stat -f '%z' "$target_dir/$binary")
    text=$(/usr/bin/size -m "$target_dir/$binary" | awk '/Section __text:/ { print $3; exit }')
    printf '%s\t%s\t%s\n' "$binary" "$bytes" "$text"
  done
} > "$raw"
