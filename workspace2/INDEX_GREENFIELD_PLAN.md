# Greenfield immutable index plane

This is a new system, not a migration of `workspace/index`. The old catalog, Dolt/SQLite split,
SeaORM entities, outbox, registry facade, Tantivy wrapper, Qdrant routing, server composition, and
DTOs have no compatibility standing. They are evidence that catalog, search, coordination, and
serving were allowed to become one dependency graph. Preserve none of those boundaries by default.

The product capability is narrower and stronger: publish immutable facts once; derive independently
replaceable indexes from sealed deltas; answer exact, text, vector, relation, and usage queries against
a pinned snapshot; and return honest partial results when only some immutable segments are reachable.
The same segment bytes and query semantics work on a laptop, an NVMe search node, or object storage.

## Laws

1. Canonical object and generation bytes are truth. Every index is a disposable projection whose
   identity includes the exact input root, schema, projection recipe, and encoded bytes.
2. A query pins one `IndexSnapshotId`. No leaf may silently answer from another head. Stale routing
   can cost work, never correctness.
3. A snapshot is a small immutable manifest of typed segment references. Publishing a snapshot is one
   durable compare-and-swap head transition after every referenced artifact is durable.
4. Exact catalog, lexical, vector, relation, and usage data are different `SegmentFamily` values.
   They share publication and range-access machinery, not payload schemas or giant request objects.
5. Segment files are immutable, independently searchable, content-addressed, and range-readable.
   Update and deletion are new delta/tombstone segments. Compaction publishes equivalent replacements.
6. The hot path validates a segment once and then borrows typed regions. It does not deserialize a
   document forest, allocate one object per row, or copy mmap/network bytes into a second cache.
7. Query cost is bounded by selected segments, touched ranges, and requested `TopK`; corpus size is not
   implicit work. Metadata must make pruning observable before payload I/O.
8. Horizontal placement is advisory. Rendezvous affinity chooses likely warm workers; any healthy
   worker can reconstruct truth from the snapshot and object store.
9. Local operation is not a degraded implementation. A local store may hold a prefix of the same
   snapshot, answer from proven segments immediately, and name the exact missing segments/ranges.
10. Text and vector relevance are versioned recipes with deterministic tie-breaking. A backend score
    is not canonical identity and cannot leak as an unconstrained `f32`.
11. Compute is synchronous over borrows. Object/file/network latency is asynchronous and yields
    bounded owned leases. A query never stages a whole remote segment merely to inspect one posting.
12. Caching is an explicit projection product: admission, retained bytes, eviction value, and rebuild
    source are typed and measured. “Put a cache in front” is not an architecture.

## Source topology

The index lives in a nested workspace so the portable root cannot accidentally inherit servers,
object-store SDKs, embedding runtimes, or query engines.

```text
planes/index/
  Cargo.toml
  crates/
    nudox-index-vocab/       no_std closed schema/query/segment vocabulary
    nudox-index-format/      no_std canonical manifest and segment wire records
    nudox-index-view/        borrowed validated manifest/segment views and cursors
    nudox-index-build/       caller-arena builders, external sort, compaction equivalence
    nudox-index-query/       sync borrowed planners, posting cursors, merge/top-k
    nudox-index-publish/     std durable snapshot log/head protocol
    nudox-index-testkit/     non-shipping deterministic drivers and mutation corpus
  adapters/
    file/                    mmap and file-range leases
    object-store/            remote range/read/write and publication CAS
    lexical-tantivy/         optional segment builder/reader experiment, never core truth
    vector-qdrant/           optional derived-view adapter, never snapshot truth
    server/                  async fan-out, admission, health and OTEL
```

A crate exists only when it owns a replaceable invariant. `lib.rs` maps modules and reexports a small
vocabulary. Format, validation, planning, execution, publication, and adapters do not share a root
implementation file.

## Identity and vocabulary

Add protocol-owned domains through the existing declarative ID registry, never naked hash literals:

```text
IndexDeltaId       = ContentId<IndexDeltaDomain>
IndexSegmentId<F>  = ContentId<IndexSegmentDomain<F>>
SegmentArtifact<F> = ArtifactId<SegmentEncoding, IndexSegmentDomain<F>>
IndexSnapshotId    = ContentId<IndexSnapshotDomain>
IndexRecipeId      = ContentId<IndexRecipeDomain>
EmbeddingModelId   = ContentId<ModelDomain>
```

`SegmentFamily` is a closed protocol enum: `Exact`, `Lexical`, `Relation`, `Usage`, and `Vector`.
Family-specific marker types prevent passing a vector segment to an exact lookup. Raw codes use
`From<SegmentFamily>` and `TryFrom<SegmentFamilyCode>`. `SnapshotOrdinal`, `SegmentOrdinal`,
`DocumentOrdinal`, `TermOrdinal`, `PostingBlock`, `TopK`, `Score`, `ByteRange`, `PartitionKey`, and
`CompactionLevel` are semantic newtypes with direct representation ergonomics. Decorative `.get()`
wrappers are forbidden.

The first schema registry is declarative and compile-time checked. It generates field tags, family
membership, fixed metadata, exhaustive matches, and golden registry enumeration. It must not generate
an ORM, a builder per field, or a dynamic reflection layer.

## Canonical input: sealed index deltas

The compiler/publication plane emits an `IndexDelta` for one sealed generation transition. A delta
contains sorted semantic keys and references to canonical objects or IR fragments; it never embeds
backend documents. Deletes and replacements are explicit typed changes. The delta identity binds:

- prior and resulting generation roots;
- projection schema and recipe;
- sorted changed semantic keys;
- referenced immutable content IDs;
- source compiler/toolchain facts where the projection depends on them.

Index construction consumes deltas, not a corpus rescan. Builders may batch compatible deltas, but
the produced segment records the exact covered delta interval so idempotency and compaction are
provable.

## Snapshot and segment structure

An index snapshot is a packed, borrowed manifest with these logical lanes:

```text
snapshot header: schema, recipe, generation, parent snapshot, segment count
segment directory: family, partition span, level, artifact id, logical id, length
pruning lanes: key min/max, field presence, bounded bloom/Xor filters, model/metric facts
replacement lanes: compact old-segment -> replacement-set relation
```

The manifest contains enough metadata to select candidate segments without opening each segment. It
does not contain node assignment, S3 URLs, cache state, health, or mutable optimizer status.

A segment has one typed envelope and family-owned regions. Every region has one wire-record authority;
layout derives from counts and field widths. The common envelope supplies logical input identity,
family, recipe, covered key interval, row count, region directory, and optional authenticated range
metadata. Family regions begin as:

- **Exact:** sorted fixed-width semantic key table, compact value descriptors, and optional FST for
  variable UTF-8 keys. Binary search/FST lookup returns borrowed value ranges.
- **Lexical:** FST term dictionary, block-addressed delta/bitpacked postings, optional frequencies and
  positions, and compact column lanes used by ranking/filtering. Postings are ordered by a dense local
  `DocumentOrdinal`; global identity stays in a separate sorted descriptor lane.
- **Relation/usage:** sorted adjacency offsets plus delta-coded typed targets. Reverse and forward
  direction are separate recipes rather than a flag interpreted in every loop.
- **Vector:** model/dimension/metric/quantization facts plus vector and graph/cluster regions. This
  family is deferred until its exact and approximate recall contract is independently testable.

A segment footer/hot header names the minimal ranges required to open and plan against a cold object.
It is a first-class range in the artifact, not an opaque in-memory cache serialization.

## Query algebra

Do not recreate a universal request DTO. Queries are closed typed operations:

```text
ExactLookup<KeySpace>
PrefixScan<KeySpace>
LexicalSearch<Recipe>
RelationWalk<Direction, EdgeKind>
UsageLookup<UsageKind>
VectorNearest<Model, Metric>
Federated<Query>
```

Concrete query values expose validated fields directly. A generated closed `AnyQuery` is permitted
only at the application boundary; it dispatches once into monomorphized family code. There is no
public `dyn Query`, no boxed stream, and no backend enum match inside posting/document loops.

Planning is a pure function of a pinned borrowed snapshot, a typed query, and explicit limits. The
result is a compact `QueryPlan` containing selected segment ordinals, exact warmup ranges, merge order,
and physical credits. Execution cannot discover an unbounded fan-out after admission.

Leaf execution returns ordered batches carrying `SegmentId`, local ordinal, semantic key, typed score,
and provenance. The root performs a deterministic bounded k-way merge. It emits one terminal fact:
`Complete`, `Partial { missing }`, `Cancelled`, `Degraded { reason }`, or `Failed`. Missing leaves are
data, not a zero-hit result.

## Horizontal scale without distributed truth

The durable sequence is:

1. write every segment artifact to durable immutable storage;
2. append a publication record naming the complete new snapshot;
3. sync and obtain a stable receipt;
4. compare-and-swap the compact snapshot head;
5. expose the new `PublishedIndexSnapshot` capability.

Search nodes watch the publication log, retain selected hot headers/segments on NVMe, and may mmap
popular complete artifacts. Rendezvous hashing ranks workers for each segment using ephemeral node
membership, but the request names the snapshot and segment IDs. A worker that lacks a segment faults
the declared ranges from durable storage or returns an exact absence. A coordinator retries another
worker without changing semantics.

Compaction is a content-addressed job over an explicit input segment set. One worker builds the output
once; replicas download it. Publication atomically replaces the set in a later snapshot. A verifier
proves that exact-family key/value results and lexical document sets are equivalent before the old set
becomes collectible. Node-local repacking is forbidden.

Partition keys come from canonical query-pruning facts such as ecosystem, tenant, or semantic key
prefix—not node count. Resharding publishes new immutable segment sets. The system does not put every
point mutation through cluster consensus, nor does correctness depend on replicas converging quickly.

## Local-first projection

A client stores the latest pinned snapshot manifest plus demand-selected segments or ranges. The local
query engine is the same borrowed engine compiled with smaller static policies. It can:

- answer exact and lexical queries from present segments;
- enumerate precisely which selected segments/ranges are absent;
- request hot headers before bodies and coalesce adjacent ranges;
- retain current roots and demanded payloads under explicit byte credits;
- expand capability during remote outage and evict disposable projections when pressure returns.

Optional index bundles are immutable artifacts with target/feature/dependency metadata. They are not
plugins loaded into the portable core through trait objects. The base client never links server,
Qdrant, Tantivy writer, object-store SDK, OTEL exporter, or embedding model dependencies.

## Memory, compute, concurrency, and SIMD

The allocation ladder is borrowed range, caller scratch, const-inline storage, typed arena/slab, and
finally exact fallible ownership. FSTs, descriptor lanes, postings, and columns are read from original
mapped/leased bytes. `Vec` is acceptable in builders only with an explicit lifetime and upper bound;
finished segments do not retain builder graphs.

Build workers own mutable arenas and external-sort runs. Query parallelism is request-scoped and
borrows one immutable snapshot. Cross-thread ownership uses scoped tasks where possible; `Arc` is not
the default query handle. Shared admission queues are bounded and may claim lock-freedom only after a
linearization, memory-order, reclamation, Loom, and receiver-concurrency proof.

SIMD is candidate-only for measured block posting decode/intersection, bitset/rank operations, and
vector distance. Every `fearless_simd` kernel has a scalar oracle, runtime feature dispatch outside the
hot loop, adversarial tails/alignment tests, and a recorded crossover. Segment construction, identity,
and ordinary lookup remain scalar unless measurement proves otherwise.

## Observability

Typed lazy probes cover snapshot pin, segment prune/select, range request/receipt, leaf start/terminal,
merge terminal, admission rejection, and compaction publication. Stable IDs and bounded numeric facts
are fields; query text, semantic keys, and provider strings are not metric labels. Disabled probes do
not format or allocate. Server adapters batch OTEL export independently from data-plane credits and
expose exporter health through the caller-owned lifecycle capability.

## Phase graph and manager slices

Each phase is independently useful, has one Terra manager, and is implemented by explicitly selected
real-Luna workers in isolated branches. Plan completion is 8/10; 9–10 require measured same-direction
stretch, never extra features.

### I0 — greenfield vocabulary and snapshot manifest

Build the nested workspace, typed identities, segment families, packed snapshot manifest builder/view,
binary-searchable segment directory, exact mutation corpus, zero-allocation borrowed validation, and
one local manifest scan. No query engine, publication adapter, text/vector backend, or async I/O.

### I1 — exact segment and local query

Build one exact-key segment format from a sorted caller-owned input, borrowed lookup/prefix cursor,
typed complete/partial terminal, and manifest→segment integration. Prove pointer containment, zero
hot-path allocations, O(log n + output) comparisons, and no revalidation/reparse.

### I2 — durable snapshot publication

Use the durable journal substrate for immutable segment publication and a compact CAS head. Stable
receipt is the only constructor of `PublishedIndexSnapshot`. Crash every prefix; prove no head names
missing bytes and compaction replacement is atomic.

### I3 — leased range and local/remote parity

Open manifests and exact segments through bounded file and simulated-network range leases. Plan first,
fetch only named ranges, handle reordering/cancellation, and run identical queries with all-local,
mixed, and remote-down availability.

### I4 — lexical segment

Add generated field vocabulary, FST dictionary, block postings, deterministic scorer/tie order, bounded
top-k merge, and split pruning. Compare the focused format against a Tantivy adapter; adopt code only
when memory/work evidence wins, never its document API by inertia.

### I5 — horizontal execution and compaction

Add stateless root/leaf fan-out, rendezvous affinity, stale routing recovery, shared compaction output,
NVMe/object-store tier controller, node loss/hot-key/rebalance tests, and exact work/byte telemetry.

### I6 — relation, usage, and vector families

Land each family as a separate rubric/slice. Vector work begins with model-typed exact scalar search,
then measured SIMD, then approximate structures with recall/error contracts. Qdrant is only a
differential/adapter candidate.

## Acceptance matrix

A score of eight requires all applicable evidence below:

- golden manifest/segment bytes and registry uniqueness;
- truncation at every boundary and targeted mutation of every header/directory field;
- exact pointer containment, allocation count, retained bytes, range bytes, comparisons, decoded
  blocks, branches, and high-water credits;
- empty/one/density-cliff/100k/million-row profiles and equal-key/tie boundaries;
- deterministic ordering across input permutations, builders, worker counts, and compaction layouts;
- complete/partial/cancel/degraded/failed terminals with no empty-success substitution;
