# Card clang-B-lane — shared emission lane: identity pools, and member structure in build_ir

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `0304784e5`. The `lower.rs` base contains ONE
  custodial commit (`wip(rust)…`) of the rust lane's in-flight reserved-anchor work: PRESERVE it
  — your changes build on top; never revert or rewrite that commit.

## Owned paths

1. `compiler/driver/lower.rs`
2. `compiler/driver/lower/clang.rs`
3. `compiler/driver/tests/clang_lane.rs`

## Forbidden surface

`compiler/ir/**` (schema 2 and the `ClangFacts` decode are landed; if a required ir API is
missing, STOP and report), `compiler/languages/**`, every other lane's file. Explicit paths
only, never `git add -A`.

## Context (facts, verified)

- `compiler/ir` now writes extension schema 2; the clang row is 28 bytes with an owner ordinal
  at `24..28`; the decoder accepts {1,2}; version faults are exact.
- `ExtensionPoolsLane` (constructed in `admit`, consumed by ir's encoder) currently carries
  `type_parameters`, `atom_lists`, `type_lists`, `entity_lists` — no identity pool.
- `build_ir` (driver) constructs `TreeItemInput` with `parent: None, members: &[]` for every
  fact — no lane populates Ir structure today.
- The clang projection emits records/enums/namespaces in pass one and their fields in pass two,
  so every member's fact ordinal is strictly greater than its owner's.

## Public terminal

1. IDENTITY POOL (finishes R5): the clang extension row's schema-2 geometry gains a pooled
   identity-list ordinal cell — wait: the schema-2 row is landed at 28 bytes WITHOUT an
   identity cell; adding one more cell changes the landed geometry. Therefore: extend the
   schema-2 clang row to 32 bytes with the identity-list ordinal at `28..32` (schema 2 is
   brand-new and unwritten by any released producer; its geometry is still this capability's to
   freeze), keep schema-1 decode intact, keep every landed falsifier green, and wire the pool
   end-to-end: the clang projection pushes the foreign override target identities it currently
   only counts, and the emitted schema-2 rows name the pool. The identity pool follows the
   existing pooled-lane law: one u32 count, fixed-width 16-byte cells, exact typed capacity
   fault at the lane bound, and the projection's row references stay pooled ordinals.
2. MEMBER STRUCTURE (R6): `build_ir` populates `parent` and `members` from the clang facts'
   owner relations: each record/enum/namespace fact names its member facts as an ordered
   `EntityListId`, and each member names its owner as parent. The lane's emission order
   guarantees owners precede members; a member whose owner is absent keeps `parent: None` (the
   existing honest default) — never a fabricated parent. Member lists come from the facts'
   authority `owner` identities, not from name matching.
3. Falsifiers in `clang_lane.rs`:
   - a C fixture with a record of two fields decodes an Ir (through the public compile path
     already used by the harness) whose record item carries exactly those two members in
     emission order, each naming the record as parent;
   - mutual-recursion fixture: members and parents survive forward-declaration collapsing;
   - foreign override fixture: the schema-2 clang row of the overriding method names the pooled
     identity list whose single cell equals the base's authority identity bytes; a fixture with
     no foreign overrides references an empty list;
   - schema-1 byte compatibility: a committed schema-1 golden fragment still decodes (the
     existing schema cell guard).

## Constraints

- Bounded lanes: new pools get fixed capacities with exact typed faults; no allocation growth
  per fact beyond the existing lane patterns; no new dependency; no unsafe; no macro.
- The custodial rust commit's `intern_reserved_anchor_type_row` and remap behavior must keep
  passing the rust lane's own gates: run `cargo test -p compiler-driver --offline --test
  rust_semantic_lane` — report its result; a red there that predates your change is a report
  item, not yours to fix.
- Formatting clean; zero new warnings in owned files.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lane` → all pass including new.
2. `cargo test -p compiler-ir --offline` → all pass (geometry change accepted by the landed
   dual-decode tests, updated in this card's owned paths? NO — ir tests are outside owned
   paths; if the landed ir tests pin the 28-byte geometry, STOP and report the exact test).
3. `cargo test -p compiler-driver --offline --test rust_semantic_lane` → report only.
4. `cargo fmt --check` on owned files; zero new warnings.

## Checkpoint

One commit, explicit paths, message `feat(compiler): wire override identity pools and member
structure through the shared emission lane`. Report: commit sha, each falsifier's assertion
summary, the four command tails, the final schema-2 clang row layout, smallest remaining red.

## Plan closure

Next decision after return: the render packet (struct bodies + goldens, compiler/ir render.rs),
then the lifecycle packet (PURL, build systems, publish→reopen→index).
