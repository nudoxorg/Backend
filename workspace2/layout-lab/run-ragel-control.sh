#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
raw_dir="$lab_root/raw/ragel-control"
mkdir -p "$raw_dir"
report="$lab_root/raw/ragel-control-$(rustc -vV | awk '/host:/{print $2}').tsv"

for model in cursor workflow; do
  for backend in T0 F0 G0; do
    source="$raw_dir/${model}-${backend}.c"
    object="$raw_dir/${model}-${backend}.o"
    nix shell nixpkgs#ragel nixpkgs#clang -c sh -c "ragel -${backend} -o '$source' '$lab_root/ragel/${model}.rl'; cc -O3 -c '$source' -o '$object'"
  done
done
nix shell nixpkgs#clang -c cargo build --manifest-path "$lab_root/Cargo.toml" --release --bin ragel-handwritten
{
  printf 'format\tnudox-layout-ragel-control-v1\n'
  printf 'current_colm_ragel_rust_backend\tunavailable_in_nix_ragelDev_7.0.0.12_broken\n'
  printf 'ragel6_control\t6.10\tno_Rust_backend;_C_only_control\n'
  printf 'columns\tmodel\tbackend\tsource_bytes\tsource_lines\tnumeric_cs_mentions\tC_text_bytes\n'
  for model in cursor workflow; do
    for backend in T0 F0 G0; do
      source="$raw_dir/${model}-${backend}.c"
      object="$raw_dir/${model}-${backend}.o"
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$model" "$backend" \
        "$(/usr/bin/stat -f '%z' "$source")" "$(wc -l < "$source" | tr -d ' ')" \
        "$(rg -o '\bcs\b' "$source" | wc -l | tr -d ' ')" \
        "$(/usr/bin/size -m "$object" | awk '/Section __text:|Section \(__TEXT, __text\):/ {print $NF; exit}')"
    done
  done
  handwritten="$lab_root/target/release/ragel-handwritten"
  printf 'handwritten\ttyped_enum_match\t%s\t%s\t0\t%s\n' \
    "$(/usr/bin/stat -f '%z' "$lab_root/src/bin/ragel-handwritten.rs")" \
    "$(wc -l < "$lab_root/src/bin/ragel-handwritten.rs" | tr -d ' ')" \
    "$(/usr/bin/size -m "$handwritten" | awk '/Section __text:/ {print $3; exit}')"
} > "$report"
