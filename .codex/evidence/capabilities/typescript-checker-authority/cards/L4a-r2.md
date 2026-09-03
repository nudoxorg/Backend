# Card L4a-r2: honest live computed cells (narrower re-issue of L4a); kill the D1 ignore; R4 doc renames

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: canonical 53637f54 (L4b's 870afdf39 is already integrated on top)
owned paths (no other file may be edited):
  - compiler/driver/lower.rs            (ONLY the build_ir TypeScript surface, lines ~891-995, plus helpers only it uses)
  - compiler/driver/lower/typescript.rs (ONLY doc-comment text at sites ~923, ~1101, ~2112, ~2823)
  - compiler/languages/typescript/authority.rs (ONLY the doc comment at ~83)
  - compiler/driver/tests/typescript_lower.rs
forbidden adjacent surface: compiler/ir/** and compiler/ir-vocabulary/** (other owner),
  checker.rs / main.cjs / lib.rs (L4b owns), every non-TypeScript surface of lower.rs,
  sibling lanes' files.

## What Terra learned from the L4a return (your predecessor's blocker is real)

`TypeScriptFacts::computed` is `Option<TypedTypeId<ComputedState>>` — a COMPUTED-space id — and the
live `ComputedType` grammar (compiler/ir/semantic.rs:1075) has no concrete-resolution node: its
variants are KeyOf/TypeOf/IndexedAccess/Conditional/Mapped/Infer/TemplateLiteral/Import/Awaited/This.
A record whose computed root is a concrete shape (Primitive/Union/Apply/FunctionPointer/Tuple/Array/
Nominal/...) therefore has NO honest live-computed representation. The old stub fabricated
`ComputedType::This` (or `KeyOf(TypeId(0))`) for every row — that fabrication is the D2 defect.

## Required behavior (the honest-subset law)

In build_ir's TypeScript extension rewrite:
1. Computed root record tag SelfType -> `tree.intern_computed(ComputedType::This)`.
2. Computed root record tag Conditional / Mapped / TemplateLiteral -> the corresponding live
   `ComputedType` node, translating the record's operand cells (payload0/payload1/text/children)
   into live TypeIds/AtomIds exactly as the record grammar's admit side wrote them (derive the
   cell positions from lower/typescript.rs's emit sites for those tags — same crate, read them).
3. Every OTHER computed root -> `None`. Honest absence. No fabricated node, no fallback.
   The fragment plane is the computed-cell authority for concrete shapes; the gap is named ESC-2
   ("computed coordinates in live Ir", compiler-ir surface) and is Terra-tracked — you do not fix
   it, document it with one comment citing ESC-2 at the `None` decision point.

## Test law

`typescript_lower.rs`: remove the `#[ignore]` from `golden_lowered_facts_match_the_frozen_table`.
- The FRAGMENT loop asserts the full frozen table (all 20 rows, computed tags + shapes) — the
  fragment is the authority.
- The LIVE loop asserts declared-cell shapes for all rows via the existing `ir_tag_shape` path and
  computed cells ONLY under the honest-subset law: `Some(SelfType-tagged)` where the record is a
  This, the mapped/conditional/template node where the record is one of those three, else `None`.
  One comment above the live computed assertions names ESC-2.
- Audit every OTHER test in the file that reads `extension.computed` through the LIVE plane
  (`golden_this_type_and_local_nominal_computed_rows_bind_by_name`,
  `template_literal_mapped_and_conditional_records_commit_their_tags`, ...): apply the same law.
  Where such a test currently passes ONLY because the stub fabricated nodes, re-point its computed
  assertions at the fragment decode (the existing `view`/`fact` helpers) — never weaken what is
  asserted about the FACTS, only where they are read from.
- Re-derive every `Frozen` table row from SOURCE TRUTH (fixtures/source.ts + the transcript), not
  by copying output; name the evidence in a one-line comment per corrected row. Both `g` overload
  rows stay distinct entities. If the fragment loop reports `TypeFacts::ForwardReference`, STOP and
  report — never relax the validator (compiler-ir is forbidden).

## R4 doc renames (comment text only; zero semantic change)

- typescript.rs ~923 "honest backlog record" -> name the TypeReason vocabulary (e.g. "the
  no-representation record: unknown with TypeReason::NoIrRepresentation and the exact spelling").
- typescript.rs ~1101 "backs the backlog reason" -> "backs the NoIrRepresentation reason".
- typescript.rs ~2112 "syntactic degradation" -> name the typed state ("stay at syntactic
  confidence with NoIrRepresentation; their module bytes are not spelled in this source").
- typescript.rs ~2823 "backlog stays countable" -> "the NoIrRepresentation reason stays countable".
- authority.rs ~83 "future typed TSZ authority adapter" -> state the current boundary: checker
  authority (checker.rs) owns checker-derived types; `analyze` parses and lexically resolves only.

## Exact focused commands

- export PATH="/opt/homebrew/bin:$PATH"
- cargo test -p compiler-driver --test typescript_lower      (green, ZERO ignored)
- cargo test -p compiler-driver --test typescript_render     (green, 14)
- cargo test -p compiler-driver --test typescript_package    (green — regression watch)
- cargo test -p compiler-languages-typescript                (green)
- cargo check -p compiler-driver --lib
- grep -n "future\|degrad\|backlog" compiler/driver/lower/typescript.rs compiler/languages/typescript/authority.rs  (zero matches expected)

## Bounds and stop decisions

- No new dependency, no unsafe, no public signature change, no new generic parameter.
- Translation interns into the existing TreeBuilder; no new retained buffers.
- A mapped/conditional/template record whose operand cells do not translate cleanly is reported,
  not forced: if the record grammar's cell layout cannot be recovered from the emit sites, keep the
  honest `None` for that tag too, pin it in one assertion, and report the exact cell you could not
  recover.
- Sibling-lane churn blocking the driver lib -> `EVIDENCE_BLOCKED: sibling churn` with the exact
  error; do not fix sibling files.

## Checkpoint protocol

Commit owned paths only, message prefix `fix(typescript):`. Return: commit sha, exact tail lines of
each focused command, the grep receipt, corrected table rows with source-truth evidence, and the
smallest remaining red row.
