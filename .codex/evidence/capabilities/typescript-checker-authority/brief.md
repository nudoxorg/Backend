# Capability: typescript-checker-authority

Opened: 2026-09-02. Terra manager: TypeScript lane.

## Public terminal

The full TypeScript fidelity journey: from npm PURL `npm:pkg@ver` through registry
locate/fetch, package/workspace location, npm build when types ship as artifacts,
TSZ checker authority (real `tsc` program API over a bounded subprocess),
OXC+checker lowering into canonical IR fragments, golden rendering from those
fragments, publish -> reopen -> index — proven over a 20-package real corpus.

## Non-negotiable laws

1. **Checker authority first-class.** `compiler/languages/typescript/checker.rs`
   runs the real TypeScript checker (`tsc` program API via the vendored driver
   `checker/main.cjs`), with invocation discipline mirroring
   `compiler/driver/native/typescript.rs` (`--noEmit`, `--pretty false`,
   `--target ES2022`, `--module ESNext`, `--jsx preserve` for TSX), bounded
   output + wall clock, SHA-256 source binding, UTF-16 -> UTF-8 span binding.
   A TypeScript compile with an unavailable checker is a typed
   `ToolingUnavailable`-class terminal, never a silently degraded declared-only
   success.
2. **No escape hatches.** No "future", "degradation", "backlog", or fallback
   authority language survives in owned production paths. A proven-but-
   unrepresented construct is a typed `TypeReason` record, not prose.
3. **Full lattice coverage.** Recursive, nominal, generic, conditional, mapped,
   template-literal, literal types; self-nominals; inference + widening;
   `this`; narrowing; JSDoc; overloads; extension facts — each with an
   end-to-end falsifier asserting decoded IR records and a golden rendering.
4. **Rendering from the lanes.** `compiler/ir` render displays (Signature,
   Type, Docs, Embedding) are driven from the TypeScript fragments; golden
   render tests per feature class. No `compiler/ir` edits from this lane
   (escalation only).
5. **Full lifecycle.** PURL -> registry -> package location (monorepo/
   lockfile-aware) -> npm build when types are build artifacts -> authority ->
   fragments -> publish -> reopen -> index.
6. **Backwards compatibility.** Schema-1 checker transcripts
   (`tests/transcripts/golden.json`) keep decoding; every frozen golden
   fragment fact set keeps validating byte-for-byte.
7. **Corpus proof.** 20 mostly-random real npm packages (niche AND famous,
   incl. monorepo subpackages and non-standard layouts): end-to-end run, deep
   decoded-IR review vs source truth, lowering perf profile; edge fixes return
   through Luna repair cards until rubric-clean.

## Explicit negative space

- No `compiler/ir/**` edits (escalate to Sol). Rendering is consumed, not changed.
- No sibling lanes' files (clang/python/go/rust/java/csharp paths, their tests,
  their evidence directories) even when the shared `--lib` test binary is broken
  by their stale inline tests. This lane's gates use top-level `--test` targets.
- No new dependency without parent authority (`ureq` is already admitted by the
  python lane's test support; reusing it needs no new authority).
- No unsafe, no SIMD, no async runtime.

## Baseline

- Baseline commit `67cdc911981905431642bfe0cfb7ce5d33cb757c` plus the in-tree
  uncommitted TypeScript working set (kept per parent instruction). The frozen
  per-file formatted-LOC + SHA-256 table lives in `index.toml`.
- Live lane state at baseline:
  - `cargo test -p compiler-languages-typescript` GREEN (17 + 5 passed).
  - `compiler-driver` lib (non-test) compiles clean; the shared `--lib` test
    binary does not compile due to sibling lanes' stale inline tests
    (clang.rs E0422/E0277/E0599, rust.rs E0433/E0277) — pre-existing at HEAD.
  - This lane's own inline tests carry 8 stale-API compile errors
    (E0308 x6: `fact_named` tuple arity + `ObjectMember` Vec; E0004 x2:
    `OccurrenceTarget::Stable` arm) — lane row R1.

## Ownership (exact)

- `compiler/languages/typescript/**` (authority, checker, coordinate, error,
  checker/main.cjs, tests).
- `compiler/driver/lower/typescript.rs` (incl. inline `mod tests`).
- `compiler/driver/native/typescript.rs`.
- `compiler/driver/tests/typescript_*.rs` and
  `compiler/driver/tests/typescript_*/` (new integration trees).
- `compiler/driver/types/compile.rs` + `compiler/driver/types/request.rs`:
  ONLY the TypeScript checker-authority surface additions
  (`SemanticAuthorityInput::TypeScript` variant + routing), coordinated with
  the lane; no other lanes' match arms may be weakened.
- `.codex/evidence/capabilities/typescript-checker-authority/**`.

## Consumers / dependency direction

- `compiler-driver` -> `compiler-languages-typescript` -> `compiler-vocabulary`,
  oxc crates, `serde`/`sha2` (checker wire).
- `compiler-driver` -> `compiler-ir` (fragments, extension sections).
- `compiler-publication` + `server-journal` + `server-index-*` consume the
  fragments downstream (lifecycle row R7).
- Public pipeline entry: `compiler_driver::{compile, compile_ir}` with
  `CompileRequest { profile: LanguageProfile::TypeScript(_) , .. }`.

## TESTING.md

TESTING.md does not exist in this repository (evidenced exclusion, matching
sibling lanes). Clause mapping is expressed directly as proof-matrix rows:
bounded-child/typed rejection rows (checker_protocol tests), exact
operand/source retention, goldens, corpus, perf.

## Product-authority questions (none open)

None pending. The checker-required-vs-degraded question is settled by the
parent mandate ("delete every degradation escape hatch").
