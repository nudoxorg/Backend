#!/bin/sh
set -eu

cell="$1"
temp_worktree="$2"
evidence_dir="$3"
root_dir="$temp_worktree/workspace2"
registry_dir="$root_dir/planes/compiler/crates/nudox-compile-registry"
skeleton_dir="$root_dir/evidence/p5-c0/compiler/skeleton"
seed_dir="$root_dir/evidence/p5-c0/compiler/seeded"

case "$cell" in
    rust-parse)
        patch_file="$seed_dir/rust-parse-constant.patch"
        test_name="rust_parse_forwards_its_own_pointer_and_length"
        callable="rust_parse"
        ;;
    rust-lower)
        patch_file="$seed_dir/rust-lower-constant.patch"
        test_name="rust_lower_forwards_its_own_pointer_and_length"
        callable="rust_lower"
        ;;
    typescript-parse)
        patch_file="$seed_dir/typescript-parse-constant.patch"
        test_name="typescript_parse_forwards_its_own_pointer_and_length"
        callable="typescript_parse"
        ;;
    *)
        exit 64
        ;;
esac

mkdir -p "$evidence_dir"
mkdir -p "$registry_dir/examples"
cp "$skeleton_dir/lib.rs" "$registry_dir/src/lib.rs"
cp "$skeleton_dir/dispatch.rs" "$registry_dir/tests/dispatch.rs"
cp "$skeleton_dir/subset.rs" "$registry_dir/tests/subset.rs"
cp "$skeleton_dir/release_consumer.rs" "$registry_dir/examples/release_consumer.rs"
patch -d "$temp_worktree" -p1 --dry-run <"$patch_file"
patch -d "$temp_worktree" -p1 <"$patch_file"

target_dir="$temp_worktree/target-$cell"
set +e
RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    --manifest-path "$root_dir/planes/compiler/Cargo.toml" -p nudox-compile-registry \
    --test dispatch "$test_name" >"$evidence_dir/$cell-test.txt" 2>&1
test_status="$?"
set -e
printf '%s\n' "$test_status" >"$evidence_dir/$cell-test.status"
test "$test_status" -ne 0
grep -F "$test_name" "$evidence_dir/$cell-test.txt"
grep -F "assertion failed" "$evidence_dir/$cell-test.txt"

set +e
RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo rustc --locked \
    --manifest-path "$root_dir/planes/compiler/Cargo.toml" -p nudox-compile-registry \
    --example release_consumer --release -- --emit=llvm-ir >"$evidence_dir/$cell-codegen.txt" 2>&1
codegen_status="$?"
set -e
printf '%s\n' "$codegen_status" >"$evidence_dir/$cell-codegen.status"
test "$codegen_status" -eq 0
find "$target_dir/release/examples" -type f -name '*.ll' -exec cp {} "$evidence_dir/$callable.ll" \;
