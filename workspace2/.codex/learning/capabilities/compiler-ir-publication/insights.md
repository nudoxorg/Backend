# Compiler, semantic IR, and publication

## Observed

- fingerprint: unavailable-native-tooling-must-not-be-fallback-parser
  role: terra
  capability and commit: compiler-ir-publication / pending native-driver checkpoint
  observed behavior and concrete artifact: the prior registry supplied borrowed echo behavior for only two synthetic language tags; the local host has reproducible Rust, Python, and Clang tools but lacks TypeScript, Go, Java, and C# compilers.
  why the current rubric/skill/tool allowed it: the first vocabulary red constrained tags, not parser provenance.
  local correction attempted and result: `nudox-compile-driver` dispatches concrete Rust/Python/Clang process adapters and returns `CompileFailure::ToolingUnavailable` with language, tool, and source identity for the other tags.
  suggested enforcement: test
  occurrences: compiler-ir-publication
  state: closed
  owner and closing artifact: terra / native compile integration test

- fingerprint: source-identity-is-not-a-semantic-entity
  role: terra
  capability and commit: compiler-ir-publication / semantic atom checkpoint
  observed behavior and concrete artifact: two valid equal-length Rust declarations previously produced an identical entity/type fragment because only a source digest differed outside semantic lanes.
  why the current rubric/skill/tool allowed it: source provenance was retained, but entity records did not bind declaration atoms or kinds.
  local correction attempted and result: each entity now binds a validated atom and closed declaration kind; the fragment carries a central `ContentId<SourceFactDomain>` plus byte length, and tests distinguish `alpha: bool` from `bravo: i32` by decoded atom and type node.
  suggested enforcement: test
  occurrences: compiler-ir-publication
  state: closed
  owner and closing artifact: terra / native compile integration test and operation semantic journey

- fingerprint: recipe-authority-must-be-recomputed-from-fragment-facts
  role: terra
  capability and commit: compiler-ir-publication / recipe-bearing range manifest
  observed behavior and concrete artifact: an authority-valid recipe identity could otherwise be paired with mutated language, stage, tool, source, or toolchain cells in a fragment.
  why the current rubric/skill/tool allowed it: typed identifier decoding verifies the registry cell but cannot prove cross-field derivation.
  local correction attempted and result: the recipe lane now retains the decoded closed facts and validation recomputes `CompileRecipeFact::derive`; a language mutation retaining the old recipe identity is a structured relation fault.
  suggested enforcement: test
  occurrences: compiler-ir-publication
  state: closed
  owner and closing artifact: terra / wire attack and range manifest tests

- fingerprint: native-parse-admission-must-not-fabricate-semantic-or-workspace-facts
  role: terra
  capability and commit: compiler-ir-publication / typed native-driver seam
  observed behavior and concrete artifact: a native adapter could borrow a resolved Rust executable for unavailable TypeScript, emit scratch `rmeta*` under the repository cwd, or lower unknown declarations to `I32` after syntax admission.
  why the current rubric/skill/tool allowed it: static registry rows, process ownership, and compact lowerer facts were tested separately rather than at their shared request boundary.
  local correction attempted and result: `CompileRequest` now distinguishes `ResolvedNative` from `ExplicitlyUnavailable`, only native lowering derives a persisted recipe, every native child uses an empty caller-owned work directory with exact metadata cleanup, and closed lowerers admit only Bool/I32/String facts or return `LoweringUnsupported`.
  suggested enforcement: test
  occurrences: compiler-ir-publication
  state: closed
  owner and closing artifact: terra / native compile integration test

- fingerprint: native-child-poll-interval-is-bounded-but-not-yet-scaling-proven
  role: terra
  capability and commit: compiler-ir-publication / pending all-native frontend slice
  observed behavior and concrete artifact: `native::terminal::wait_for_terminal` polls `try_wait` every 1ms; its deadline and cancellation fixtures prove bounded interruption, but long-lived child wakeups scale linearly with elapsed duration.
  local correction attempted and result: retained the 1ms interval because the current process API has no cancellation-aware blocking wait; replacing it needs focused cancellation-latency and wakeup-rate evidence rather than an unmeasured retry-policy change.
  suggested enforcement: benchmark
  occurrences: compiler-ir-publication
  state: open
  owner and closing artifact: terra / native child lifecycle benchmark

- fingerprint: mapped-fragment-borrows-require-one-complete-proof
  role: terra
  capability and commit: compiler-ir-publication / mapped fragment retrieval
  observed behavior and concrete artifact: a byte-range manifest alone cannot safely lend file-backed semantic views unless the exact complete mapping first satisfies both its artifact commitment and compact-fragment grammar.
  why the current rubric/skill/tool allowed it: range verification protected fetched sections, but did not own a file-backed lifecycle or prohibit an extra parse on each view.
  local correction attempted and result: optional `mmap` opens a checked nonzero immutable file length, maps exactly that length, validates the complete six-lane manifest and grammar once, then retains the `FragmentLayout` so views borrow the same mapping pointer without reparse or copy.
  suggested enforcement: test
  occurrences: compiler-ir-publication
  state: closed
  owner and closing artifact: terra / mapped fragment feature tests

## Explained

## Corrected

## Promoted
