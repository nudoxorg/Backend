# Semantic changed-key frontier audit

This audit records what the current compiler and row-index code can prove
about semantic edits. The target contract remains
[`semantic-history-and-work-avoidance.md`](../architecture/semantic-history-and-work-avoidance.md).
The current code has stable row identities and efficient persistent-tree
updates, but it does not yet have a compiler-owned proof that a sparse list of
changed rows is complete. An empty list is therefore a zero-work update claim,
not evidence that compilation was a semantic no-op.

## Current producer and index path

`backend-engine::driver::compile_semantic` collects one authority transaction,
materializes an owned `Ir`, then writes the compact fragment from the same
facts. `compile_ir` is an owned-image compatibility entrypoint and performs a
separate authority transaction if called beside `compile`. The owned image
contains final facts, not the producer's per-read dependency graph or a delta
from a previous image. See `crates/engine/src/driver/README.md` and
`crates/engine/src/driver/types/compile.rs`.

`CorePayloadHash` currently covers declaration shape, type structure, ordered
product children, and locally bound members. Its own coverage value explicitly
excludes documentation, visibility, language extensions, source provenance,
occurrences, and opaque parentage. A missing member-set capture is also not
evidence of an empty member set. This hash is useful for its declared core
contract; it cannot prove all-family row reuse. See
`crates/semantic/src/ir/semantic/relations.rs` and
`crates/engine/src/driver/README.md`.

The canonical plane encoder walks each reader family, collects stable keys,
sorts the key/handle pairs, encodes every row, and writes the segments. Its
family oracle repeats that encoding and compares exact output. Separately,
`StableRowIndex::prepare_update` trusts the caller's ordered row changes, and
`prepare_payload_update` hashes only the supplied payloads. Both can path-copy
sparse edits and reuse equal subtrees, but neither receives a completeness
capability or checks that a producer did not omit a changed key. The current
call-site search found no production caller of either row-index update method;
the implementation is a substrate and its current usage is in its tests and
benchmark. See `crates/semantic/src/ir/versioned_records.rs` and
`crates/semantic/src/ir/row_index.rs`.

V2 content identity already has an aggregate verifier for the exact census
and cross-family closure of all seven normalized-reachable row families. Its
generation identity adds build identity and an input/read-frontier witness.
`SemanticInputWitness::claimed` is deliberately not authorized; `admitted`
requires a live complete `CoverageWitness`. This closes an input-admission
boundary when a trusted authority has produced that witness. It does not
produce semantic changed keys. The compiler driver currently returns no such
previous/next frontier pair. See
`crates/semantic/src/ir/semantic_generation.rs` and
`crates/semantic/src/ir/versioned.rs`.

## Dependency model for a complete semantic frontier

Every domain below must be represented in the producer's dependency closure.
The seven row families are exact output families; the final column includes
dependencies that must invalidate those rows or adjacent products.

| Domain | Current output family or product | Change that must be visible |
| --- | --- | --- |
| Declaration identity and shape | Core | Insert, replacement, deletion, parentage change, or rename. A rename is a tombstone plus a new stable key, even if payload bytes happen to match. |
| Type structure and type references | Types and Core; type references in language extensions | A changed reachable node invalidates rows for every dependent declaration and any extension row seeded by that type. Reachability must include roots from declarations and all selected language extensions. |
| Graph relations and occurrence sites | Relations and Occurrences | Endpoint, kind, resolution, confidence, source-site, and multiplicity changes. Removing a declaration can tombstone incident relations and occurrences. |
| Documentation | Documentation | Captured-empty versus unavailable, fragment order/content, and documentation links. A docs edit must also invalidate embedding inputs derived from docs. |
| Source identity and provenance | SourceProvenance and occurrence source fields | File identity, path, span, or captured/unavailable status. Source offsets may change while declaration identity remains stable. |
| Language extensions | LanguageExtensions | Profile-owned facts, ordered lists, absent/present state, and every type reference embedded in an extension row. A profile/toolchain change is also a build-identity change. |
| Embedding inputs | Separate embedding plane | Rendered signature, documentation, graph context, model/recipe, and normalization. Embeddings are not one of the seven IR families, so their invalidation must remain separately bound to the embedding identity. |
| External inputs and negative reads | Generation input witness | Positive file reads, missing-path probes, directory listings, environment, toolchain, generated artifacts, and classified external mounts. An unseen negative read can change resolution when a formerly missing candidate appears. |
| Purity and nondeterminism | Build and input admission | Clock, network, randomness, mutable services, or unclassified child-process reads make reuse ineligible unless the exact values are captured in the build/input authority. |

The source of these families is the typed encoder set in
`crates/semantic/src/ir/versioned_records/`. The read-manifest layer already
models scoped positive and negative observations in
`crates/semantic/src/read_manifest/`; that is a separate axis from semantic
row invalidation. Input completeness cannot fill a missing type or row edge in
the compiler's semantic dependency graph.

## Crate graph

The production dependency direction relevant here is:

```text
backend-engine -> backend-semantic, backend-flow, frontends/*
backend-semantic -> backend-flow, backend-version
backend-flow -> backend-version, backend-store
```

`backend-flow` is the differential dataflow and arrangement runtime. Its
regular dependencies do not include the compiler driver. Its dev-dependencies
include semantic, engine, and the frontends because `crates/flow/tests/`
contains compiler-corpus journeys. The flow crate's frontier types and
arrangement deltas concern dataflow time and relation changes; they are not
the producer-owned compiler read frontier or semantic changed-key frontier.
This graph was read from the crate manifests; Cargo metadata was not run during
this audit.

## Independent oracle and proof boundary

The stable-index tests already compare index diffs with a separate `BTreeMap`
oracle, exercise all seven family boundaries, replay forward and reverse
deltas, and check exact root admission from complete records. This audit adds
adversarial cases for rename-as-delete-plus-insert, cold reconstruction,
dependent rows omitted from a claimed type edit, and an empty claim against a
changed documentation row. These cases validate row-index mechanics and show
what a complete producer claim must include. They do not assert that current
compiler observations can mint a complete frontier.

The important distinction for production is therefore:

- A full canonical-reader scan can be an independent output oracle.
- A producer-owned sparse frontier needs a separately admitted owner receipt
  tied to the exact old/new generation authorities and complete semantic
  dependency closure.
- Only after that receipt is verified may an empty frontier authorize a
  semantic no-op or sparse edits authorize persistent-tree path reuse.
- If the receipt, a domain, a negative read, an external dependency, or a
  process observation is unsupported, the candidate must fall back to the
  canonical full scan.

No production work counters were collected: the worktree had multiple active
machine-wide Cargo builds and the task limit prohibits starting Cargo while
more than three are active. Existing counters expose tree rows/nodes, reused
nodes, encoded bytes, decoded rows, and row-payload hash bytes. In the added
tests, the expected accounting is symbolic: rename changes two keys, the
mixed seven-family edit changes seven keys, and the intentionally omitted or
empty claim fails the full-scan root comparison.
