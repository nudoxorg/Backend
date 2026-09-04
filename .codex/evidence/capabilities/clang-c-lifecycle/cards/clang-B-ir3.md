# Card clang-B-ir3 — ExtensionPoolsLane gains the identity pool the schema-2 row already names

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `31dfeab8e` or later.
- Same escalated compiler/ir packet; owned paths `compiler/ir/**`; Cargo.toml read-only;
  forbidden: `compiler/driver/**`, `compiler/languages/**`, other lanes' files.

## Finding being completed

B-ir2 landed the 32-byte schema-2 clang row whose `28..32` cell is the pooled identity-list
ordinal, but the encoder still writes the hardcoded empty sentinel (`u32::MAX`): the section
input (`ExtensionPoolsLane`, defined in `compiler/ir/extension_pools.rs`) has no identity-list
field, so no producer can populate it. This card completes that plumbing.

## Public terminal

1. `ExtensionPoolsLane` gains an identity-list lane following the existing pooled-lane
   conventions exactly (same field shape, length discipline, payload_len accounting, and
   directory treatment as the landed list lanes).
2. The encoder writes the identity pool payload and emits per-row ordinals into the schema-2
   clang row's `28..32` cell (empty lanes keep the existing sentinel); schema 1 output and
   schema-1 decode stay byte-identical.
3. Round-trip falsifiers: a schema-2 fragment whose clang row names a two-cell identity list
   decodes both cells byte-identically through the public decode path; empty stays empty;
   geometry (32-byte rows) and version-fault falsifiers stay green unedited.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-ir --offline` → all pass including the new round-trip.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 15/15 (the harness helper is
   stride-aware; the lane does not populate the pool yet, so empty-sentinel rows must keep
   decoding).
3. `cargo fmt --check` on owned paths; zero new warnings.

## Checkpoint

One commit, explicit paths, message `feat(compiler-ir): admit identity pools in the section
input`. Report: commit sha, falsifier summaries, command tails.

## Plan closure

Next decision: the clang lane driver packet (B-lane) wires the pool end-to-end and populates
member structure in build_ir.
