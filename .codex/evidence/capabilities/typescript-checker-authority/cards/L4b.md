# Card L4b: production checker package context (matrix row R10)

registered role: nudox_luna_implementer (expected `luna`/max)
baseline: canonical 58634ef11 + trunk narrowings landing in owned paths
  (frozen per-file hashes in index.toml [baseline.files])
owned paths (no other file may be edited):
  - compiler/languages/typescript/checker.rs
  - compiler/languages/typescript/checker/main.cjs
  - compiler/languages/typescript/lib.rs        (export additions only)
  - compiler/languages/typescript/error.rs      (only if a new typed cause is required)
  - compiler/driver/tests/typescript_package.rs (NEW file)
forbidden adjacent surface: compiler/driver/lower.rs + lower/typescript.rs (L4a owns them),
  compiler/ir/**, report grammar version (schema stays REQUIRED_SCHEMA_VERSION; adding the
  package root is request-side context, not a transcript schema change), sibling lanes' files.

## Public terminal (matrix row R10 verbatim intent)

Production `Checker` runs against a package root so in-package imports and the package's own
node_modules dependencies resolve; decoded reference rows carry module-qualified origins that a
bare isolated temp-file run provably cannot produce.

## Required behavior

1. `Checker` gains a package-root run path (signature is yours, e.g.
   `run_in_package(&self, profile, source: &[u8], package_root: &Path) -> Result<Report, CheckerError>`).
   Laws it must satisfy:
   - The caller's package tree stays byte-identical after the run (the run must not write into it).
     Stage a bounded copy of the package tree into the run work directory (typed caps: file count,
     total bytes, deadline; exceeding one is a distinct typed cause), write the entry source into
     the staged copy at its package-relative path, and point the vendored driver at that file so
     TypeScript module resolution sees the package layout (relative imports, node_modules).
   - The existing `run()` behavior, caps, deadline, process-group kill, and exit-3 protocol are
     untouched for the no-root case; R11's bounded-child tests stay green.
2. `main.cjs` must not need protocol changes for resolution (ts compiler options already resolve
   from the entry file's directory). If it does need a change, keep every offset semantic
   (UTF-16 code units) and the output grammar additive-only.
3. New test file `compiler/driver/tests/typescript_package.rs` builds a synthetic package fixture
   in a temp dir: entry `index.ts` re-exporting from `./lib/util.ts`, importing a dependency from
   the package's own `node_modules/<dep>` (a stub package you create with a real index.d.ts), and
   one deliberately unresolved import.
   Falsifiers:
   a. BARE-RUN DISCRIMINATOR: the same entry bytes through `Checker::run()` (no root) cannot
      produce the module-qualified origins the package run produces — assert the exact difference
      (the bare run's rows for the same import stay unresolved/syntactic).
   b. PACKAGE RUN: reference rows for the re-export and the node_modules import carry
      module-qualified foreign origins naming the exact module specifier; the unresolved import
      stays honestly unresolved (no invented origin).
   c. Purity: after both runs, the fixture tree hashes identically to before (committed digest
      computed in-test).
   d. The test asserts `!skipped` semantics by construction (no env-var skip path exists).
4. Existing tests stay green; if a transcript byte changes anywhere (goldens), STOP and report —
   package context must not alter single-file report bytes.

## Proof-matrix rows bound to this card

- R10 full falsifier chain (a)(b) above; (c)(d) are the honesty guards.
- R11 partial: existing checker_protocol bounded-child tests stay green.

## Exact focused commands

- cargo test -p compiler-languages-typescript
- cargo test -p compiler-driver --test typescript_package
- cargo test -p compiler-driver --test typescript_lower   (regression watch; blocked only by sibling churn — report if so)
- cargo check -p compiler-driver --lib

## Bounds and stop decisions

- No new dependency, no unsafe, no new generic parameter, no public enum non-exhaustive hole.
- New typed causes in error.rs follow the existing thiserror doc-comment style; every cause keeps
  its `#[source]`.
- If module-qualified origins require report grammar changes beyond additive fields, STOP and
  report the exact grammar conflict for Terra; do not bump the schema version yourself.
- If the shared worktree's compiler-driver lib does not compile due to SIBLING-lane churn, run the
  crate-level gates and report the driver-gate blockage as `EVIDENCE_BLOCKED: sibling churn`.

## Checkpoint protocol

Commit owned paths only, message prefix `feat(typescript):`. Return: commit sha, focused command
outputs, the discriminator's exact before/after rows, and the smallest remaining red row.
