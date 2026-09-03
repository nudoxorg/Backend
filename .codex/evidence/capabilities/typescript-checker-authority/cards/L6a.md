# Card L6a: computed trees end-to-end — forward nominals, D1 kill, geometry truth

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: 309acc8f1 (L5 freeze; see index.toml per-file table). Trunk's
schema-2 computed segment (dab8c366e), computed-segment reference path
(fcaeaa894), and computed-cell reconstruction (0dc2fce43) are IN this
baseline. Forward computed nominals are admissible on the wire; the lane
has not caught up.

owned paths (no other file may be edited):
  - compiler/driver/lower/typescript.rs
  - compiler/driver/lower.rs (TypeScript surface only)
  - compiler/driver/tests/typescript_lower.rs
  - compiler/languages/typescript/tests/fixtures/source.ts ONLY IF the
    golden transcript is regenerated with the real checker AND the pinned
    transcript sha256 in typescript_lower.rs / checker_protocol receipts is
    updated in the same commit; otherwise untouched (prefer NOT touching it)

forbidden adjacent surface: compiler/ir/** , compiler/publication/**,
server-journal/**, compiler/languages/typescript/checker.rs and main.cjs
(they are L6b's), sibling lanes' files, typescript_render.rs,
typescript_purl_lifecycle.rs / typescript_support/ (L4c-r1 is landing in
parallel — do not create or edit those paths), no new dependencies.

## Public terminal (matrix rows R12 + R5 + R1)

`const a = new B(); class B {}` travels end-to-end green: the checker
reports `a`'s computed nominal pointing at the LATER-declared `B`; the lane
lowers it; the fragment wire carries it; publish -> reopen -> decode
returns the same computed cell; the render names `B`. The frozen golden
table test passes BOTH halves un-ignored. The pool-bound typed-rejection
falsifier exercises the CURRENT geometry.

## Required behavior

1. **D1 un-ignored and green.** `golden_lowered_facts_match_the_frozen_table`
   currently fails at row 0 (`n`, source truth `number`) with computed
   (Nominal, 1) on the in-memory path. Diagnose the true mechanism. Then:
   before un-ignoring, RE-VERIFY every frozen-table row against source
   truth (source.ts + golden.json transcript + the checker's own printed
   types). Where a row's expectation contradicts source truth, fix the
   TABLE and say so in the commit message; where the lowering contradicts a
   source-true row, fix the LOWERING. Do not tune either side to make the
   other pass without the source-truth receipt in the commit message.
2. **Forward-nominal journey (R12).** New tests in typescript_lower.rs
   using an INLINE source (`const` bytes, do not edit source.ts):
   `export const made = new Box().self(); export class Box { self(): this { return this; } }`
   ordering variant where the class FOLLOWS the use, e.g.
   `const a = new B(); class B {}` (adapt to the exported-form the public
   pipeline accepts; keep the use-before-declaration shape). Falsifiers:
   (a) the real `Checker` (not a hand-built report) reports `a`'s computed
   type referencing `B`; (b) `compile_ir` decodes `a`'s computed cell as
   the nominal `B` — not Unknown, not another owner's row; (c) `compile` ->
   `publish_compiled` into a fresh `DurablePublisher` -> `DurablePublisher::reopen`
   -> `open_published` -> the decoded fragment asserts the same computed
   cell (mirror python_purl_lifecycle.rs's choreography inline; keep it
   minimal, no shared module); (d) a render display for `a` names `B`.
3. **Geometry-truth falsifier (R1).**
   `computed_row_pool_bound_and_union_child_bound_are_typed_rejections`
   must again prove its law at the CURRENT geometry: derive the live bound
   from the emission constants (name them in the test via an existing
   public/`pub(crate)` constant or a size-of-derived fact — no magic
   number copied by value), assert the typed rejection at that bound, and
   assert one bound-minus-one case still LOWERS.
4. **No lane-order fabrication.** Whatever mechanism you find, the repair
   must keep the mint/validation laws: a computed cell is never fabricated
   to satisfy an ordering accident; a mis-ordered row is a typed rejection
   (or an internally re-derived honest order), never a silent swap.

## Exact focused commands (run with PATH=/opt/homebrew/bin:$PATH and
NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules; worktree
/private/tmp/nudox-fidelity-typescript)

- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-languages-typescript
- cargo check -p compiler-driver --lib
- cargo test -p compiler-driver --test typescript_authority (regression watch)
- cargo test -p compiler-driver --test typescript_package (regression watch)
- cargo test -p compiler-driver --test typescript_render (regression watch;
  14/14 must stay green — you do not own this file)

## Bounds and stop decisions

- No compiler/ir or publication edits. If the mis-binding's true root cause
  is inside compiler/ir's segment encode/decode/reconstruction or the
  publication path, STOP: leave the lane un-patched, report the exact
  trunk surface with a minimal repro (source + observed vs expected rows),
  and return `ESCALATE` — that is a Terra escalation to Sol, not your
  patch. Do not work around a trunk defect in the lane.
- No new `TypeReason` variants unless a genuinely new provenance class
  appears; no vocabulary changes.
- Keep the `checker` package-run path (R10) intact: `typescript_package`
  must stay green.
- Formatting: rustfmt clean; no statement packing; no `unwrap`/`expect`
  added in production paths; clippy lints denied in tests as today.
- Schema-1 transcript compatibility is a law: if your repair changes what
  the golden transcript decodes INTO, that may only move lane lowering, not
  the transcript wire format (checker.rs/main.cjs are out of your paths).

## Checkpoint protocol

Commit owned paths only, message prefix `fix(typescript):`. Return: commit
sha, focused command outputs, the row-by-row source-truth verdict for the
frozen table (which rows you changed, if any, and why), the diagnosed
mechanism in three sentences, and the smallest remaining red row.
