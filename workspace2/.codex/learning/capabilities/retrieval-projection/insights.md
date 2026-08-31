# Retrieval projection insights

## Observed

- fingerprint: trustfall-fixed-query-variable-grammar
  role: terra
  capability and commit: retrieval-projection, pending first Trustfall commit
  observed behavior and concrete artifact: the initial fixed `Neighbors(sourceHigh: $sourceHigh, sourceLow: $sourceLow)` operation was rejected by Trustfall 0.8; the executable red was `cargo test -p nudox-index-trustfall --test graph_query --offline`.
  why the current rubric/skill/tool allowed it: the schema/query rule specified typed Trustfall but did not encode Trustfall's declarative filter grammar.
  local correction attempted and result: the fixed `Entities` traversal uses typed `@filter` variables, then resolves `neighbors` from the already borrowed `ValidatedGraphView`; static grammar and adapter-invariant tests pass.
  suggested enforcement: test
  occurrences: retrieval-projection
  state: closed
  owner and closing artifact: Terra, `crates/nudox-index-trustfall/src/schema.rs`

## Explained

- Trustfall 0.8's public adapter ABI is synchronous but requires boxed iterator return types and an execution-scoped `Arc`; these are upstream interface obligations rather than retained graph ownership. The adapter owns neither transport nor graph data, and public failure states retain only closed local phases because Trustfall's concrete frontend errors are private upstream.

## Corrected

- The new adapter splits the full `u32` entity identity into two checked `Int` filter/output halves, so it does not stringify or truncate canonical IDs. It establishes output capacity before parsing/executing, preserves caller slots on rejection, and sorts bounded emitted facts by entity/partition.

## Promoted

- task-local disposition: keep async acquisition, cancellation, S3/runtime, and cache machinery out of `nudox-index-trustfall`; only graph-vector may obtain and pin the borrowed view.
