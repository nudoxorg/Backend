#!/bin/sh
set -eu

control_worktree="$1"
candidate_worktree="$2"
evidence_dir="$3"
target_triple="$(rustc -vV | sed -n 's/^host: //p')"
control_root="$control_worktree/workspace2"
candidate_root="$candidate_worktree/workspace2"
control_manifest="$control_root/planes/compiler/Cargo.toml"
candidate_manifest="$candidate_root/planes/compiler/Cargo.toml"
control_registry="$control_root/planes/compiler/crates/nudox-compile-registry"
candidate_registry="$candidate_root/planes/compiler/crates/nudox-compile-registry"
candidate_skeleton="$candidate_root/evidence/p5-c0/compiler/skeleton"
control_target="$control_worktree/target-c0-compiler-control"
candidate_target="$candidate_worktree/target-c0-compiler-candidate"

exact_rlib() {
    pattern="$1"
    set -- $pattern
    test "$#" -eq 1
    test -f "$1"
    printf '%s\n' "$1"
}

build_pair() {
    manifest="$1"
    target_dir="$2"
    RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo clean --manifest-path "$manifest" \
        -p nudox-compile-registry -p nudox-compile-vocab
    test "$(find "$target_dir/$target_triple/release/deps" -maxdepth 1 -type f \
        -name 'libnudox_compile_registry-*.rlib' -print 2>/dev/null | wc -l | tr -d ' ')" = 0
    test "$(find "$target_dir/$target_triple/release/deps" -maxdepth 1 -type f \
        -name 'libnudox_compile_vocab-*.rlib' -print 2>/dev/null | wc -l | tr -d ' ')" = 0
    RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo build --locked --manifest-path "$manifest" \
        -p nudox-compile-registry --release --target "$target_triple"
    test "$(find "$target_dir/$target_triple/release/deps" -maxdepth 1 -type f \
        -name 'libnudox_compile_registry-*.rlib' -print | wc -l | tr -d ' ')" = 1
    test "$(find "$target_dir/$target_triple/release/deps" -maxdepth 1 -type f \
        -name 'libnudox_compile_vocab-*.rlib' -print | wc -l | tr -d ' ')" = 1
}

mkdir -p "$candidate_registry/examples" "$evidence_dir/control/registry" "$evidence_dir/candidate/registry"
cp "$candidate_skeleton/lib.rs" "$candidate_registry/src/lib.rs"
cp "$candidate_skeleton/dispatch.rs" "$candidate_registry/tests/dispatch.rs"
cp "$candidate_skeleton/subset.rs" "$candidate_registry/tests/subset.rs"
cp "$candidate_skeleton/release_consumer.rs" "$candidate_registry/examples/release_consumer.rs"

build_pair "$control_manifest" "$control_target"
build_pair "$candidate_manifest" "$candidate_target"

control_deps="$control_target/$target_triple/release/deps"
candidate_deps="$candidate_target/$target_triple/release/deps"
control_registry_rlib="$(exact_rlib "$control_deps/libnudox_compile_registry-*.rlib")"
control_vocab_rlib="$(exact_rlib "$control_deps/libnudox_compile_vocab-*.rlib")"
candidate_registry_rlib="$(exact_rlib "$candidate_deps/libnudox_compile_registry-*.rlib")"
candidate_vocab_rlib="$(exact_rlib "$candidate_deps/libnudox_compile_vocab-*.rlib")"
consumer_source="$candidate_registry/examples/release_consumer.rs"

rustc "$consumer_source" --crate-name release_consumer --edition 2024 -C opt-level=3 \
    --target "$target_triple" -L "dependency=$control_deps" \
    --extern "nudox_compile_registry=$control_registry_rlib" \
    --extern "nudox_compile_vocab=$control_vocab_rlib" \
    --emit=llvm-ir,asm --out-dir "$evidence_dir/control"
rustc "$consumer_source" --crate-name release_consumer --edition 2024 -C opt-level=3 \
    --target "$target_triple" -L "dependency=$candidate_deps" \
    --extern "nudox_compile_registry=$candidate_registry_rlib" \
    --extern "nudox_compile_vocab=$candidate_vocab_rlib" \
    --emit=llvm-ir,asm --out-dir "$evidence_dir/candidate"
rustc "$control_registry/src/lib.rs" --crate-type lib --crate-name nudox_compile_registry \
    --edition 2024 -C opt-level=3 --target "$target_triple" -L "dependency=$control_deps" \
    --extern "nudox_compile_vocab=$control_vocab_rlib" \
    --emit=llvm-ir,asm --out-dir "$evidence_dir/control/registry"
rustc "$candidate_registry/src/lib.rs" --crate-type lib --crate-name nudox_compile_registry \
    --edition 2024 -C opt-level=3 --target "$target_triple" -L "dependency=$candidate_deps" \
    --extern "nudox_compile_vocab=$candidate_vocab_rlib" \
    --emit=llvm-ir,asm --out-dir "$evidence_dir/candidate/registry"

{
    rustc -Vv
    cargo -V
    printf 'target=%s\n' "$target_triple"
    printf 'consumer=%s\n' "$consumer_source"
    printf 'control-registry=%s\n' "$control_registry_rlib"
    printf 'control-vocab=%s\n' "$control_vocab_rlib"
    printf 'candidate-registry=%s\n' "$candidate_registry_rlib"
    printf 'candidate-vocab=%s\n' "$candidate_vocab_rlib"
    shasum -a 256 "$consumer_source" "$control_registry/src/lib.rs" "$candidate_registry/src/lib.rs" \
        "$control_registry_rlib" "$control_vocab_rlib" \
        "$candidate_registry_rlib" "$candidate_vocab_rlib"
} >"$evidence_dir/custody.txt"
