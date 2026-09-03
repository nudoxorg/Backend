# Card L4a: honest computed cells in the live Ir; kill the D1 ignore; R4 doc renames

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: canonical 58634ef11 + trunk narrowings landing in owned paths
  (frozen per-file hashes in index.toml [baseline.files])
owned paths (no other file may be edited):
  - compiler/driver/lower.rs            (ONLY the build_ir TypeScript surface, lines ~891-995, plus helper functions it alone uses)
  - compiler/driver/lower/typescript.rs (ONLY doc-comment text at sites ~923, ~1101, ~2112, ~2823)
  - compiler/languages/typescript/authority.rs (ONLY the doc comment at ~83)
  - compiler/driver/tests/typescript_lower.rs
forbidden adjacent surface: compiler/ir/** (wire validator is other-owner; never relax it),
  compiler/languages/typescript/checker.rs, main.cjs, lib.rs (L4b owns them),
  every non-TypeScript surface of lower.rs, any sibling lane's files.

## Public terminal

`cargo test -p compiler-driver --test typescript_lower` is fully green with ZERO ignored tests:
`golden_lowered_facts_match_the_frozen_table` loses its `#[ignore]` and passes both its live-Ir
loop and its fragment loop.

## Root cause (verified by Terra; do not trust, verify)

lower.rs build_ir currently fabricates the live computed cell for every TypeScript declaration
(lines ~945-973): `ComputedType::KeyOf(TypeId::new(0))` for a Str primitive record, else
`ComputedType::This`. That is why row `n` decodes `(Primitive, SelfType, 0)` where source truth is
a string literal/primitive. `live_type()` in the same file ALREADY translates anonymous pool rows
(`index >= ANONYMOUS_ROW_BASE`) into live types via the same record/child machinery used for the
declared plane; the stub simply does not call it.

## Required behavior

1. Replace the stub so the computed cell is `live_type(tree, self, ANONYMOUS_ROW_BASE + target, ...)`
   over the declaration's computed root anonymous row (the `value.computed` cell is a pool ordinal;
   the current code already reads the record to validate it). No fabricated values remain: any tag
   `live_type` maps to `UnknownType::Unsupported` stays that exact honest terminal.
2. Re-derive every `Frozen` table row from SOURCE TRUTH (tests/fixtures/source.ts plus the checker
   transcript the test already decodes), never by copying current output. If a row was authored to
   match the stub, correct it and name the source-truth evidence (the exact source line / checker
   type) in a one-line comment above the row. Both `g` overload rows stay distinct entities.
3. The fragment loop must also pass. If it reports `TypeFacts::ForwardReference`, STOP and report:
   that is a wire-plane fact for Terra, not something you may fix by relaxing the validator or
   reordering assertions.
4. R4 doc renames (comment text only; zero semantic/wire/cell change):
   - typescript.rs ~923 "honest backlog record" -> name the TypeReason vocabulary
     (e.g. "the no-representation record: unknown with TypeReason::NoIrRepresentation and the exact spelling").
   - typescript.rs ~1101 "backs the backlog reason" -> "backs the NoIrRepresentation reason".
   - typescript.rs ~2112 "syntactic degradation" -> name the typed state ("stay at syntactic
     confidence with NoIrRepresentation; their module bytes are not spelled in this source").
   - typescript.rs ~2823 "backlog stays countable" -> "the NoIrRepresentation reason stays countable".
   - authority.rs ~83 "future typed TSZ authority adapter" -> state the current boundary honestly:
     checker-derived types come from the Checker authority (checker.rs), not from `analyze`.

## Proof-matrix rows bound to this card

- R5 (frozen-table half): un-ignored green table = the falsifier against lowering drift.
- R4 secondary: grep receipt.

## Exact focused commands

- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-languages-typescript
- cargo check -p compiler-driver --lib
- grep -n "future\|degrad\|backlog" compiler/driver/lower/typescript.rs compiler/languages/typescript/authority.rs
  (receipt: zero matches outside permitted bounded-subprocess honesty comments; expected: none at all)

## Bounds and stop decisions

- No new dependency, no unsafe, no public signature change, no new generic parameter.
- Allocation law: translation interns into the existing TreeBuilder; no new retained buffers.
- If the honest translation needs a live-grammar node that does not exist (e.g. a computed tag with
  no ConcreteType/ComputedType mapping), keep `UnknownType::Unsupported` for exactly that tag, add
  one table row pinning it, and report the gap. Do not invent grammar.
- Do not edit any other test in the target; do not reorder assertions; do not touch `#[ignore]`
  attributes on other tests (there are none).
- If the shared worktree's compiler-driver lib does not compile due to SIBLING-lane churn
  (csharp/clang/python/rust files), report `EVIDENCE_BLOCKED: sibling churn` with the exact error;
  do not fix sibling files.

## Checkpoint protocol

Commit owned paths only, message prefix `fix(typescript):` per repo style. Return: commit sha,
focused command outputs, the grep receipt, any table row you re-derived with its source-truth
evidence, and the smallest remaining red row.
