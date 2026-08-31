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

- fingerprint: sync-terminal-overclaim
  role: terra
  capability and commit: retrieval-projection, pending graph/Qdrant completion commit
  observed behavior and concrete artifact: exact, lexical, and local vector operations are synchronous borrowed calculations and had been rubric-labeled as if each could independently emit cancellation and asynchronous failure terminals.
  why the current rubric/skill/tool allowed it: terminal vocabulary was specified globally without distinguishing a synchronous pure query from its bounded acquisition boundary.
  local correction attempted and result: retained core complete/partial/degraded and typed preflight `Result` semantics; added `GraphDegradation` plus degraded/degraded-partial to the existing bounded graph lease, which now has complete/partial/degraded/cancelled/failed states and verifies exact missing partitions.
  suggested enforcement: test
  occurrences: retrieval-projection
  state: closed
  owner and closing artifact: Terra, `crates/nudox-index-graph-vector/tests/rejected_runtime_attacks.rs`

## Explained

- Trustfall 0.8's public adapter ABI is synchronous but requires boxed iterator return types and an execution-scoped `Arc`; these are upstream interface obligations rather than retained graph ownership. The adapter owns neither transport nor graph data, and public failure states retain only closed local phases because Trustfall's concrete frontend errors are private upstream.

## Corrected

- The new adapter splits the full `u32` entity identity into two checked `Int` filter/output halves, so it does not stringify or truncate canonical IDs. It establishes output capacity before parsing/executing, preserves caller slots on rejection, and sorts bounded emitted facts by entity/partition.
- The local Qdrant launcher starts within its disposable storage directory, runs the metric/isolation/update/delete journey, stops the service for the typed outage assertion, then restarts on the same storage and verifies readback/query/delete. The final live run passed all four ignored tests on 2026-08-31.
- `nudox-index-retrieval` is the only server boundary that accepts the non-copyable `PublishedIndexSnapshot` witness and a `Cancellation`. It maps exact/lexical manifests, Tantivy, Trustfall, and Qdrant to one closed complete/partial/degraded/cancelled/failed terminal while retaining direct IDs, partition absences, and concrete adapter causes. Blocking adapters sample cancellation before work only.
- Trustfall composes the bounded `GraphTerminal`: cancelled and failed acquisition never construct the synchronous adapter; partial and degraded-partial retain `MissingPartitions`; a view whose authority differs from the acquisition terminal fails before Trustfall construction. The focused sealed-publication test also proves Qdrant and Trustfall wrong-authority preflight preserves output sentinels.
- ledgers: Trustfall retains only the upstream-required call-scoped `Arc` and boxed iterators; it borrows graph rows and writes at most four rows × sixteen edges into caller output. Its source de-duplication is bounded O(64²), while hit sorting is bounded O(64²). Qdrant owns no new production allocation; serde response vectors remain bounded by its existing response cap, and test-only collection strings/temp storage are released by the runner. Errors remain closed typed enums at every added boundary. The graph acquisition lease remains stack-owned SPSC: no mutex, no `Arc`, cancellation has fixed slots, and the terminal is fused after its one publication.

## Promoted

- task-local disposition: keep async acquisition, cancellation, S3/runtime, and cache machinery out of `nudox-index-trustfall`; only graph-vector may obtain and pin the borrowed view.
