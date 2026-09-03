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
   A TypeScript compile whose checker is unavailable (env-var binary missing,
   node absent, or exit-3 `typescript` module missing) is a public
   `CompileFailure::Authority` terminal whose diagnostic retains the exact
   `CheckerError` cause — the swallow arm at `lower/typescript.rs`
   (`ToolingUnavailable | ModuleUnavailable => None`) is deleted. Zero facts
   are emitted on that terminal. No new `NativeTool`/vocabulary variant is
   introduced (vocabulary is outside lane ownership); the existing
   `AuthorityFailure::TypeScript` plumbing names the failure.
2. **No escape hatches.** No "future", "degradation", "backlog", "proceeds at
   reduced fidelity" authority language or authority-optional branch survives
   in owned production paths. A proven-but-unrepresented construct is a typed
   `TypeReason` record; its doc language names the TypeReason, not a backlog.
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
   fragments -> publish -> reopen -> index. Honest split of responsibility
   (python-lane precedent): registry locate/fetch/extract and the npm build
   invocation are lane TEST SUPPORT with typed errors, byte caps, and
   deadlines; the production terminal is the compile pipeline over the
   located package sources. The production checker authority gains a
   package-context run path (`Checker` runs against a package root, so
   in-package imports and node_modules dependencies resolve) — R10 proves a
   module-qualified origin a bare temp-dir run cannot produce.
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
- No new runtime dependency without parent authority. Admitted dev/test
  dependencies: `ureq` (registry fetch, python-lane precedent) and the
  repository-root `package.json` -> `typescript` npm module (5.9.3) required
  by the vendored checker driver — both test-time only, never in the
  portable client graph.
- No unsafe, no SIMD, no async runtime.

## Baseline

- Baseline re-frozen at commit `fa7f6966d` (L0 lane-green repair included;
  see index.toml per-file table and the pre-edit-1 packet for why the
  original freeze was superseded). Live environment: node v26.8.1,
  `typescript` npm module 5.9.3 resolved from repository-root
  `node_modules/typescript` (PATH `/opt/homebrew/bin`).
- Live lane state at the re-freeze (Terra receipts, reproduced):
  - `cargo test -p compiler-languages-typescript` GREEN (1 + 17 + 5 passed).
  - `cargo check -p compiler-driver --lib` clean.
  - The shared `cargo check -p compiler-driver --lib --tests` binary does
    not compile due to sibling lanes' stale inline tests (clang.rs ~20
    errors, rust.rs ~9, python lane ~6 incl. its test-support files) —
    pre-existing at HEAD and outside this lane's paths. This lane's gates
    are top-level `--test` targets, which build the lib without inline test
    modules.

## Ownership (exact)

- `compiler/languages/typescript/**` (authority, checker, coordinate, error,
  checker/main.cjs, tests).
- `compiler/driver/lower/typescript.rs` (incl. its test region until L1
  migrates it into the integration tree).
- `compiler/driver/native/typescript.rs`.
- `compiler/driver/tests/typescript_*.rs` and
  `compiler/driver/tests/typescript_*/` (new integration trees).
- `compiler/driver/types/compile.rs` + `compiler/driver/types/request.rs`:
  ONLY the TypeScript checker-authority surface additions
  (`SemanticAuthorityInput::TypeScript { report: &'source Report }` variant
  + routing), coordinated with the lane; no other lanes' match arms may be
  weakened; the shared enum stays exhaustive (not `#[non_exhaustive]`), so
  the two in-repo forced matches (`compile.rs` authority-mismatch and
  direct-authority checks) gain explicit TypeScript arms — rustc custody.
  Borrowing `&'source Report` preserves `Copy` on
  `SemanticAuthorityInput`/`CompileRequest`.
- `compiler/driver/lower.rs`: ONLY the TypeScript lane surface (its
  `EmissionExtension::TypeScript` arm and TypeScript-relevant constants);
  sibling lanes' surfaces there are forbidden.
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
the go lane's recorded adaptation). Clause mapping is expressed directly as
proof-matrix rows: bounded-child/typed rejection rows (checker_protocol
tests), exact operand/source retention, goldens, corpus, perf.

## Product-authority questions

None open. The checker-required-vs-degraded question is settled by the
parent mandate ("delete every degradation escape hatch") and pinned by the
pre-edit-1 review (B2): reuse `AuthorityFailure::TypeScript`, zero new
vocabulary.

## Open environment notes

- interface/protocol mirrors (`interface/protocol/tests/support/compiler/`)
  do not carry authority inputs today; if protocol goldens later gain a
  checker-authority field, that mirror is a separate boundary decision
  (q2 of pre-edit-1).
- The shared `--lib` test binary of `compiler-driver` is broken by sibling
  lanes' stale inline tests (clang ~20, rust ~9, python ~6 errors at the
  re-freeze). Out-of-constraint for this lane; repairs belong to sibling
  lanes. Until then this lane's gates are top-level `--test` targets only.
