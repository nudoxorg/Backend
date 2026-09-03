# Pre-edit packet v2 — clang-c-lifecycle (review-id: pre-edit-2)

Supersedes pre-edit-1 after independent review (2 blockers, 3 majors — all incorporated;
findings F1–F8 from reviewer round pre-edit-1 are binding corrections). Rationale-free.

## Worktree and snapshot protocol (binding for every card)

- All worker edits happen in git worktree `.local/worktrees/clang-lifecycle`, branch
  `luna/clang-lifecycle`, pinned at `5417930ce` + the owned-file drift (digest `cc11322a4f7f7138`).
- The worktree carries UNCOMMITTED borrowed copies of other lanes' in-flight fixes
  (`driver/Cargo.toml`, `Cargo.lock`, `lower/{rust,typescript}.rs`, `languages/{rust,typescript,csharp}/**`,
  `server/index/build/fact/value.rs`). Workers never stage, edit, or commit those files; Terra
  re-syncs them from the shared tree before each gate run and records both digests.
- Gates run after `cargo clean -p <crate>` to defeat stale-unit warning nondeterminism.

## Card 1 — luna / clang-lane-repair (checkpoint A; repairs R1)

- registered role: luna; baseline: branch tip as pinned above; owned path:
  `compiler/driver/lower/clang.rs` ONLY (its `#[cfg(test)] mod tests` plus exactly one production
  dead-binding removal).
- Full diagnostic inventory to repair (fresh-reproduced, 27 diagnostics, 4 classes):
  1. missing test imports: `SourceSpan`, `IncludeFact` (from `compiler_languages_clang`) and the
     `LanguageExtensionWireFact` trait (from `compiler_ir`) used by `ClangFacts::decode` at ~2022;
  2. missing `impl core::convert::From<compiler_ir::FragmentError> for TestError` — 12 `?` sites
     at ~2084–2515 rely on it; the variant `TestError::Validate` already exists;
  3. `NativeTool::CCompiler` at ~1914 → `NativeTool::Clang` (the closed enum has no CCompiler);
  4. cascade E0308s in the two final tests resolve via (1);
  5. delete the production dead binding `let (opener, _) = ...` → rename to `_opener` or restructure
     the destructuring (one line, at ~1663) and the dead `let _ = ...` shadow bindings in the two
     final tests.
- falsifier: the R1 row command, run twice, identical outputs; every clang-named test green.
- bounds: ≤ 30 changed lines total; STOP and report if anything beyond these classes appears.
- commit: `test(compiler-driver): repair clang lane test module and lift stale fixtures` staging
  only the owned file.

## Card 2 — luna / clang-overrides-edges (checkpoint B; repairs R2–R5, R13–R15)

- owned paths: `compiler/languages/clang/{facts.rs,scratch.rs,collect.rs,ffi.rs,error.rs,lib.rs}`,
  `compiler/languages/clang/tests/{live_authority.rs,boundary.rs}`,
  `compiler/driver/lower/clang.rs`, `compiler/ir-vocabulary/occurrence.rs`,
  `compiler/ir-vocabulary/tests/occurrence_vocabulary.rs`.
- NOT owned (verified non-consumers; touching them is scope slack):
  `compiler/ir/semantic_facts.rs` (consumes via `try_from`, no per-variant arm),
  `compiler/ir/semantic_extension_section.rs` (zero `ReferenceKind` involvement), `server/**`.
- vertical, in order:
  1. ffi.rs: bind `clang_CXXMethod_isVirtual`, `clang_CXXMethod_isPureVirtual`,
     `clang_getOverriddenCursors`, `clang_disposeOverriddenCursors` (all verified ungated) into the
     existing `RequiredApi` family + `is_loaded`; one dispose site, reached on every path including
     mid-iteration capacity faults (R14); calling-while-unloaded stays impossible through the gate
     (R13).
  2. facts.rs: ONE closed three-state cell on `DeclarationFact`:
     `MethodVirtuality::NonVirtual | Virtual | PureVirtual` (libclang law: pure ⇒ virtual; the
     bool pair would encode an uninhabited quadrant). New fixed record
     `OverrideFact { source: SymbolIdentity, target: SymbolIdentity }` (identities, never names).
  3. scratch.rs + error.rs: new bounded `overrides` lane + `ScratchLane::Overrides`.
  4. collect.rs: per C++ method cursor, classify the virtuality cell; stream one `OverrideFact` per
     overridden cursor USR; de-duplicate per distinct base identity across redeclarations (R15).
  5. ir-vocabulary/occurrence.rs: additive `ReferenceKind::Overrides = 7`. Encode via the enum's
     `repr(u8)` discriminants with a single declaration (prefer `value as u8` or a
     discriminant-derived match) so a future duplicate code is a compile error by construction;
     adapt `TryFrom<u8>`; flip the existing test that asserts code 7 is rejected; add the
     duplicate-code mutation test (one mutated duplicate discriminant must fail to compile — a
     trybuild or hand-checked rustc invocation is acceptable evidence if no trybuild harness
     exists in that crate).
  6. driver/lower/clang.rs: project each OverrideFact into one occurrence owned by the pushed
     derived-method fact: `kind: Overrides`, `confidence: Oracle`, target `Local(base ordinal)`
     when the base identity was pushed in this TU, otherwise the existing foreign c-universe path.
     No owner → absent, exactly like existing occurrence laws.
- falsifiers: R2, R3, R4, R13, R14, R15 (read proof-matrix.md rows before coding).
- bounds: ≤ 400 net production lines across owned paths (tests excluded); allocation ledger per
  the skill for the native cursor array; no new dependency; no unsafe outside ffi.rs; no
  `ClangFacts` extension-row layout change (no schema work in this card); no input.rs changes.
- exact focused commands (worktree):
  - `cargo clean -p compiler-languages-clang && cargo test -p compiler-languages-clang --offline`
  - `cargo test -p compiler-languages-clang --offline --features native-test`
  - `cargo clean -p compiler-ir-vocabulary && cargo test -p compiler-ir-vocabulary --offline`
  - `cargo clean -p compiler-driver && cargo test -p compiler-driver --offline --lib clang`
  - `cargo fmt --check` on owned paths
- commits: (1) `feat(compiler-ir-vocabulary): admit Overrides reference kind` (vocabulary +
  tests only); (2) `feat(clang): collect C++ virtual override edges` (authority + lane + tests).
  Report both hashes, the falsifier outputs, and the smallest remaining red row.

## Escalation obligations recorded for later packets (not in these cards)

- Schema-2 owner-cell design (R5/R6): when issued, the card MUST use the existing
  `optional_u32` sentinel convention for the owner cell (never `0`, which collides with the
  zero-based `DeclarationId.raw`), grow `validate_fact_encoding`'s Clang arm, state the
  retained-bytes delta (+4 bytes × every clang fact) as a measurement row, and carry dual
  {1,2} schema decode with a preserved schema-1 golden section.
- Trunk primitive lifting for C widths/floats/char (render): one shared-trunk writer card,
  cross-lane golden adaptation in the same checkpoint (python lane's open question).
