# Card clang-B-ir2 — schema-2 clang row gains the pooled identity-list cell

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ current HEAD (`0304784e5` or later; B-ir1 landed at
  `f53c0958a`). Verify with `git log --oneline -3`.

## Owned paths

`compiler/ir/**` (same escalated packet as B-ir1). Cargo.toml read-only.

## Forbidden surface

`compiler/driver/**`, `compiler/languages/**`, other lanes' files. Explicit paths only.

## Context

B-ir1 landed schema 2 with the 28-byte clang row
(`qualifiers | storage | size_bits | align_bits | templates | includes | owner`). The clang
lane's driver packet now needs one more cell: a pooled identity-list ordinal. Schema 2 is
brand-new and unwritten by any released producer, so its geometry is still freezable.

## Public terminal

1. The schema-2 clang row becomes 32 bytes: the landed 28-byte layout PLUS a pooled
   identity-list ordinal at `28..32` (u32; `u32::MAX`-style empty encoding if the existing
   pooled lanes use one — follow the landed pooled-lane convention exactly).
2. The encoder accepts an identity-list lane in the section input (16-byte fixed-width cells,
   one u32 count lane following the existing pool laws); the decoder round-trips it; empty
   lists decode to empty; capacity faults stay exact and typed.
3. Schema 1 decode stays byte-identical; the dual-decode law and all landed falsifiers stay
   green; schema values outside {1,2} still fail with the exact version fault.

## Falsifiers (compiler/ir/tests/)

- Round-trip: schema-2 fragment with one clang row carrying a two-cell identity list decodes
  both cells byte-identically; a row with the empty list decodes empty.
- Geometry: the encoded row length is exactly 32; schema-1 output length stays 24 per row.
- The landed owner-round-trip and version-fault tests stay green unedited.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-ir --offline` → all pass including new.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 15/15 (helper handles stride
   32 only after you update it? NO — that file is forbidden; the helper selects stride by
   schema and currently maps 2→28. If clang_lane goes red because the helper's stride-2 mapping
   is now wrong, STOP and report — that one-line mapping is the lane's to change).
3. `cargo fmt --check` on owned paths (ignore `compiler/ir/lib.rs` if its drift predates you —
   report it); zero new warnings.

## Checkpoint

One commit, explicit paths, message `feat(compiler-ir): pool override identities in the schema-2
clang row`. Report: commit sha, falsifier summaries, command tails, final layout.

## Plan closure

Next decision: the clang lane driver packet wires the pool end-to-end (B-lane re-dispatch), then
the render packet.
