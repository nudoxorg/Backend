# Card clang-B-ir1 — extension section schema 2: clang owner cell, override identity cells, dual decode

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `6c5ec427c` or later. The clang lane harness
  extension helper is ALREADY schema-aware (stride 24/28 by the u16 schema cell at offset 4),
  so the v1 stop no longer applies. Verify with `git log --oneline -3`.
- ESCALATED packet authorized by the capability brief: `compiler/ir` shared-crate edits. Every
  consumer of `compiler/ir` is affected by a wire change; the dual-decode law below is the
  backwards-compatibility contract.

## Owned paths

`compiler/ir/**` — semantic_extension_section.rs, extension_pools.rs, lib.rs, tests/, and any
ir module the schema change necessarily touches. Cargo.toml is read-only (no new dependency).

## Forbidden surface

`compiler/driver/**`, `compiler/languages/**`, `compiler/ir-vocabulary/**` except read-only
use, every other lane's test files. The worktree carries other lanes' uncommitted work
(including `compiler/driver/lower.rs`): never stage or commit anything outside the owned paths.

## Public terminal

1. New fragments write extension-section schema 2. The clang plane's schema-2 row carries an
   explicit owner ordinal (the pushed fact the row binds to) and the clang plane gains access to
   a pooled identity list (16-byte `SymbolIdentity`-shaped cells) for foreign override-edge
   target identities. Row width and directory layout follow the existing fixed-record law: the
   geometry is stated once in code, derived into encode and decode, never duplicated.
2. The decoder accepts BOTH schema 1 and schema 2 (`{1,2}` closed set; anything else is the
   existing exact typed version fault). A schema-1 byte fixture committed as a golden validates
   unchanged after the change; its decoded clang rows are byte-identical to today's.
3. Schema-2 rows round-trip: owner ordinals and pooled identity cells decode exactly what was
   encoded; empty lists decode to empty, never dangling.
4. Backwards-compat falsifier: a fragment written by the CURRENT code (schema 1) still opens,
   and its semantic planes render/decode identically, after this change lands.
5. Structural mutation falsifiers survive: duplicate plane codes, wrong row counts, and
   truncated sections still fail with the exact existing fault classes; a schema byte mutated
   to 0 or 3 is rejected with the version fault naming the observed cell.

## Consumers that must stay green (run them; edit none)

- `cargo test -p compiler-ir --offline`
- `cargo test -p compiler-driver --offline --test clang_lane` (the clang lane's harness — its
  extension-row decoder helper reads schema-1 geometry positionally; if schema 2 changes the
  geometry it reads, the helper must keep working for schema-1 fragments and the lane's tests
  must stay green; if the helper needs a schema-2 branch, that is a STOP decision — report it,
  because `compiler/driver/tests/clang_lane.rs` is outside this packet)
- `cargo test -p compiler-driver --offline --test python_render` and
  `cargo test -p compiler-driver --offline --test rust_render_golden` (shared-crate blast
  radius: their goldens must not move)

## Constraints

- The extension section's one wire authority, magic, and geometry header stay file-global.
- No serde, no dyn, no new dependency, no unsafe. Fixed endian records via the existing
  put/get helpers; every bounds failure keeps the exact typed fault.
- The schema constant becomes a closed set (`const SCHEMA_LEGACY: u16 = 1; const SCHEMA: u16 =
  2;`) with the written version chosen once at encode time.
- Tests live in `compiler/ir/tests/` as ordinary owning-crate tests; goldens are committed
  byte fixtures with a provenance comment.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-ir --offline` → all pass including the new dual-decode, round-trip,
   and mutation tests.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 15/15 pass.
3. `cargo test -p compiler-driver --offline --test python_render` and `... --test
   rust_render_golden` → all pass, unedited.
4. `cargo fmt --check` on owned paths; zero new warnings.

## Checkpoint

One commit, explicit paths, message `feat(compiler-ir): admit extension schema 2 with clang
owner and identity cells`. Report: commit sha, the new falsifiers' assertion summaries, the four
command tails, and the exact schema-2 clang row layout (byte offsets).

## Plan closure

Next decision after return: the render packet (B-ir2, struct bodies + goldens) in the same
crate, or the driver-side build_ir member packet (R6) — sequenced by what this card's wire
makes possible.
