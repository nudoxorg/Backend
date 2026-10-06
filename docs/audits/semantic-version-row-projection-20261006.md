# Semantic version row projection source handoff

Source-only repair based on root `3ec4df3b7c871e4b9bdc73919c0ed7e6d8ff5dcb`, in private branch `codex/sol61-semantic-surface-20261006`. Root is the sole integrator. No GUI, owner, build, or test process was started for this repair. Compilation remains unverified until a fresh whole-fleet census admits a bounded job; the ILO disk reserve hold remains in force.

The real configured Semver named version response refused 64,807 bytes against the 49,152-byte public budget. The successful generic public packet retains a 32,645-byte selected `SemanticVersionRecord` with a 31,131-byte Published history status for all 42 selected images. The readable product row copied that complete history status while the `ProductSemanticData::Versions` facet retained it again.

The repair introduces `ProductSemanticHistoryStatus` and `ProductSemanticHistoryProofSummary` for readable rows. Every status state, selection identity, refusal/retry reason, published commit/reference, verified reference tip, and input replay status comes directly from the same immutable reply. Published rows carry only the two proof facts needed by the reader. The exact version facet still carries every source and publication proof field, including the entire per-image catalog, unchanged. Projection does not consult mutable owner state. Older Published row DTOs still decode because the concise proof DTO accepts their additional full-proof fields. Source operand and outer status gates remain strict.

The actual captured Published row status measures 485 compact JSON bytes instead of 31,131, removing 30,646 duplicate bytes. This is a source-data measurement of the declared projection fields, not a compiled production packet measurement or runtime pass. Public byte budgets and hard bounds are unchanged. Exact operands that really exceed those bounds still receive an atomic typed oversized refusal.

The exact captured successful public packet is checked in at `crates/present/fixtures/semantic-versions-semver-public.json`, with its provenance and SHA-256 in the neighboring Markdown file. It is complete public egress evidence; no certificate, schema, or query proof was invented. The actual `SemanticVersionRecord` has no `schema` or `query_proof` field. The existing complete shape-export schema fixtures and certificate-bearing selected-row gates are retained.

New regression source covers complete source DTO serialization, all 42 exact history proofs surviving Summary/Standard/Full versions facets, typed row summaries, reuse of the exact published source as a shape request operand, reproduction of the old duplication failure, and genuinely oversized operand refusal. CLI JSON bytes are checked against the production shared encoder; MCP whole JSON-RPC responses and readable text are checked against the same encoder and renderer. Existing status tests cover all seven states and older Published row DTO decoding.

Source checks completed: direct installed Rust 1.97.1 `rustfmt --check` on all six changed Rust files, `git diff --check`, and Python inspection of the actual fixture SHA-256, lengths, selection/current/complete state, and 42-image catalog. No Cargo test or compile result is claimed.

Selector dependencies are isolated before this repair, and root can integrate them separately:

| Original | Private cherry-pick | Purpose |
| --- | --- | --- |
| `952d4c0293` | `59ae7bbc8f` | Full retained selected row IDs |
| `b28c099914` | `3bdc38fbd6` | Canonical symbol-key strings |
| `baf567bdd4` | `1bf7a796bf` | Boxed shared surface request fixture |
| `9aedd8f96b` | `dc54210301` | Checked projection fixture with semantic lane unconfigured |
| `63e9bd6a99` | `2e29546228` | Catalog-owned source and view recipe |
| `f3b9319b84` | `32760e4816` | Exact bounded query page proof and strict ReplyDto serialization/decode |

The frozen `f3b9319b84` selector correction has not yet been compiled. The prior `63e9bd6a99` CLI query proof fixture was failing; it must not be reported green based on source review or earlier projection tests.

After root admits a suitable warm target through a fresh whole-fleet census, run these serially with at most four Cargo jobs and the existing per-host resource caps:

```sh
CARGO_BUILD_JOBS=4 cargo test -p backend-present --lib captured_semver_history_fits_once_and_preserves_the_complete_source_operand
CARGO_BUILD_JOBS=4 cargo test -p backend-present --lib semantic_history
CARGO_BUILD_JOBS=4 cargo test -p backend-cli --lib cli_named_versions_preserves_the_captured_semver_operand_at_every_detail
CARGO_BUILD_JOBS=4 cargo test -p backend-cli --lib cli_resolve_selector_round_trips_into_the_shared_shape_request
CARGO_BUILD_JOBS=4 cargo test -p backend-mcp --lib semantic_versions_jsonrpc
CARGO_BUILD_JOBS=4 cargo test -p backend-mcp --lib semantic_shapes
```

Use the source-matched full adapter suites when the focused gates pass and fleet admission permits them. A fresh matched runtime should then re-run the named Semver versions route at each detail and the exact public selector-to-shape journey. This presentation repair grants no compiler member/type completeness or whole-package acceptance credit.
