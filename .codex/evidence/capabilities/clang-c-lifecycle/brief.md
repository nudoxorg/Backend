# Capability brief — clang-c-lifecycle

## Public terminal

One closed pipeline journey: a PURL naming a C/C++ codebase plus its local checkout opens a
whole-translation-unit lifecycle — include/build closure discovery through the codebase's own build
system (compile_commands.json via libclang's compilation-database API; CMake, Make, BUCK adapters
that drive the build system rather than reimplement it), sysroot/toolchain binding, direct libclang
authority per translation unit, canonical fragment admission, durable publish → reopen → index —
after which every committed fragment (old and new generations) still validates, and the semantic IR
renders C/C++ structs, signatures, docs, and occurrences through `compiler-ir` `render.rs`.

## Chief-owned red journey (mandate)

1. FULL projection into `ClangFacts` + lane: recursive/forward-declared types (two-pass,
   self-nominals), pointers/references/arrays/fn-ptrs, C++ templates/namespaces/virtual override
   edges, qualifiers/storage/layout in the extension row, Parameter-fact signatures, doxygen docs,
   occurrences (Oracle→Local / Foreign c-universe), includes.
2. STRUCT RENDERING via `compiler/ir` `render.rs`; golden render tests.
3. FULL LIFECYCLE from PURL as above; old fragments keep validating.
4. Integration tests on REAL codebases, niche+broad (single-header library such as stb + one small
   real project), decoded-output analysis, edge-case feedback via Luna cards.

## Ownership and concurrent paths

- Mine: `compiler/languages/clang/**` (authority crate, lifecycle, integration tests),
  `compiler/driver/lower/clang.rs`.
- Escalated (one coherent packet, one writer each): `compiler/ir-vocabulary/occurrence.rs`
  (additive `ReferenceKind::Overrides = 7`), `compiler/ir` (dual-schema extension section, clang
  extension row owner cell, `render.rs` struct bodies), `compiler/driver/lower.rs` (shared emission
  lane: owner-cell plumbing, `build_ir` member/parent population).
- Never touched by this capability: other language lanes' projection files
  (`lower/{rust,go,python,typescript,csharp,java}.rs`), heart/interface/server product code except
  as read-only composition from integration tests.

## Non-negotiable laws

- No scanner/token/source-text fallback anywhere; missing libclang, missing build system, or
  missing tool is a typed terminal that retains the exact cause.
- Every fragment published stays validating: the extension section gains schema 2 with dual
  (1,2) decode; occurrence vocabulary gains only an additive code 7. Old bytes decode identically.
- Emission stays two-pass declarations-first with strictly backward targets; pooled anonymous rows
  stay topologically ordered; no fabricated coordinates, no partial lying rows.
- Whole-translation-unit work runs through the codebase's build system; the pipeline never guesses
  flags it did not read from the located database or build-system output.
- Bounded lanes: every fact array capacity boundary is an exact typed error, never truncation.
- Cancellation is observed before native loading and at every translation-unit boundary.
- No new external crate dependency; libclang access stays inside the one reviewed `ffi.rs` unsafe
  boundary (clang-sys 1.9.1, runtime loading, `clang_3_6` surface — verified ungated symbols:
  `clang_CXXMethod_isVirtual`, `clang_CXXMethod_isPureVirtual`, `clang_getOverriddenCursors`,
  `clang_disposeOverriddenCursors`, `clang_CompilationDatabase_*`, `clang_Cursor_isVariadic`,
  `clang_getCanonicalCursor`).

## TESTING.md

No `TESTING.md` exists in this repository (verified by tree search). Clause mapping therefore
lives entirely in the proof matrix; the matrix binds every law to a falsifier and exact command.

## Baseline (frozen 2026-09-02, independent of git status)

Baseline commit `2c0b26f86...` plus the working-tree state of owned paths, frozen by digest:

| file | LOC | sha256 (first 16) |
|---|---|---|
| compiler/languages/clang/lib.rs | 30 | 90d594ee0d0e7be8 |
| compiler/languages/clang/facts.rs | 366 | 39b1c63ab6aa164d |
| compiler/languages/clang/collect.rs | 758 | 66f86491fe9a0f96 |
| compiler/languages/clang/ffi.rs | 648 | 187503978a845195 |
| compiler/languages/clang/input.rs | 118 | d5ba915af5a8dfef |
| compiler/languages/clang/scratch.rs | 24 | c7c515a3d498ff4a |
| compiler/languages/clang/error.rs | 164 | 256eef6a0634551e |
| compiler/languages/clang/Cargo.toml | 26 | f9aafe838ec93bdf |
| compiler/driver/lower/clang.rs | 2627 | 0cf3ad3fff9268ec |
| compiler/languages/clang/tests/live_authority.rs | 329 | a57b8795171b2ab9 |
| compiler/languages/clang/tests/boundary.rs | 104 | 6b989ee0e12c67dd |

Environment facts: libclang at `/Library/Developer/CommandLineTools/usr/lib/libclang.dylib`
(runtime-loaded by clang-sys). Tests run through `.local/target` (workspace-local target dir).
Known cross-lane red (not mine, external): `compiler-driver` lib-test binary fails to compile due
to other lanes' in-flight adaptation to the python lane's `OccurrenceTarget::Stable` vocabulary
addition (`lower/{rust,go,typescript}.rs` test modules). Clang-lane acceptance gates are chosen so
they are decidable while that external red persists; closure requires it healed or formally
attributed.
