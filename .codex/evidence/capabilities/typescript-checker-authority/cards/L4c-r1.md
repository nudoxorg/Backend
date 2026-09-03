# Card L4c-r1: full lifecycle from PURL `npm:pkg@ver` (matrix row R7) — re-based

registered role: nudox_luna_implementer (expected `luna`/max)

This card is card L4c.md verbatim in intent, re-frozen because the baseline
moved: the lane consumed trunk (canonical consumed first; origin/canonical
2f82d74b4 is an ancestor of HEAD 309acc8f1) and L6a is landing in parallel.

baseline: 309acc8f1 (L5 freeze; index.toml per-file table).
L6a owns compiler/driver/lower/typescript.rs, compiler/driver/lower.rs,
compiler/driver/tests/typescript_lower.rs in the same worktree — treat
those as live; your owned paths are disjoint from all of them.

owned paths (no other file may be edited):
  - compiler/driver/tests/typescript_purl_lifecycle.rs (NEW)
  - compiler/driver/tests/typescript_support/mod.rs    (NEW)
  - compiler/driver/Cargo.toml                         (AT MOST one dev-dependency line:
    serde_json = { workspace = true }, needed for package.json/package-lock.json parsing;
    no other manifest change)

forbidden adjacent surface: everything else. No production code changes. The registry/extract/
build mechanics are TEST SUPPORT with typed errors, caps, and deadlines; the production pipeline
owns compile -> publish -> reopen -> index exactly as the python lane modeled it.

## Public terminal (matrix row R7 verbatim intent)

One famous single-package PURL and one monorepo/scoped-subpackage PURL travel end-to-end:
registry locate/fetch/extract -> (npm build ONLY when the package's types ship as build
artifacts) -> production authority -> fragments -> publish -> journal reopen -> index rows —
plus a second chained generation, and the old schema-1 golden fragment keeps validating beside it.

## Required behavior

Execute card L4c.md sections "Required behavior" (points 1-9), "Exact focused
commands", "Bounds and stop decisions", and "Checkpoint protocol" unchanged,
with these L5 environment deltas:

- Environment: worktree /private/tmp/nudox-fidelity-typescript.
  Every command runs with PATH=/opt/homebrew/bin:$PATH and
  NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules (the worktree
  has no root node_modules; the checker resolves the 5.9.3 module via
  NODE_PATH — the driver's documented mechanism).
- Regression watch baseline: typescript_lower is RED at 309acc8f1
  (geometry falsifier + D1, L6a is repairing). Your green obligations are
  your own target, `cargo check -p compiler-driver --tests` (with your
  paths), `typescript_authority`, `typescript_package`, `typescript_render`,
  and the typescript crate tests. Do not touch typescript_lower.rs.
- The corpus card (L7) will extend your typescript_support module NEXT, so:
  keep every support function typed, single-purpose, and reusable (PURL
  parse, locate/fetch/extract, entry resolution, lockfile facts, build leg)
  — the corpus driver is a consumer, not a fork.
- The dev-dependency line and test-support code must not regress the
  portable client graph: support lives under tests/ only.

## Checkpoint protocol

Commit owned paths only, message prefix `test(typescript):`. Return: commit
sha, focused command outputs, the two PURLs + third build-leg package (or the
two attempts), index row counts, pinned tarball digests, and the smallest
remaining red row.
