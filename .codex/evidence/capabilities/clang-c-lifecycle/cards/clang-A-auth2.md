# Card clang-A-auth2 — canonical dedupe must prefer the definition sighting

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ current HEAD (A-auth checkpoint `d7e770e47` + journal)
- Owned paths: `compiler/languages/clang/collect.rs`, `compiler/languages/clang/tests/live_authority.rs`

## Finding being repaired

`record_declaration`'s canonical-identity dedupe (commit `d7e770e47`) returns early on the first
sighting of a USR. Counterexample (reproduced on this branch):
`struct Node;\nstruct Node { int x; };` yields the record fact as
`kind=Record, definition=Declaration, span=(0,11)` — the forward sighting — losing the
definition state and measured layout. The driver's frozen header law reads: cursors sharing one
libclang USR collapse to one fact **that prefers the definition**, so measured layout facts come
from the complete declaration.

## Required behavior

One fact per canonical USR, and when a later sighting is a definition while the retained fact is
not, the retained fact is replaced by the definition sighting (identity, name, span, definition
state, type_root, virtuality, storage, documentation all from the definition). Member/child
facts of skipped cursors must keep being visited exactly as now.

## Falsifier

Add one live-authority test: fixture `struct Node;\nstruct Node { int x; };` must yield the
record fact with `definition == DefinitionState::Definition`, name `Node`, and (if measured)
`type_root` from the complete declaration; exactly one record fact; the Field `x` still present.
All existing crate tests stay green (the A-auth falsifiers included).

## Constraints

Same as clang-A-auth: libclang only, frozen `clang_3_6` surface (no new symbols/feature flags),
no source scanning, bounded scratch lanes, explicit paths only, no `git add -A`. Only the two
owned files may change.

## Evidence

1. `cargo test -p compiler-languages-clang --offline --features native-test` → all green
   including the new falsifier.
2. `cargo test -p compiler-driver --offline --test clang_lane 2>&1 | tail -5` → still
   5 passed / 7 failed (no new red, no repaired red expected).
3. `cargo fmt --check` on owned files; zero new warnings.

## Checkpoint

One commit, message `fix(clang): collapse canonical declarations preferring the definition`.
Report commit sha, the falsifier body, both command tails.

## Plan closure

Next decision: re-dispatch card clang-A2-repair on top of the repaired authority.
