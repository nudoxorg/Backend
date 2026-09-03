# Card L3a-r1 — revert two unsound L3a changes; honest declared/computed separation

Registered role: nudox_luna_implementer (opencode `luna` subagent).
Baseline: HEAD (2d736582 or newer). Shared worktree; stage ONLY owned paths.

## Terra verdict on L3a (commit 2d736582)

ACCEPTED: the D3 entity-materialization repair in build_ir (entities no
longer vanish), the render lookup corrections (Constant, not Static), and
the D4 valid `this` source.

REJECTED — revert both, they violate the capability's non-negotiable laws:

1. `compiler/driver/lower/typescript.rs` `intern_computed_reference` gained
   `.filter(|fact| *fact < owner)` on the local-fact resolution. That makes
   a computed nominal reference to a SAME-FILE declaration declared AFTER
   the owner (legal TypeScript: `const a = new B(); class B {}`) fall into
   the foreign/npm-universe branch — a silent wrong-origin misattribution
   (rubric blocker B3). REVERT the filter to the exact prior resolution.
2. `compiler/driver/lower.rs` build_ir gained a "computed primitive
   fallback": when a fact has no declared semantic type and its extension
   computed cell is a Primitive, the COMPUTED row substitutes as the fact's
   semantic_type. That conflates the declared plane (what the source wrote)
   with the computed plane (what the checker derived) — the distinction the
   whole checker authority exists to preserve (Origin law). REVERT the
   substitution block. The entity-materialization part of the L3a change
   (entities no longer vanish) must STAY.

## Then make the render assertions honest

The inference/widening render class (and any class that leaned on the
substitution) must assert the honest facts: a declared-Unknown fact renders
its declared honesty AND the computed type is asserted through the
fragment/extension surface where coordinates are authoritative
(`typescript_lower`'s fragment-path helpers or the extension plane rows) —
never by reading the computed cell through the live Ir type lane (D2's
known coordinate-semantics gap, escalated to Sol).

D1 stays as-is: `golden_lowered_facts_match_the_frozen_table` keeps its
`#[ignore = "..."]` (Terra analysis: anonymous computed rows referencing
fact rows are forward by construction in the fragment's anonymous-first
layout; the validator's backwardness law needs segment-aware semantics —
compiler-ir surface, escalated, not lane-repairable). Keep the D1
regression falsifier the L3a worker added, but it must not assert the
unsound filter's behavior — assert instead that a computed reference to an
EARLIER fact resolves locally (the fixture's `made` -> `Box` case), which
is the law that matters to consumers.

## Owned paths

compiler/driver/lower/typescript.rs, compiler/driver/lower.rs,
compiler/driver/tests/typescript_render.rs,
compiler/driver/tests/typescript_lower.rs.

## Gates (PATH exported)

1. `cargo test -p compiler-driver --test typescript_render` green (14).
2. `cargo test -p compiler-driver --test typescript_lower` green with the
   ONE documented D1 ignore and NO new ignores.
3. `cargo test -p compiler-driver --test typescript_authority` green (9).
4. `cargo test -p compiler-languages-typescript` green (1+17+5).
5. Sibling bundle (python_render rust_render_golden go_image java_image
   csharp_image authority_terminal) green.
6. `cargo check -p compiler-driver --lib` clean; census 0.

## Commit

One checkpoint: `fix(typescript): keep computed facts out of the declared plane`
staging ONLY owned paths.

## Return

commit hash; confirmation of both reverts (diff lines); how each render
class asserts computed types now; gate tails; deviations; smallest red row.
