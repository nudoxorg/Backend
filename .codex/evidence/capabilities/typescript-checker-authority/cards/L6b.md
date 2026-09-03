# Card L6b: full lattice coverage — structured conditional/mapped/template/literal records, honest renders, TypeReason vocabulary

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: L6a's result commit on top of 309acc8f1 (Terra names the sha at
dispatch; your owned paths' L5 hashes are in index.toml, L6a deltas are in
the dispatch note). Dispatch AFTER L6a lands — same files.

## Why this card exists (evidence, not opinion)

- Render goldens currently PIN `?unsupported` for conditional
  (typescript_render.rs:136), mapped (:146), template-literal (:157), and
  literal unions (:168) — the gap is asserted as truth.
- checker/main.cjs `typeTree` (lines ~114-153) has no conditional, mapped,
  or template-literal branches; such checker types fall to
  `{ kind: 'other', text }`, and checker.rs `TypeTree` has no variants for
  them, so the lane can only emit Unknown/`Other`.
- The IR row grammar already owns the closed terms: `TypeTag` carries
  Conditional, Mapped, TemplateLiteral, IndexedAccess, KeyOf (compiler/ir
  semantic.rs ~339-367) and render.rs renders them (lines ~367-433). The
  lane's own `ComputedType` grammar mirrors them. The gap is entirely in
  lane-owned serialization + lowering.

owned paths:
  - compiler/languages/typescript/checker.rs
  - compiler/languages/typescript/checker/main.cjs
  - compiler/languages/typescript/tests/ (checker_protocol.rs,
    semantic_authority.rs, fixtures/, transcripts/ — you may regenerate the
    golden transcript with the real checker; schema-1 decode compat stays)
  - compiler/driver/lower/typescript.rs
  - compiler/driver/lower.rs (TypeScript surface only)
  - compiler/driver/tests/typescript_render.rs
  - compiler/driver/tests/typescript_lower.rs

forbidden adjacent surface: compiler/ir/**, compiler/vocabulary/**,
compiler/publication/**, server-journal/**, sibling lanes, no new
dependencies, no new `NativeTool`/vocabulary variants.

## Public terminal (matrix row R13 + R4)

A source declaring `type Cond<T> = T extends string ? "s" : "n"`,
`type RO<T> = { readonly [P in K]: T }`-style mapped types, a
template-literal type, and a union of literal types lowers to STRUCTURED
typed records on the declared plane (and on the computed plane wherever the
checker proves such types); renders display the real constructs; no
`?unsupported` remains for any construct the IR grammar can represent.
The four "backlog"/"degradation" doc sites in lower/typescript.rs
(~923, ~1101, ~2112, ~2822) and the ~1343 "fallback" doc carry TypeReason
vocabulary naming exactly what the record is.

## Required behavior

1. **Checker wire grammar (main.cjs + checker.rs).** Serialize, in a closed
   typed form: conditional (check type, extends type, true branch, false
   branch), mapped (modifier add/remove/preserve, key/target), template
   literal (ordered parts: literal text spans and placeholder types).
   Reuse `TypeTree` recursion for operands; depth cap stays. Extend the
   Rust `TypeTree` enum additively — schema-1 transcripts must KEEP
   decoding unchanged (old payload bytes never gain required fields);
   `deny_unknown_fields` behavior must not strand old decoders.
2. **Lowering.** Declared-written occurrences of these constructs intern as
   the matching IR row terms; checker-computed occurrences intern on the
   computed plane exactly like today's compound shapes (children first,
   backward-closed order, minted proofs). `Literal` members on the DECLARED
   plane (the `Literals` union case) intern as real literal records — no
   Unknown fallback for a representable literal.
3. **Renders.** Replace the four `?unsupported` goldens with the real
   renderings and add one decoded-IR falsifier per construct in
   typescript_lower.rs asserting the exact record kind and operands
   (conditional arms, mapped modifier, template parts, literal bases).
   Render text must come from the lanes — do not touch compiler/ir.
4. **Vocabulary (R4).** Rename the four backlog/degradation doc sites and
   the fallback doc to TypeReason-named vocabulary (e.g. the record a
   proven-but-unrepresented construct gets is named by its TypeReason, not
   a backlog). No behavior change in that step; grep over owned production
   files must come back clean for backlog/degradation markers afterward
   (protocol-honesty comments and the `fallback_name` parameter stay
   exempt).
5. **Honest `Other` survives.** Types the checker proves but no closed term
   represents STILL lower to the retained-spelling record with the honest
   TypeReason — this card removes only the dishonest cases where a closed
   term exists.

## Exact focused commands (PATH + NODE_PATH per index.toml environment)

- cargo test -p compiler-driver --test typescript_render
- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-languages-typescript
- cargo test -p compiler-driver --test typescript_authority
- cargo test -p compiler-driver --test typescript_package
- cargo check -p compiler-driver --lib
- grep receipt: backlog/degradation markers over owned production files

## Bounds and stop decisions

- If the declared segment on the wire rejects computed-kind row tags (e.g.
  ordering or segment-membership rules), STOP and report the exact trunk
  surface with a minimal repro — Terra escalates. Do not re-encode lane-
  side to dodge a wire restriction.
- No `unreachable!`/`expect` in trusted projection paths; exhaustive
  matches over the extended enum; rustc custody proves the match arms.
- Rendering text: assert exact goldens (no `contains`), including the
  conditional's rendered arms and the template's parts.
- Schema-1 golden transcript decode must stay green; if you regenerate the
  transcript, the new bytes keep `schemaVersion: 1` only if the format is
  additive-compatible (old bytes decode with the new code) — otherwise bump
  the field HONESTLY and keep the OLD golden decoding in the chained test
  (R5's decode half pins the old bytes).

## Checkpoint protocol

Two checkpoints allowed: (1) wire grammar + lowering + decoded-IR
falsifiers; (2) renders + vocabulary renames + grep receipt. Commit each
after its focused falsifiers, message prefix `feat(typescript):` /
`fix(typescript):`. Return: commit shas, focused command outputs, the grep
receipt, the list of constructs still honestly `Other` (with source-truth
justification each), and the smallest remaining red row.
