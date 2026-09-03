# Card clang-A3-overrides (v2) — C++ virtual override edges become occurrences at code 7

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `bc0fb3110` (A2 checkpoint integrated: 12/12
  `clang_lane` green). Verify with `git log --oneline -3`.
- v2 ruling (Terra, after the v1 lifetime stop): LOCAL override edges are emitted as occurrences.
  FOREIGN override edges (base in an include) have NO honest representation on today's schema-1
  wire — a formatted hex path cannot borrow the source lifetime, and the vocabulary has no
  identity-keyed foreign occurrence — so they are DEFERRED to the authorized schema-2 extension
  packet, which carries them as identity cells. Do not fabricate a path; do not silently drop
  without the record below; the module header must name the deferral explicitly.

## Owned paths

1. `compiler/driver/lower/clang.rs`
2. `compiler/driver/tests/clang_lane.rs`

## Forbidden surface

`compiler/languages/clang/**`, `compiler/driver/lower.rs`, `compiler/ir/**`,
`compiler/ir-vocabulary/**`, every other lane's file. The worktree carries other lanes'
uncommitted work: explicit paths only, never `git add -A`.

## Public terminal

Override edges from the authority's `overrides` plane are emitted as occurrences with
`ReferenceKind::Overrides` (kind byte 7):

1. LOCAL: fixture `struct Base { virtual int f(); };\nstruct Derived : Base { int f() override; };\n`
   (Cxx profile, via `inspect_cxx`) decodes an occurrence with kind `ReferenceKind::Overrides`,
   confidence Oracle, owner = the pushed `Derived::f` fact, target
   `OccurrenceTarget::Local(base-ordinal)` strictly backward, span owner-relative and inside the
   owner's extent.
2. ABSENCE: a fixture with no virtual methods decodes zero kind-7 occurrences.
3. EDGE GUARD: an override edge whose source method is not a pushed declaration emits nothing —
   an occurrence owner must be a pushed declaration.
4. MUTATION FALSIFIER: removing `virtual` from the base (plain shadowing, no override) must
   change the decoded bytes and eliminate the kind-7 occurrence.
5. FOREIGN DEFERRAL RECORD: with a base living in an absolute-path include (write `base.h` into
   the work dir and interpolate its path into the fixture), the derived method still produces NO
   kind-7 occurrence today, and this is not silent: the deferral is recorded in the module
   header next to the other not-provable positions, and the test asserts exactly this interim
   state (zero kind-7 occurrences for the foreign fixture) with a comment naming the schema-2
   packet as the destination.

## Secondary row — dead code cleanup in the owned file

`lower/clang.rs` still warns: `ProjectionFault::Depth`/`Key` never constructed, field
`anonymous_rows` never read, method `types()` never used. Resolve each lawfully: construct the
fault at its real site if the class is live, or delete it if the mechanism is genuinely gone.
Do not delete a fault class whose condition is reachable. Zero warnings from the owned file is
the exit condition.

## Constraints

The module header of `lower/clang.rs` remains the law plus the recorded rulings (emission-order
law; owner ruling; USR-hex foreign keys for override edges). No scanner fallback; typed
capacity terminals; no new allocation owners (the override pass iterates the authority slice);
no new dependency; no unsafe; no wire/schema changes. The harness assertions stay exact; you may
add tests, never weaken one.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lane` → all pass (12 existing + new).
2. `cargo test -p compiler-languages-clang --offline --features native-test` → all pass.
3. `cargo test -p compiler-driver --offline --lib clang 2>&1 | grep "error" | grep -c "lower/clang.rs"`
   → `0`.
4. `cargo fmt --check` on owned files; zero warnings from `lower/clang.rs`.

## Checkpoint

One commit, explicit paths, message `feat(clang): project virtual override edges to occurrence
code 7`. Report: commit sha, each new falsifier's assertion summary, all four command tails, the
dead-code resolutions, smallest remaining red.

## Plan closure

Next decision after return: the escalated rendering packet (extension-section schema 2 + owner
cell, build_ir member population, `render.rs` struct bodies, golden tests), or one
falsifier-bound repair card.
