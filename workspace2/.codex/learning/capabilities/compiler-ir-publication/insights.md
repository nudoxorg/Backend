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

## Explained

## Corrected

## Promoted
