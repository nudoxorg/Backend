> **Research, not the final specification:** use the [final v2 index](../architecture/README.md). These reports preserve alternatives; final v2 equations, package boundaries and corrections take precedence.

# V2: one versioned relation and arrangement engine

## Decision

Replace duplicated exact, lexical, graph, document, embedding, shadow, and cache lifecycles with one append-only versioned relation log and a family of persistent arrangements over the same typed rows. The engine is a local-first incremental view-maintenance runtime. A remote is a worker and durable pack peer, never the only authority. Existing `server/index` behavior remains the differential oracle during migration.

The design borrows differential dataflow's weighted changes and partially ordered logical time, DBSP's algebraic incremental view maintenance, LSM's write/merge separation, and prolly/Merkle structural sharing. Differential dataflow explicitly retains indexed updates at multidimensional timestamps for reuse across input and iterative dimensions ([CIDR 2013 paper](https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf)); its Rust documentation also requires advancing and flushing input frontiers before progress and permits buffered temporal batches ([timelydataflow book](https://timelydataflow.github.io/differential-dataflow/chapter_3/chapter_3_4.html)). DBSP presents a general incrementalization method for rich relational and recursive queries ([DBSP paper](https://arxiv.org/abs/2203.16684)).

The proposed engine is deliberately narrower than a general dataflow system: typed deltas, deterministic arrangements, bounded local execution, and explicit approximation barriers. It should not import a heavyweight runtime until measurements prove the need.

## One data model

Every logical fact is a row in a versioned relation:

```rust
struct Row {
    object: ObjectId,       // stable object/version coordinate
    relation: RelationId,   // Name, Exact, Link, Document, Embedding, ...
    key: KeyRef,            // canonical bytes or interned ID
    value: ValueRef,        // column-specific payload
    valid: Validity,        // present or deleted
}

struct Delta {
    key: RowKey,
    value: Value,
    time: LogicalTime,      // epoch, source sequence, sub-epoch
    diff: i64,              // +1/-1 or aggregate multiplicity
    proof: AuthorityProof,  // source image, generation, signature, recipe
}
```

`RowKey` is `(relation, object, key)`; updates are represented as `-old +new` at one logical time. A deletion is a durable negative delta, never physical absence in the log. Each batch carries source sequence, parent root, recipe/schema version, and a digest over canonical encoded deltas. A signed batch is accepted only after verifying the source authority, parent frontier, signature, and per-relation schema. Local ingestion can produce batches; remote batches can be replayed idempotently by digest.

Use a total `Epoch` for externally visible versions and a product timestamp `(epoch, source_partition, iteration)` internally. The product permits parallel source partitions and recursive graph rounds while snapshots expose only closed epochs. A frontier is the antichain of minimum incomplete times. A query at epoch `e` may publish only when its dependency frontier is strictly greater than `e`.

## Physical layout

The durable engine has four layers:

1. A journal of signed delta batches, content-addressed by canonical bytes. Journal records are length-bounded, checksummed, and fsynced before the local head advances.
2. A mutable microbatch, columnar and private to one writer. It contains sorted keys, relation/object IDs, offsets into a byte arena, validity bits, times, and diffs. It is sealed at a row, byte, or latency threshold.
3. Immutable arrangement runs. Each run has a Merkle/prolly root, a sorted key index, compressed hot columns, cold payload blocks, a deletion ledger, and a min/max time frontier. LSM trees provide the right mental model: buffer writes, flush sorted runs, compact in the background ([O'Neil et al., LSM-tree](https://www.cs.umb.edu/~poneil/lsmtree.pdf)). Prolly trees are useful for content-addressed ordered snapshots and structural sharing ([prolly Rust implementation](https://github.com/crabbuild/prolly)).
4. Query operators over arrangement cursors. Operators consume positive and negative weights and emit deltas or a materialized terminal. A query cache is an arrangement keyed by `(query_plan_digest, input_roots, frontier, recipe)` rather than an independently invalidated mutable table.

Hot columns are fixed-width and SoA: relation (u16), object ordinal (u32/u64), key hash (u64), value offset/length (u32/u32), time (u64), diff (i32/i64), flags (u8). Cold columns hold variable bytes, source spans, documentation, graph metadata, and embedding blocks. A row is 32–48 hot bytes before payload references. For `n` rows, hot memory is approximately `n * H + 8 * B`, where `H` is hot-row width and `B` is column/block metadata count. Keep `diff` signed and widen during aggregation to avoid overflow.

Intern immutable atoms, package coordinates, relation names, and repeated declaration identities in a content-addressed dictionary. Interning is safe only within a root/schema namespace; the row still commits the canonical bytes or dictionary digest. Never intern mutable strings globally: that turns eviction into semantic invalidation.

## Arrangements replacing duplicated indexes

One relation can have several physical arrangements, all derived from the same delta stream:

| Arrangement | Key/order | Replaces | Exactness |
|---|---|---|---|
| primary | `(object, property, time)` | catalog/version rows | exact |
| exact-key | `(exact_key, object, time)` | exact segments | exact |
| term membership | `(term, object, time)` + score columns | lexical segments/Tantivy shadow table | exact membership; backend scoring optional |
| link forward/reverse | `(source, relation, target)` and reverse | graph CSR/Trustfall graph tables | exact closed epoch |
| document | `(object, field, token)` | document/source lookup | exact if source plane is present |
| vector base | `(model, metric, object)` | immutable vector segments | exact coordinates, approximate neighbor result |
| ANN graph | `(model, metric, object)` adjacency blocks | Qdrant/graph vector projection | approximate, versioned |
| stats | `(query key, epoch)` aggregates | global scoring/cache state | exact aggregate, recipe-dependent |

The relation log is the sole invalidation source. A changed name emits deltas to term/document arrangements; changed links emit link deltas; changed embedding coordinates emit vector deltas. Exact-key, term, graph, and vector outputs can therefore share object/version authority without sharing their algorithms.

## Ingestion and signed delta protocol

The pipeline is:

```text
source object/version
  -> canonical typed delta batch
  -> verify parent/root/signature/schema
  -> append journal
  -> merge microbatch
  -> seal run at frontier
  -> arrange deltas and advance frontier
  -> publish local snapshot / ship pack remotely
```

Canonical encoding sorts `(relation, key, object, time, diff)`, length-prefixes bytes, and excludes transport headers. `batch_digest = H(schema || parent_root || frontier || canonical_deltas)`. A signature covers the digest and source identity. Apply is idempotent: an already seen digest is acknowledged; a same sequence with another digest is a fork requiring explicit merge policy.

The deletion ledger retains `(row_key, removed_value_digest, delete_time, source_batch_digest)`. Compaction can drop a negative delta only when every retained reader frontier is beyond its time and the positive predecessor is unreachable from all retained roots. This is stronger than a tombstone TTL and prevents a late replica from resurrecting data.

## Incremental operators

Core operators are `map`, `filter`, `join`, `reduce`, `distinct`, `top_k`, and `iterate`. Each has a delta rule. For a join `R ⋈ S`, an incoming `ΔR` contributes `ΔR ⋈ S`; `ΔS` contributes `R ⋈ ΔS`; simultaneous changes include `ΔR ⋈ ΔS` exactly once. A grouped sum stores `(key, sum, count)` and applies `sum += diff * value`, `count += diff`.

Top-k is not a plain group-by. Maintain per-query-key ordered candidates plus a boundary certificate. A candidate insertion/deletion only affects the output if it crosses the current boundary; otherwise update the hidden ordered set. For score `s(x,q)`, deterministic order is `(-score, object_id)`. If scores are additive and local, a bounded heap with a loser boundary is sufficient. If scores include global IDF/normalization, maintain global statistics as another arrangement and issue a new ranking epoch whenever those stats change:

`score(t,d,q) = tf(t,d) * (log((N + 1)/(df(t) + 1)) + 1) * field_weight`.

An update to `df(t)` can change every candidate containing `t`; the arrangement must emit retractions/reinsertions for affected query keys. Do not claim constant-time top-k for global statistics. Use a two-stage plan: incremental candidate membership, then bounded re-score of affected terms/documents. ANN scores have the same issue when normalization or model changes.

Graph reachability is an iterative arrangement. Maintain edge deltas and a frontier per iteration. For transitive closure, emit seed paths at iteration 0 and join newly reached paths with newly arrived edges; negative edges require differential retractions through all dependent paths or a recompute barrier. Strongly connected components and recursive Trustfall-like traversals therefore need a closed frontier and a bounded iteration budget. A lease can expose batches, but the arrangement owns truth and the lease owns backpressure.

Documents and source links remain lazy: membership/top-k emits `(object, image_digest, ordinal)`; a source cursor then opens the canonical image and borrows path/span bytes. Client serialization copies bytes. This keeps variable source payloads out of hot arrangements.

## SIMD and branch cost

Use SIMD only after the arrangement has reduced candidates. Store filterable fixed-width columns in 64-row or 256-row blocks. A query builds bit masks for relation, epoch, validity, and coarse numeric predicates, then intersects machine words before decoding cold payloads:

`candidate_mask = relation_mask & epoch_mask & live_mask & partition_mask`.

For 256 rows, four `u64` words require four ANDs per predicate block; a scalar row branch is replaced by bit iteration (`trailing_zeros` and clear-low-bit). Keep a scalar fallback for short runs and architectures without the selected instruction set. Branch-heavy paths to target are string prefix scanning, newest shadow checks, per-row authority checks, and Qdrant response matching; arrangement sorting and bitmaps move these checks to flush time or word operations.

Memory tradeoff: a live bitmap costs `n/8` bytes; one bitmap per predicate costs `p*n/8`. At `n=1,000,000`, one bitmap is 125 KiB; 16 predicates are 2 MiB before compression. Roaring-style containers help sparse masks but add branches; benchmark dense and sparse distributions. SIMD does not make variable-length UTF-8 comparison cheap, so retain a prefix trie/ART-like key index for exact/prefix navigation. ART's adaptive node widths reduce pointer work for in-memory string keys ([ART paper](https://db.in.tum.de/~leis/papers/ART.pdf)); use it as a hot cursor index, not a second authority.

## Compaction and merge frontier

Each run records key range, time range, relation set, root digest, and level. Choose size-tiered compaction for write-heavy microbatches and leveled compaction for read amplification. A merge is eligible when runs overlap in key range and their time frontiers are below the global safe frontier. Merge identical `(row_key, value_digest)` weights; retain the latest live value and deletion ledger entry when no retained reader can observe the older state.

The root manifest is an immutable map from relation/level to run roots. A new snapshot reuses unchanged roots. A remote pack ships only missing content-addressed nodes plus the manifest; receiver verifies node digest, parent root, schema, and signature before advertising the frontier. Pack shipping can therefore reuse existing index-pack durability, but arrangement roots and ANN state must be versioned separately.

## ANN incrementality barrier

Exact vector rows can be maintained incrementally. ANN graph topology cannot generally be treated as an ordinary relational view: inserting one point may require neighbor rewiring; deleting one point can leave stale edges and degrade recall. FreshDiskANN reports real-time updates with separate graph update/consolidation behavior ([paper](https://arxiv.org/abs/2105.09613)); Microsoft’s implementation documents lazy deletes followed by consolidation to restore graph quality ([dynamic index docs](https://github.com/DEVBOX10/microsoft-DiskANN/blob/main/workflows/dynamic_index.md)).

Therefore use three vector states:

* `VectorExact`: authoritative coordinates and live/deleted ledger, incrementally maintained.
* `AnnDelta`: recent inserts, replacements, and lazy deletes searched exactly or with a small fresh graph.
* `AnnBase`: immutable graph snapshot built at a sealed frontier, with recall metadata.

Search unions `AnnBase` and `AnnDelta`, filters deletion ledger, then reranks exact coordinates. Consolidation is an asynchronous barrier that builds a new `AnnBase` at frontier `f`; it is never allowed to advance the ANN advertised frontier beyond its actual input. Model/metric/dimension changes create a new namespace and cannot reuse old graph nodes.

## Local/remote execution

The planner selects a local arrangement cursor, remote arrangement shard, or both based on root availability, frontier, estimated cost, and staleness policy. A remote reply carries `(plan_digest, root_digest, frontier, relation, coverage, candidates)`. Merge rejects mismatched plan/root/frontier and treats missing shards as partial coverage. Local can answer from the last complete root while remote catches up; a result is marked complete only when all dependencies are beyond the query epoch.

Remote execution sends immutable run-root requests, not arbitrary row scans. Remote packs are content-addressed and resumable by missing node digest. The local cache key is `(root_digest, plan_digest, recipe_version)`. Network retries are idempotent because reads are root-addressed and writes use batch digest.

## Complexity, memory, and branch budget

For a microbatch of `b` rows and `L` overlapping runs, flush sorting costs `O(b log b)`. A point lookup costs `O(log fanout N + L)` before bitmap filtering; compaction of `m` rows costs `O(m)` merge work and writes `O(m)` bytes. A top-k update costs `O(log k)` when the boundary is unchanged, plus `O(a log k)` for `a` affected candidates; global-stat changes cost `O(A log k)` for affected query keys. Graph incremental work is proportional to delta joins plus retractions, and can approach full recomputation for non-monotone deletes. ANN update work is `O(log N)` exact insertion plus implementation-specific graph rewiring; consolidation is a batch barrier.

For `b=256`, one microbatch hot arena at 40 bytes/row is ~10 KiB plus payload refs. A 1M-row live bitmap is 125 KiB. A 16-wide top-k heap at 32-byte candidate records is 512 bytes per active query, excluding index nodes. Avoid per-row `String`, `Vec`, `Arc`, or trait objects in hot lanes; allocate cold payload blocks and network bodies separately.

## Convergence reference model

The test oracle is a simple replay interpreter:

```text
for batch in canonical_batches(order by time, partition, digest):
  verify_signature_and_parent(batch)
  relation[key] += batch.diff
for query at closed epoch e:
  scan all live relation rows with time <= e
  evaluate exact query semantics
  sort by deterministic recipe
```

The optimized engine converges when, after all input frontiers pass `e` and all operators drain, its materialized deltas reduce to the same multiset as the interpreter. Differential checks compare exact rows, lexical memberships/tombstones, graph paths, document source proofs, vector exact candidates, ANN recall against exact top-k, and complete/partial/degraded coverage. Replay permutations within the same declared partial order must produce equal roots and outputs. Crash tests cut after journal append, run seal, manifest publish, and remote pack receipt; reopening either sees the old root or a fully validated new root.

## Migration and benchmark program

1. Implement the journal/delta codec and replay oracle. Adapt existing `build`, `ingest`, and `publish` outputs into signed batches; retain current exact/lexical/graph/vector implementations as reference readers.
2. Implement primary and exact-key arrangements; prove update/delete ledger behavior against existing exact segments and catalog history.
3. Add term/document and forward/reverse link arrangements. Compare lexical ranking, prefix behavior, Trustfall traversal, and source lifetime against current tests.
4. Add microbatch/LSM/prolly persistence and root-addressed pack shipping. Measure write amplification, read amplification, compaction debt, bytes shipped, and recovery time.
5. Add incremental top-k with explicit global-stat epochs. Benchmark local update cost for stable stats versus `df` churn.
6. Add VectorExact/AnnDelta/AnnBase behind the barrier. Benchmark recall@k, update p99, delete debt, consolidation throughput, and stale frontier behavior against exact search.
7. Add local/remote planner and bounded parallel workers only after root/frontier correctness is stable.

Required matrix: rows 1K/1M/100M; delta batches 1/64/256/4096; update/delete ratios 99:1, 9:1, 1:1; run counts 1/4/16/64; prefix selectivity 0.01%/1%/50%; graph fanout 4/16/64; top-k 8/32/256; vector dimensions 128/768/1536; ANN delta age 0/1K/1M; local-only, remote-only, and mixed execution. Record p50/p95/p99 latency, CPU cycles, branch misses, allocations, hot/cold bytes, compaction write amplification, frontier lag, and ANN recall.

## What remains separate

Do not force all behavior into one generic operator. Source acquisition/signature verification, canonical semantic image decoding, ANN graph maintenance, global ranking statistics, and client serialization have different failure and consistency laws. The relation engine unifies identity, delta transport, version/frontier, persistence, and arrangement mechanics; each consumer retains a typed adapter and explicit quality class (`Exact`, `CompleteAt(frontier)`, `Approximate(recall)`).

The existing code is the oracle, not a constraint: its typed snapshots, tombstones, source proofs, Qdrant readback, graph lease, and pack durability define compatibility checks while the new engine chooses a more unified physical architecture.

## Sources and evidence limits

Primary sources consulted: differential computation (CIDR 2013), timely/differential documentation, DBSP (arXiv 2203.16684), O'Neil’s LSM-tree paper, ART (Leis/Kemper/Neumann), Microsoft FreshDiskANN, Microsoft DiskANN update documentation, and the prolly implementation README. Performance numbers in this document are formulas or design targets, not measurements. ANN recall, compaction cost, SIMD benefit, and remote crossover points require workload benchmarks.
