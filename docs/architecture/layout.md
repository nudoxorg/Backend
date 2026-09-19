# Data structures, layout, and the work they eliminate

The target is a shared versioned object/relation engine with one typed batch format and one family of ordered arrangements. This document specifies its representation and algorithms. Sizes below are initial engineering choices to test, not measured optimums. The canonical schema and chunk policy are versioned; physical tuning must not silently change semantic identities.

## 1. Logical object granularity versus physical allocation

A logical object is a schema-defined value, not necessarily a separate heap allocation, file, or network request. A declaration can be a logical facet manifest while thousands of declarations occupy columnar segments with shared dictionaries and sparse defaults. Nodes of the Merkle map, immutable relation batches, source chunks, documentation blobs, recursive type components, vectors, and view fragments all use the same typed object/version and storage admission machinery.

The engine must not allocate a HashMap, Arc, object header, and full set of hashes for every scalar field. At one million entities, one 32-byte identifier lane alone costs about 32 MB before allocator/packing overhead. A naïve seven-facet hash vector can consume hundreds of MB without storing useful payload. Deduplicate default/empty values, keep repeated type/doc references in dictionaries, and store sparse facets only when present.

Persist one complete row/object version where the canonical map requires it. Store independently reusable large facets as separate object references. For small inline facets, derive a canonical facet fingerprint when a recipe needs it, or use a validated changed-column mask from the commit owner; do not eagerly materialize seven independent object files per entity. A physical leaf has per-column/plane fingerprints, letting equal lanes bypass decoding during diff; per-row hashes are optional accelerators under an explicit layout recipe.

All logical fields are covered by the row's complete canonical value. “Sparse” means an explicit canonical default/unavailable state, never an omitted semantic distinction. Facet selection saves downstream work; it must not allow a supposedly equal object version to conceal changed evidence.

## 2. Three coordinated structures

### 2.1 Canonical ordered map

```text
StateRoot
  schema / key order / chunk policy
  root node ID

InternalNode
  separator keys[]
  child logical hashes[]
  child row counts[]

LeafNode
  ordered logical keys[]
  complete value versions[]
```

Use deterministic key-based boundary anchors with minimum/target/maximum directory row counts. A starting design is 64/256/1024 entries for metadata leaves, with an encoded-byte hard cap and oversized keys handled explicitly. Key canonicalization must define exact order before chunking. Natural boundaries derive from a domain-separated key hash, not mutable payload contents. Parent chunking uses the same declared principle over separator keys. Tiny maps occupy one leaf; each level must strictly reduce node count.

Domain-separate anchor selection by tree level (or specify an equivalent proved hierarchical promotion policy). Reusing the same low hash bits at every level correlates parent separators with already-selected leaf anchors and can collapse intended fanout. The entire level rule belongs to the canonical cut-policy version; tuning it is not physical compaction.

Updates sort/consolidate a small mutation batch, descend to touched ranges, reuse equal subtrees, and rebuild boundary neighborhoods until canonical cuts resynchronize. Persist exactly the new canonical nodes. Same visible map + same schema/cut policy = same state root, independent of operation order.

Do not use a history-dependent split/merge hysteresis rule in this canonical tree. Hysteresis belongs in physical packing/compaction. If the canonical boundary policy changes, that is a new tree-format/root namespace even if logical row values are unchanged; compatibility conversion must be explicit.

### 2.2 Shared relation arrangements

```text
ArrangementState
  relation/schema/key-order identity
  source root bindings + logical frontier
  logical visible relation root
  physical trace checkpoint

TraceCheckpoint (LayoutId)
  immutable consolidated base segments[]
  bounded delta runs[]
  logical/physical compaction permissions
  statistics + fence/replay position
```

The trace is indexed by `(key, value, logical time)` with signed weights. It supports ordered key seek, range merge, column projection, and reusable cursors. Current snapshot reads sum the retained updates under the declared logical time. Zero weights are consolidated away only when no required observation/support information is lost.

Many queries share the same arrangement handle. A name lookup, UI outline, and graph query can share entity/header and containment arrangements rather than building one private index per feature. The recipe identifies required order, projection, and input semantics; two differently ordered indexes are distinct arrangements even when they index the same relation.

The storage strategy is immutable sorted runs with a size-tiered/leveled merge policy chosen per workload. Compaction is scheduled in bounded chunks with explicit memory and I/O credits, prioritizing read amplification and reclaimable bytes. The original [LSM-tree work](https://www.cs.umb.edu/~poneil/lsmtree.pdf) supplies the merge-structured storage basis; the engine adds versioned trace/reader-frontier laws rather than treating compaction as an unconditional background file rewrite.

### 2.3 Physical object packs and a replaceable location index

```text
Logical ObjectVersion -> Location { PackId, segment, row_or_extent }

Pack
  independent integrity header
  typed segment directories
  column pages / blob extents / dictionaries
  encoded extent checksums or hashes
```

The location index is rebuildable from pack catalogs. A pack move/recompression produces a new PackId; after safe admission, the location index may select it. Readers pin the old extent until they finish. Merkle roots refer to logical object/node versions, not physical offsets. Per-object verification recomputes canonical value identity when admitting an unfamiliar physical encoding; trusted immutable local materializations retain that proof.

Use one pack lifecycle for storage, checkpoints, sync, and remote outputs: prepare → encode → validate/admit → link → pin → retire/repack. Whole original IR image encodings become import/export materializations over the same logical semantic relations during migration. They cease to be the mandatory intermediate for every query.

## 3. Columnar microbatches

The shared batch descriptor separates hot control from cold payload:

```text
BatchHeader
  schema ID, relation ID, recipe/layout version
  base/target root or source frontier bindings
  row count, column ranges, dictionary binding

Hot lanes
  key ordinals / value ordinals
  signed weights
  shared or compressed times
  selection bitmap / initialized prefix length

Cold lanes
  logical IDs, provenance, diagnostics, long atoms/docs
  sparse extension values, variable payload offsets
```

For single-epoch batches, store time once in the header. For map transitions, put/remove presence can be encoded compactly and lower into signed weights at the compute boundary. Do not pay a full timestamp vector and a 64-bit weight on every immutable current-state row when the schema permits a narrower representation. Intermediate arithmetic still uses checked widths sufficient for multiplicity; packed widths are validated on freeze.

Execution microbatches initially target hundreds to a few thousand rows with byte ceilings, not one batch per entity and not an entire package by default. Large docs/vectors remain separately paged. Worker queues transfer a batch owner/lease, not a Vec of reconstructed entity structs. Operators borrow slices and output selection vectors or leased columns.

Use fixed-width/dictionary/bit-packed lanes where beneficial; variable columns use offset arrays and contiguous bytes. Sorted keys allow delta/prefix encoding. Per-column codecs support hot uncompressed access and cold compression independently. Canonical logical hashing uses the schema's value encoding, so changing a storage codec does not change ObjectVersion.

SIMD/bitmap kernels handle selection, key comparisons where supported, validity masks, vector arithmetic, and consolidation scans. Choose a kernel once per batch/column type. Branch on physical encoding and language shape at coarse boundaries rather than on every row. Adaptive radix or Eytzinger-style local search indexes may accelerate hot seeks without becoming canonical identity. [ART](https://db.in.tum.de/~leis/papers/ART.pdf) is a relevant implementation reference for cache-conscious adaptive indexing; adoption must earn its extra retained bytes.

## 4. Shared algorithms instead of wrapper traits

| Primitive | Used by | Replaces |
|---|---|---|
| Ordered immutable map + subtree diff | Object roots, commit state, peer reconciliation, complete relation snapshots | Full flat-root comparison and bespoke range diff loops |
| Consolidate/sort signed batches | Every incremental relation/operator | Repeated per-adapter dedup/sort/staging |
| Ordered trace cursor + key-range seek | Exact names, lexical posting joins, graph directions, docs/member joins | Repeated snapshot-specific index builders and lookup wrappers |
| Delta join/reduce/distinct/top-k support | Queries, documents, graph, ranking, UI projections | Per-feature invalidation/rebuild orchestration |
| Versioned input manifest + recipe memo | Compiler, embeddings, renders, remote tasks | Independent caches with incompatible invalidation |
| Demand/frontier lease | Queries, UI, sync, remote work, compaction | Unbounded watcher queues, ad hoc retention epochs and global invalidation |
| Pack admission/pin/retire | Local storage, checkpoints, network batches, remote results | Duplicate object/index/image store lifecycles |

This is the abstraction reduction the first plan lacked. It replaces actual algorithms and retained representations across domains. It does not require a universal `Repository<T>` facade around every old store.

## 5. Incremental operator laws

**Map/filter:** transform only changed rows, preserve weights, consolidate identical outputs. Predicate/recipe changes are input changes and may require rebuilding its affected scope.

**Join:** retain arrangements on join keys. For a delta on one side, seek matching keys on the other; simultaneous updates use a consistent old/new convention. Output is proportional to join fan-out, which can exceed input delta size. Heavy keys require partition/skew handling or a plan switch; `O(delta)` without fan-out is an invalid promise.

**Distinct/set membership:** retain support counts and emit changes only when zero/nonzero membership changes. Negative intermediate weights must not violate committed relation invariants.

**Group aggregates:** count/sum can update algebraically; min/max/ordered aggregates need per-group ordered support for deletions. Shared per-group arrangements avoid global scans.

**Top-k:** maintain ordered candidate support and boundaries. On a deletion/refined score, refill from retained candidates or a bounded range seek. Global statistics may invalidate all scores; expose the dependency and choose late scoring, incremental statistic factoring, or a deliberate approximate result mode.

**Recursion:** execute a monotone/fixed-point formulation with iteration time and proven convergence, or use an affected-region recomputation barrier for non-monotone cases. Deleting an edge in a cycle can require rederivation; counting immediate incoming support is insufficient. Bounded recursion/work diagnostics remain typed.

**Foreign/native operators:** operate on witnessed immutable input scopes and emit complete scoped replacement/delta outputs. They are nodes in the same graph, but their internal algorithm is opaque unless the authority exposes incrementality. No reader should infer changed facts solely from an incomplete semantic hash.

**Side-effect sinks:** consume a versioned intent and idempotency/fence identity once. They are not algebraic operators that “undo” a network write when a weight becomes negative. Compensation and desired-state reconciliation are explicit domain operations.

## 6. Vector search is incremental without pretending ANN is linear

Use three cooperating structures under one version contract:

1. `VectorFacts`: canonical object/version/model/input identities with immutable vector values.
2. `DeltaExact`: a small recent-insert/update overlay plus retractions/tombstones, searchable exactly.
3. `AnnBase`: an immutable or internally updateable ANN materialization tied to its indexed vector root and algorithm recipe.

A query searches the selected ANN base and recent exact overlay, rejects stale/retracted candidates, reranks against exact current vector/version facts, and returns explicit coverage/approximation. Base rebuild/merge happens in background or remotely; replacing it changes layout/materialization identity, not canonical vector values. The overlay has size/latency thresholds; if it grows beyond the declared envelope, schedule rebuild, fall back deliberately, or report a different supported query mode.

This prevents one changed embedding from forcing a global ANN rebuild while preserving version correctness. It does not guarantee exact nearest neighbors from an approximate base. [FreshDiskANN](https://arxiv.org/abs/2105.09613) is a relevant primary reference for fresh dynamic vector indexing; the concrete engine should benchmark its own supported dimensions, update rates, local memory, and recall targets.

Embedding reuse is keyed by actual normalized input bytes, tokenizer/provider/model immutable revision, dimension, metric/normalization, and context recipe. If a graph-context embedding reads neighbors, those reads enter its manifest. A “docs-only” edit may still invalidate a configured embedding; the dependency graph decides, not a hardcoded optimization.

## 7. Delta plans and recompute plans share one optimizer

For each demanded output shard, compare:

```text
reuse local object
reuse/fetch remote object
advance retained arrangement with Δ
recompute affected key/range/SCC
rebuild scope from a checkpoint
```

Estimate work from actual batch counts, changed bytes, key skew, fan-out, trace depth, warm dictionaries, authority scope, and data location. The plan is itself immutable and versioned by recipe/config; its run carries telemetry without contaminating semantic identity. Physical statistics can choose a different plan while equivalent results retain the same logical output root.

Choose recomputation when delta maintenance would traverse most of the relation or accumulate large support state. The optimizer's purpose is minimum total work and latency under memory limits, not maximum cache-hit percentage or maximum number of delta operators. Broad changes and cold starts are legitimate rebuild cases within a delta-native engine.

## 8. Rust contracts that carry this design

These are signatures to implement and compile-check during the first kernel slice, not a claim that the current repository exposes them.

```rust
// Concrete IDs have private representation and domain/schema markers.
struct ObjectKey<T: Schema> { /* stable scoped key */ }
struct ObjectVersion<T: Schema> { /* complete canonical value hash */ }
struct StateRoot<R: Relation> { /* canonical visible relation root */ }

struct Delta<'base, R: Relation> {
    // Sealed expected-base witness + ordered/consolidated change columns.
    // No public constructor from arbitrary labels.
}

trait IncrementalOperator {
    type Inputs;
    type Output: Relation;
    type State; // arrangement/support, not hidden global cache
    type Error;

    fn step(
        state: &mut Self::State,
        inputs: Self::Inputs,
        scope: &mut WorkScope,
    ) -> Result<PreparedDelta<Self::Output>, Self::Error>;
}

trait BatchSource {
    type Batch<'a> where Self: 'a;
    fn next(&mut self) -> Option<Self::Batch<'_>>;
}
```

Use GAT lending cursors so an operator cannot reset its scratch while a consumer still borrows the previous batch. A higher-ranked session binds a fresh invariant batch/dictionary brand to actual immutable backing; typed `LocalRow<'batch, R>` remains compact. Persisted keys remain stable owned identities and reenter through checked lookup. Branded IDs do not justify unchecked bounds or confer publication authority.

Use typestate at real transitions: unvalidated input → admitted canonical batch → prepared delta → committed state; executing pure job → prepared result → accepted result. Consume mutable buffer capabilities when freezing them. Return reusable buffers through leases only after the last immutable pin drops. `AsRef<[u8]>` is not a blanket immutability contract.

Monomorphize hot operator/column kernels over a finite set of meaningful types, and erase at job/protocol/plugin boundaries. Do not encode the entire dynamic query graph in one Rust generic type; it would explode compile time and make runtime graph reuse difficult. The graph IR is compact data whose node selects a specialized kernel.

Schemas centrally define canonical encoding, column projection, equality/hash coverage, and reference interpretation. Generate repetitive mechanical lane/codec dispatch from that definition. Keep language interpretation and relational algorithms handwritten. This avoids the current multi-file schema drift while retaining inspectable logic.

## 9. Memory reuse, compaction, and work partitioning

Use worker-local slabs for mutable batches, shared immutable segments for arrangements, and a bounded size-class pool for recycled columns. Consolidation reuses sort/key/weight buffers; untouched columns borrow the prior immutable segment. A changed column may be copy-on-write while the segment manifest references unchanged columns. Batch dictionaries are built once and reused across operators where compatible.

Partition by stable key range/hash according to operator semantics. Put the same-key state with the same owner; move immutable shard checkpoints plus subsequent deltas when rebalancing. Workers operate on disjoint shard state and borrowed shared inputs; cross-worker messages carry leased consolidated batches. Hot-key subdivision needs a merge law, especially for aggregates/top-k, rather than arbitrary splitting.

Reserve distinct interactive, foreground/background, transfer, and compaction envelopes. A compactor must not pin every old/new segment and wait for output space indefinitely. Use a bounded merge workspace and publish replacement extents atomically after validation. Account simultaneously live old and new segments until readers release the old layout.

Trace compaction advances only to the meet permitted by all active observers/operators. Durable historical roots live in the object store; a UI undo history should not keep every historical execution timestamp in every hot arrangement. To inspect an old snapshot outside the retained hot frontier, reopen its immutable checkpoint or replay in a separate bounded session. This separates history richness from permanent RAM growth.

Delta chains/runs are bounded by policy for read amplification. A limit triggers compact/rebase work or admission backpressure; it does not delete history a consumer still needs. Slow subscribers can receive a new complete view root and a reset message instead of an unbounded queued delta backlog.

## 10. Required law suite and reference model

The small [reference model](../prototypes/README.md) tests these laws independently
of the backend:

- Same visible map, different insertion orders → same canonical state root.
- Value-only change → unchanged key cuts and shared unaffected subtrees.
- Exact-base delta apply, composition, inverse, and stale-base rejection.
- Consolidated weighted updates equal whole-state reconstruction; simultaneous join changes include the cross term correctly.
- Physical repacking leaves logical roots unchanged.
- Frontier compaction preserves all still-permitted observations.

It is a correctness model, not the optimized kernel or a performance benchmark. Production validation additionally covers faulted storage, Miri/initialized-drop laws where applicable, concurrent frontier/pin tests, native authority parity, stale remote results, and million-entity edit workloads. Measure node visits, changed bytes, emitted/support rows, allocation/retained bytes, physical write amplification, remote bytes, and end-to-end latency separately.

## 11. Factorize retained computation, not just stored bytes

The end-state flow engine should support **factorized view plans and selected higher-order delta views** alongside ordinary flat batches. Merely storing a huge join result more compactly after computing every pair misses the main opportunity. [F-IVM](https://arxiv.org/abs/2303.08583) combines factorized computation and higher-order maintenance through hierarchies of views. [DBToaster](https://vldb.org/pvldb/vol5/p968_yanifahmad_vldb2012.pdf) shows how materialized delta queries can support each other's updates. Those results motivate an additional plan family; their reported speedups are not predictions for this repository.

For this backend, represent reusable grouped results as immutable prefix/factor nodes over the same logical object references and column batches. A factor node names its variable/key order, child factors, multiplicity or aggregate payload, input roots, recipe, completed execution frontier and coverage. An index/view recipe determines a factorization; it does not introduce a second semantic relation. Expanded tuples must equal the declared flat relation under set/bag/order semantics.

`FactorRoot = H(factor_schema, key_order, support_semantics, recipe, source_roots, completed_frontier, coverage, child_factor_roots, payload_versions)`. Its retained trace also binds `Upper` and `Since`. A remote factor delta names exact base/target FactorRoots and the input transition/frontier it advances. These IDs are explicitly derived materialization identities; they are not `StateRoot<R>` for an expanded flat relation whose canonical root has never been computed. A structurally valid factor at an incomplete frontier cannot satisfy complete demand.

Consider a query joining declarations to both documentation terms and outgoing references. Flattening `Terms(entity, term) ⋈ Edges(entity, target)` can create a term×target product per entity before a downstream aggregate discards most columns. A factorized plan keeps shared entity prefixes with term and target factors, computes separable aggregates without constructing the Cartesian product, and expands only the demanded projection/window. Correlated filters or requested full tuples may require expansion; output-size lower bounds still apply. The planner cannot compress away results a client explicitly requests.

For repeatedly updated three-way queries, retain a useful intermediate derivative view when it substantially reduces future delta probes. Updating a different input also incurs maintenance of that derivative, so select the hierarchy using update rates, key skew, demand and retained-byte cost. This is a plan chosen by the existing optimizer, not an independent cache hidden in each operator. Limit derivative order and plan enumeration; unrestricted higher-order materialization can consume more memory than recomputing.

Persist factor/checkpoint nodes with the same store admission, roots, source bindings and pin laws. Share equal factors across query recipes only when their schema, key order, support and input semantics match. A recipe's logical expanded result root must not silently change because the planner picked another factorization; use a recipe/layout-bound materialization identity until canonical logical-result equality is established. No giant expansion is required merely to claim an unneeded flat StateRoot: the factorized view may expose its explicit representation/equivalence contract and defer a canonical expanded root to the operation that requires it.

Local and remote placement now operates on factor/arrangement shards. A remote retaining the target factor can accept a small edge delta and return a changed aggregate/window factor. It need not receive the complete join inputs again or send an enormous flattened intermediate. Factorized work is especially useful for graph summaries, member/type/document projections and repeated analytical queries; ordinary direct name lookups stay simple.

Payload arithmetic is typed. Integer count/weight operators use checked exact arithmetic. Floating reductions must specify deterministic order or an explicit numerical equivalence/tolerance contract; algebraic ring rewrites are not automatically bitwise-valid IEEE-754 transformations. Nonlinear ANN and side effects remain their explicit operators. This keeps factorization an algorithmic extension of one engine rather than a false universal algebra.

## 12. Incrementally maintain the machinery that decides reuse

The dependency manifest is itself a versioned ordered relation. Maintain shared reverse arrangements:

```text
Readers(object_or_facet_key, recipe_shard, observed_version)
RangeReaders(relation, interval_or_prefix, recipe_shard, observed_membership)
RecipeUsers(recipe_version, output_shard)
AuthorityUsers(authority_policy, receipt_or_view)
```

A commit joins its changed key/facet/range set with these arrangements to find candidate invalidations. It does not scan every cached result's dependency list. Positive and negative/range reads both participate. A content-identical producer result cancels propagation before deeper recipes are scheduled. A shared source/provenance manifest can advance while type-only readers keep equal observed facet versions.

Validate large unchanged read sets by reusing Merkle subrange summaries, with checked coverage of the actual read footprint. A summary covering a superset can produce conservative false invalidation but cannot justify ignoring a changed read. Updating only intersecting summaries lets the system avoid spending a full compile's worth of dependency checks to discover a cache hit. Range boundary changes and dynamically discovered dependencies remain budgeted work.

Deduplicate the dynamic graph by `(recipe, selected parameters, validated inputs)` and intern compatible operator subplans. Maintenance of the dependency and demand relations uses the same batch/consolidation/arrangement primitives, with a control epoch distinct from authoritative history. This is where the unification becomes recursive in a useful sense: the engine uses its change machinery to avoid redoing change discovery. It must still have a finite control loop and explicit dirty→validated→ready transitions; a recipe cannot validate its own output by a circular dependency claim.

Fuse compatible stateless column operations into one batch kernel so intermediate selection masks and borrowed columns stay in worker-local scratch. Dispatch once on the batch's validated schema/encoding, specialize over finite kernel families, and place a boundary at stateful joins, authority calls, resource yields and durable publication. Cooperative byte/row time slices preserve cancellation and interactive fairness even inside a fused kernel. This reduces branches, queue hops and materialization while leaving the dynamic work graph small and inspectable.

These features belong to K5/K6/K11 after the shared trace is real. They deepen the same chosen representation and optimizer; they are not a parallel framework or a requirement that every product operation become a factorized query.
