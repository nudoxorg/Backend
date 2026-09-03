# Card L3a — lane defect repairs D1 + D3, plus the D4 source correction

Registered role: nudox_luna_implementer (opencode `luna` subagent).
Baseline: HEAD (868fa009a or newer) with the current working set. Shared
worktree; stage ONLY owned paths.

## Owned paths

- `compiler/driver/lower/typescript.rs` — D1 fix (+ production region only)
- `compiler/driver/lower.rs` — D1/D3 if the cause lands there (build_ir /
  admit ordering)
- `compiler/driver/tests/typescript_lower.rs` — D1 regression falsifier
- `compiler/driver/tests/typescript_render.rs` — un-ignore the three
  missing-declaration tests after D3, and rewrite the `this` source (D4)

## Forbidden

compiler/ir/**, compiler/driver/types/**, compiler/driver/tests/typescript_authority.rs,
sibling lanes, new dependencies, unsafe.

## D1 — computed rows must be topologically backward (corpus-blocking)

FACT (Terra reproduced via L1b-r1's fragment-path golden test): decoding the
golden fixture's fragment WITH checker rows reports
`TypeFacts::ForwardReference { ordinal: 4, position: 2, target: 37 }` —
anonymous row 4's child at position 2 targets row 37, violating the lane's
committed invariant ("anonymous rows first, topological by construction;
every child points backward" — lower.rs admit; the same law is stated on
pass_checker's doc).

MANDATE: find why `intern_computed_tree` (pass_checker/pass_narrowings)
commits a row whose child references a later row (suspect: union/intersection
or object members interning the PARENT before a compound CHILD, or the
narrowing pass interning after the checker pass in an order that breaks
backwardness), fix the INTERN ORDER so children always precede parents, and
add a regression falsifier in typescript_lower.rs:
`computed_rows_stay_topologically_backward` — the golden fragment decodes
type rows with every child target < row ordinal, and
`golden_lowered_facts_match_the_frozen_table`'s fragment-path half un-ignores
and passes.

## D3 — non-exported top-level bindings vanish from the live Ir

FACT (Terra verified from L2-r1): in three render sources, `const h`, `let x`,
`const a` are absent from compile_ir's entity set while other declarations of
the same sources appear; the FRAGMENT path publishes non-exported bindings
(typescript_lower's narrowing tests prove `let wide` lands as a fact).
MANDATE: write a minimal repro first (e.g. `let x = 7;` alone through
compile_ir vs compile) and localize: does the FactSet push the fact (fragment
has it) but build_ir drops/misnames it, or does the render test's name lookup
fail? Fix the production cause. If the facts ARE present and the lookup is
wrong, fix the lookup and say so plainly (the three `#[ignore]`s then un-ignore).

## D4 — the `this` render source is invalid TypeScript (Terra card error)

`function make(): this { return this; }` plus top-level `this` is a
compile-error source (this-types are class-scoped). Rewrite the `this` class
source to a VALID class-scoped form, e.g.:
`class Cell { value = 1; self(): this { return this; } pair(): [this, this] { return [this, this]; } }`
and re-run its golden. If the real checker still exceeds the 60s deadline on
a VALID small source, report that as a separate typed finding (do not hide it
with a skip).

## Falsifiers / gates

1. `cargo test -p compiler-driver --test typescript_lower` green including
   the new D1 falsifier and the un-ignored fragment-path golden table.
2. `cargo test -p compiler-driver --test typescript_render` green with at
   most ZERO deliberate reds (D3 un-ignored, D4 rewritten). If a genuine new
   lane defect surfaces, keep it a documented `#[ignore]` + report.
3. `cargo test -p compiler-driver --test typescript_authority` green.
4. `cargo test -p compiler-languages-typescript` green (1+17+5).
5. Sibling bundle green (python_render rust_render_golden go_image java_image
   csharp_image authority_terminal).
6. `cargo check -p compiler-driver --lib` clean; census
   `cargo check -p compiler-driver --lib --tests 2>&1 | grep "^error" | grep -c "lower/typescript.rs"` = 0.
PATH: `export PATH="/opt/homebrew/bin:$PATH"` first.

## Commit

One checkpoint: `fix(typescript): backward computed rows, live entity completeness`
staging ONLY owned paths (verify git status first; sibling files stay
unstaged).

## Return

commit hash; D1 root cause (exact interning site) + D3 root cause (exact
site) and fixes; D4 outcome; gate tails; deviations; smallest remaining red.
