# Capability brief — python-type-authority

## Public terminal
A `pypi:pkg@ver` PURL becomes published, re-openable, index-validating IR fragments whose
semantic lanes carry Ruff syntax facts plus pyrefly type-authority facts, and whose lanes
drive the IR struct renderers — proven on real packages.

## Non-negotiable laws
1. Ruff stays the sole syntax authority (spans, declarations, written annotations).
2. pyrefly is the peer type authority: inferred types, import resolution, symbol resolution,
   delivered only through its bounded typed transaction; unavailable authority is honest
   absence, never a fabricated fact.
3. Every lane fact carries exact source spans or borrowed bytes; honest `TypeReason`s only
   where nothing is derivable.
4. Compound annotations lower fully: list/dict/tuple/Callable/Optional/TypedDict→anonymous
   record/Protocol/literals/PEP 604 unions/PEP 695 type aliases and parameters.
5. Occurrences carry Index/Import/Oracle tiers; Import targets mint `pypi` Package foreign keys.
6. Struct rendering is driven by IR lanes only; golden render text is evidence.
7. Lifecycle: locate+fetch from PyPI, workspace detection, build only when the workspace
   requires it, publish→reopen→index round-trip, older generations keep validating.
8. Real-package evidence: pinned requests, attrs, flask, plus two niche packages; assertions
   analyze decoded lanes, never "no error" weakest-form checks.

## Baseline (frozen 2026-09-02, working tree state, rustfmt-clean)
file | LOC | sha256(16)
compiler/languages/python/lib.rs | 1473 | 44bb6a4975606f43
compiler/languages/python/checker.rs | 2098 | a1cbb59c5def9954
compiler/languages/python/tests/facts.rs | 238 | ef6bcc99b01950a5
compiler/languages/python/tests/quoted_annotations.rs | 156 | cc45f31b14e187b8
compiler/languages/python/tests/syntax_facts.rs | 173 | e4813e077b175181
compiler/driver/lower/python.rs | 3532 | 576119066b6f7a90

## Concurrent path ownership
- Terra python (this lane): compiler/languages/python/**, compiler/driver/lower/python.rs,
  new integration test files under compiler/driver/tests/.
- Adjacent lanes in this shared worktree own clang/go/csharp/typescript/rust files and
  compiler/driver/lower.rs (formatting-only drift observed); this lane never writes them.
- compiler/ir/** changes via escalation only.

## Affected consumers
compiler-driver (semantic lanes), compiler-publication (publish/reopen), server-journal
(durable generations), server-index (round-trip validation), compiler-ir render displays.

## TESTING.md digest
TESTING.md not present in repository root; clause mapping is expressed directly as proof
matrix rows (test laws inherited from deliver-reviewed-rust-slice: boundary cases, typed
terminals, exact diagnostics, allocation/bound evidence for adapter work).

## Product-authority questions
None open. New dev-dependencies for the integration test tree (HTTP fetch, wheel unpack)
are briefed by the parent mandate's lifecycle clause; they stay test-only.
