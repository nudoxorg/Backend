# Card clang-A2-repair (v4) — bring the clang lane projection onto the frozen emission-order contract

- Registered role: `nudox_luna_implementer` (this environment: `luna` subagent, effort max)
- Baseline: worktree `.local/worktrees/clang-lifecycle`, branch `luna/clang-lifecycle` @ `092e21bbc`
- v4 changes from v3: the harness itself was corrected (Terra) — the call-span assertion now
  locates the call site and asserts the OWNER-RELATIVE span (`092e21bbc`), and the template
  fixture runs under `LanguageProfile::Cxx(CxxStandard::Cxx23)` because templates do not exist
  in C. The known-red set is the six tests named below.

## Ruled emission-order law (Terra; binds this card; also verify the module header matches)

- Pass one pushes NAMED declarations in source order: named records, enums (each immediately
  followed by its own enumerators), typedefs/aliases, templates, namespaces, and functions
  (each preceded by its parameter carriers and non-void result carrier, per the function law).
  Rationale: enumerators reference nothing, so they break no type cycle; fields do.
- Pass two pushes record fields in source order, breaking record↔field type cycles so every
  nominal target is strictly backward.
- A variable/static whose type anchors an anonymous record flows through the reserved-anchor
  mechanism: the anonymous row reserves the variable's ordinal, the anonymous record's field
  facts are pushed next, then the variable fact. This is why an anonymous record's fields
  precede the variable that anchors them.

## Owned paths — the ONLY files this card may edit

1. `compiler/driver/lower/clang.rs`
2. `compiler/driver/tests/clang_lane.rs`

## Forbidden surface

`compiler/languages/clang/**` (authority is now correct and frozen at its current state),
`compiler/driver/lower.rs`, `compiler/ir/**`, `compiler/ir-vocabulary/**`, every other lane's
file. **The worktree carries other lanes' uncommitted work** (rust/python/typescript/csharp,
including `compiler/driver/lower.rs`). NEVER stage or commit any file outside the two owned
paths; commit with explicit paths only, never `git add -A` / `git add .`.

## Public terminal

Every test in `compiler-driver`'s `clang_lane` integration target passes — the six currently
red ones (`mutual_recursion_collapses_forwards_and_names_pointer_children`,
`enumerators_and_typedef_have_content_addressed_rows`,
`template_parameter_is_pooled_and_field_is_typevar`,
`include_atoms_share_one_extension_pool_list`, `doxygen_ref_is_a_local_link_with_text_fragments`,
`macro_definition_and_invocation_are_typed_facts`) included — with no weakening of any harness
assertion. Two Prepare-stage failures must disappear for the law-abiding reason:
`Documentation { EmptyCell { ordinal: 1, field: "text" } }` (doxygen fixture) and
`TypeFacts { NominalForward { ordinal: 0, target: 1 } }` (mutual-recursion fixture) — not by
swallowing the fault.

## Laws you must preserve (module header of `compiler/driver/lower/clang.rs` is the law)

Two-pass declarations-first emission; strictly backward fact targets; pooled anonymous rows
topologically ordered; no fabricated coordinates; no partial lying rows; occurrences resolve
local/foreign/unresolved at oracle/oracle/index confidence with owner-relative spans; doxygen
text lines plus `@ref`/`\ref` links, never an empty text cell; qualifiers, storage, measured
layout, template parameters, and include spellings travel only in the extension row; exact
typed capacity terminals; no scanner or source-text fallback; no truncation.

Recorded owner ruling (Terra, binds this card): the header sentence "owners are the pushed
declarations the authority names" is satisfied for an authority reference whose `owner` is None
by the innermost pushed declaration whose source span CONTAINS the reference's span; a
reference contained by no pushed declaration has no honest owner and stays unemitted. The
committed harness macro test asserts only kind + confidence, never a fabricated owner.

Authority facts now flowing that the projection must handle lawfully: TemplateParameter
declarations (pool them as the extension row's template parameters; the harness demands `T` is
NOT an entity), record facts with `name: None` (anonymous rows), definition-preferred canonical
collapse.

## Required evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lane` → every test passes; none removed.
2. `cargo test -p compiler-languages-clang --offline --features native-test` → every test passes
   (guards the frozen authority).
3. `cargo test -p compiler-driver --offline --lib clang 2>&1 | grep "error" | grep -c "lower/clang.rs"`
   → `0` (other lanes' test modules keep the lib-test target itself red; not yours to fix).
4. `cargo fmt --check` on the owned files; the owned files compile with zero warnings.

## Constraints

- Keep the harness's assertions exactly as committed unless an assertion contradicts the
  module-header laws; in that case STOP and report (stop decision, not a test edit). You MAY
  add assertions to an owned test only to retain exactness or typed failure detail.
- Anti-cheat law: the repair must make the emission honestly satisfy the laws for the whole
  fault class. Forbidden: fixture-name or fixture-shape special-casing; synthesizing filler
  rows; clamping/rewriting nominal ordinals so a validator stops rejecting; swallowing a typed
  `Prepare` fault to return partial rows as success. If the honest fix would require editing a
  forbidden path, that is the STOP decision.
- No new allocation owners in the projection path; scratch arrays stay fixed-capacity; no new
  dependency; no unsafe; no macro without two real consumers.
- Diagnostics stay typed: do not erase failure detail to make a test pass.

## Checkpoint and commit protocol

One commit on `luna/clang-lifecycle`, explicit paths only, message
`fix(clang): meet the frozen emission-order contract`. Report: commit sha, the seven red
falsifiers' before/after status, tails of the four command outputs, one-sentence mechanism per
repaired test, smallest remaining red.

## Plan closure

The exact next decision after this card returns: accept the checkpoint and freeze card
clang-A3-overrides (emit `ReferenceKind::Overrides` occurrences from the authority's
`OverrideFact` USR pairs with harness falsifiers), or issue one falsifier-bound repair card.
