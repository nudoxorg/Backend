#!/bin/sh
set -eu

temp_worktree="$1"
evidence_dir="$2"
root_dir="$temp_worktree/workspace2"
registry_dir="$root_dir/planes/compiler/crates/nudox-compile-registry"
skeleton_dir="$root_dir/evidence/p5-c0/compiler/skeleton"
patch_file="$root_dir/evidence/p5-c0/compiler/seeded/constant-typescript-lower.patch"
target_dir="$temp_worktree/target-typescript-lower"

mkdir -p "$evidence_dir"
cp "$skeleton_dir/lib.rs" "$registry_dir/src/lib.rs"
cp "$skeleton_dir/dispatch.rs" "$registry_dir/tests/dispatch.rs"
patch -d "$temp_worktree" -p1 --dry-run <"$patch_file"
patch -d "$temp_worktree" -p1 <"$patch_file"

set +e
RUSTC_WRAPPER= CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    --manifest-path "$root_dir/planes/compiler/Cargo.toml" -p nudox-compile-registry \
    --test dispatch typescript_lower_has_exact_typed_operands >"$evidence_dir/typescript-lower-test.txt" 2>&1
test_status="$?"
set -e
printf '%s\n' "$test_status" >"$evidence_dir/typescript-lower-test.status"
test "$test_status" -ne 0
grep -F "typescript_lower_has_exact_typed_operands" "$evidence_dir/typescript-lower-test.txt"
grep -F "assertion \`left == right\` failed" "$evidence_dir/typescript-lower-test.txt"
