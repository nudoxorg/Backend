#!/bin/sh
set -eu

lab_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output_root="$lab_root/raw"
target_root="$lab_root/target/runtime-layout"
/bin/mkdir -p "$output_root"

{
    printf 'collector\tllvm-debug-layout-discovery\n'
    printf 'rustc\t%s\n' "$(rustc --version)"
    printf 'command\tRUSTFLAGS=-C debuginfo=2 --emit=llvm-ir cargo test -p nudox-runtime --lib --no-run\n'
    RUSTFLAGS='-C debuginfo=2 --emit=llvm-ir' CARGO_TARGET_DIR="$target_root" \
        nix shell nixpkgs#clang -c cargo test --manifest-path "$lab_root/../Cargo.toml" \
            -p nudox-runtime --lib --no-run
    layout_ir=$(rg --files "$target_root/debug/deps" -g 'nudox_runtime-*.ll' | /usr/bin/head -n 1)
    printf 'layout_ir\t%s\n' "$layout_ir"
    rg -n '^%.*(waiter::WaiterSlot|waiter::WaiterStateCore|atomic_waker::AtomicWaker)' "$layout_ir"
    rg -n 'name: "(Fabric<u8, nudox_runtime::runtime_tests::Work>|Runtime<u8, nudox_runtime::runtime_tests::Work>|WaiterSlot|AtomicWaker|WaiterRegistry)"' "$layout_ir"
    rg -n -A 18 'name: "WaiterSlot"' "$layout_ir"
    rg -n -A 8 'name: "AtomicWaker"' "$layout_ir"
    rg -n -A 164 'name: "Fabric<u8, nudox_runtime::runtime_tests::Work>"' "$layout_ir"
    rg -n -A 4 'name: "Runtime<u8, nudox_runtime::runtime_tests::Work>"' "$layout_ir"
} > "$output_root/runtime-private-layout-aarch64-apple-darwin.txt"
