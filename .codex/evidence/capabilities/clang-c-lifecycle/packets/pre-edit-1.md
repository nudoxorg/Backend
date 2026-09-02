# Pre-edit packet — clang-c-lifecycle (review-id: pre-edit-1)

Rationale-free contract packet. Reviewer: apply review-rust-gem + deliver-reviewed-rust-slice
to the CONTRACT below (brief.md, proof-matrix.md at this capability directory; skeleton and cards
here). No builder rationale exists; no production edit has occurred under this packet.

## Coupling skeleton

| module | invariant owner | public terminal | dependencies | state/control boundary |
|---|---|---|---|---|
| clang/facts.rs | authority fact vocabulary (adds OverrideFact, virtual/pure cells) | pub facts | core | closed enums, fixed-width Copy records |
| clang/ffi.rs | the one reviewed unsafe libclang boundary | pub(crate) | clang-sys 1.9.1 runtime | typed NativeApi load gates; no raw escape |
| clang/collect.rs | traversal into caller-bounded slots | collect / collect_cancellable | ffi | exact capacity/cancel terminals |
| driver/lower/clang.rs | lane projection laws (two-pass, strictly backward targets) | pub(crate) collect | clang facts + shared FactSet | folded lane terminals |
| ir-vocabulary/occurrence.rs | closed reference-kind lattice | pub ReferenceKind | none | additive code 7; duplicate codes stay rejected |
| ir/semantic_extension_section.rs | schema versioned extension rows | encode/reopen | vocabulary | dual schema {1,2} decode |
| ir/semantic.rs + render.rs | Ir structure + rendering | pub render API | vocabulary | members/parents derived from owner cells |
| driver/lower.rs build_ir | shared emission lane (shared file, one writer card) | admit / build_ir | ir | owner-cell plumbing, no behavior change for other lanes |
| clang/lifecycle (new module) | PURL, database location, build-system adapters, whole-TU pipeline | typed lifecycle terminals | collect/ffi, std::process | bounded capture, typed timeouts, cancellation at TU boundaries |
| tests trees | one law per test | gates | all above | no exported fixture APIs |

Resource controls: new fact lane capacities follow the existing bounded-slot pattern with exact
typed overflow errors; process adapters use bounded output capture and typed deadlines; zero new
external crate dependencies; zero new unsafe beyond the reviewed ffi.rs boundary; no SIMD.

## Card 1 — luna / clang-lane-repair (checkpoint A)

- registered role: luna (`.opencode/agents/luna.md`), expected gpt-5.6-luna/max
- baseline: 2c0b26f86 + working tree (digests in brief.md); owned paths: `compiler/driver/lower/clang.rs` ONLY
- goal: restore the clang test module to a compiling state: the test module at the bottom of
  `compiler/driver/lower/clang.rs` references `SourceSpan` and `IncludeFact` without importing
  them (compiler errors at lines ~2574–2619). Add the missing imports to the test module's use
  list. Also delete the two dead let-bindings in `include_spellings_borrow_the_delimited_path`
  / `owner_relative_spans_subtract_and_reject_wrapping` (`let _ = ...` shadow noise) if they
  reference the same names; do not touch any other code.
- proof rows: R1
- falsifier (must fail before, pass after): `cargo test -p compiler-driver --offline --lib clang 2>&1`
  contains `error[E0422]` naming `compiler/driver/lower/clang.rs`; after the fix the only remaining
  diagnostics must name other lanes' files (external red, record verbatim).
- bounds: ≤ 15 changed lines; no production (non-test) edits; no other file.
- stop: report if the fix cannot be made within bounds.
- commit protocol: one commit `fix(compiler-driver): restore clang lane test module imports`,
  staging only the owned file; never `git add -A`.

## Card 2 — luna / clang-overrides-edges (checkpoint B, after A)

- owned paths: `compiler/languages/clang/{facts.rs,scratch.rs,collect.rs,ffi.rs,error.rs,lib.rs}`,
  `compiler/languages/clang/tests/live_authority.rs`, `compiler/driver/lower/clang.rs`,
  `compiler/ir-vocabulary/occurrence.rs`, `compiler/ir-vocabulary/tests/occurrence_vocabulary.rs`,
  `compiler/ir/semantic_facts.rs` (decode arm only), `compiler/ir/semantic_extension_section.rs`
  (validate arm only)
- goal (one vertical): C++ virtual override edges end-to-end.
  1. ffi.rs: bind `clang_CXXMethod_isVirtual`, `clang_CXXMethod_isPureVirtual`,
     `clang_getOverriddenCursors`, `clang_disposeOverriddenCursors` (verified ungated), extend the
     matching RequiredApi family and its `is_loaded` check.
  2. facts.rs: `DeclarationFact` gains `virtual: bool`, `pure_virtual: bool` (independently valid
     authority facts; consumed by the edge law below and by tests). New fixed record
     `OverrideFact { source: SymbolIdentity, target: SymbolIdentity }` — identities, never names.
  3. scratch.rs: new bounded lane `overrides`; error.rs: matching `ScratchLane` variant.
  4. collect.rs: for each declaration cursor that is a C++ method with `isVirtual`, record the
     virtual/pure cells and stream one OverrideFact per overridden cursor identity
     (`clang_getOverriddenCursors` → `clang_getCursorUSR`), disposing the native cursor array
     exactly once on every path.
  5. ir-vocabulary/occurrence.rs: additive `ReferenceKind::Overrides = 7` with the same derivation
     and code round-trip as existing variants; adapt every exhaustive consumer in the SAME
     checkpoint (decode in `ir/semantic_facts.rs`, `validate_fact_encoding` in
     `semantic_extension_section.rs`, vocabulary tests). Duplicate-code rejection stays.
  6. driver/lower/clang.rs: project each OverrideFact into one occurrence owned by the pushed
     derived-method fact: `kind: Overrides`, `confidence: Oracle`, target `Local(base ordinal)`
     when the base identity was pushed in this TU, otherwise the existing foreign c-universe path
     with the written spelling of the derived method's declaration name. No owner, no honest span
     → absent, exactly like the existing occurrence laws.
- must prove (falsifiers R2, R3, R4 in proof-matrix.md; read them before coding)
- bounds: ≤ 380 net production lines across all owned paths (facts/ffi/collect/vocabulary/lane
  combined, tests excluded); every retained allocation documented per the skill ledger; no new
  dependency; no unsafe outside ffi.rs; live authority test requires the `native-test` feature.
- forbidden adjacent surface: other lanes' projection files; `build_ir`; render; publication;
  ClangFacts extension row layout (no schema change in this card); input.rs/parse arguments.
- exact focused commands:
  - `cargo test -p compiler-languages-clang --offline`
  - `cargo test -p compiler-languages-clang --offline --features native-test`
  - `cargo test -p compiler-ir-vocabulary --offline && cargo test -p compiler-ir --offline`
  - `cargo test -p compiler-driver --offline --lib clang 2>&1 | grep -c "lower/clang.rs"` → 0 diagnostics
  - `cargo fmt --check` on owned paths
- commit protocol: two commits — (1) vocabulary additive variant + consumers + tests;
  (2) authority facts + lane projection + tests. Report both hashes and the smallest remaining red.
