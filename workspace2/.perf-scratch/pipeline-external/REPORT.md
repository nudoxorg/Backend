# External-tier performance design audit

Date: 2026-08-29  
Scope: `workspace2/` only; read-only audit of source and plans.  
Status: no S3, Tantivy, SQL, database, HTTP, or async-runtime implementation exists in this
workspace. The recommendations below are therefore contract work, not claims of an unmeasured
speedup.

## Executive finding

The core has unusually good seams for an external data plane, but the seams are not yet specific
enough to prevent the first adapter from accidentally imposing one-object-at-a-time S3 traffic,
one-row-at-a-time indexing, or a database-shaped source of truth. The important next move is to
standardize *physical request geometry* and *ownership* before selecting an SDK.

The semantic core already says that roots/locality are immutable and placement is orthogonal
(`ARCHITECTURE.md:85-93`), that async I/O must yield owned bounded leases while validation and
hashing borrow synchronously (`ARCHITECTURE.md:89-93`, `ASYNC_STREAMING.md:20-31`), and that durable
truth is an immutable publication log plus a compact atomic head (`ARCHITECTURE.md:114-123`). Keep
those laws. Add an adapter-level contract with explicit ranges, packs, epochs, idempotency, and
segment manifests; otherwise a convenient S3 or Tantivy integration will smuggle policy into the
identity layer.

No `tantivy`, `s3`, `object_store`, `sqlx`, `rocksdb`, `redb`, or `postgres` dependency or source
integration was found. `ARCHITECTURE.md:248-257` explicitly rules out database engines as truth and
keeps transport technology behind an adapter. The current plan is consequently a good *direction*
but not yet an established external-system design.

## Current seams and what they imply

* `PreparedObjectPack::prepare` validates sorted descriptors and exact body lengths. It can emit
  either one caller-owned contiguous pack or a canonical index prefix followed by an
  allocation-free iterator over the original body slices
  (`crates/nudox-object-pack/src/write.rs`). This is the right upload seam: adapters can use
  vectored file I/O or bounded multipart coalescing without a complete staging copy. It still
  exposes no typed body-range lookup, selected verification, or range binding; those are correctly
  listed as missing in `ROADMAP.md:71-83`.
* `ObjectPackIndex` is a borrowed validated directory (`crates/nudox-object-pack/src/index.rs:13-39`)
  and is therefore a natural zero-copy directory view over an S3 range, mmap, or leased buffer.
  Do not deserialize it into `Vec<ObjectRef>` for a remote adapter.
* `HydrationPlanView::fetches` yields exact `ObjectRef` plus a closed `FetchRoute` and borrows
  caller scratch (`crates/nudox-hydration/src/plan/output.rs:17-45, 122-149`). This is a semantic
  demand stream, not yet a transport batch. Add a physical coalescing stage that consumes this
  iterator and emits bounded pack/range requests without changing `Fetch`.
* `Provider`/`BatchSource` is a concrete, GAT-based synchronous cursor with borrowed batches and
  no boxed stream (`crates/nudox-operation/src/contract.rs:10-61`). Use this for local compute and
  a corresponding async adapter returning owned leases; do not make `BatchSource` itself an S3
  client abstraction.
* `Store::insert_owned` transfers one verified owner with no payload copy and returns the original
  owner on rejection (`crates/nudox-store-memory/src/store.rs:122-137, 202-233`). A remote consumer
  should validate/hash a lease and transfer it directly; a temporary `Vec<u8>` per object would
  destroy the intended ownership win.
* `GenerationView` checks root/locality identity once, then its scans compose borrowed root and
  locality (`crates/nudox-root/src/locality/view.rs:327-369`). External indexes should consume this
  bound view or a sealed projection, never re-compare generation IDs for every hit.

## Required external architecture

### 1. Durable object plane: immutable packs, manifests, and a CAS head

Use three immutable content-addressed object classes:

1. `ObjectPack`: concatenated canonical bodies plus sorted fixed-width directory. Target a measured
   pack body of 8--64 MiB, with a hard maximum selected by the range verifier and memory budget.
   Start experiments at 8, 16, 32, and 64 MiB. Small packs amplify S3 request/TLS/index overhead;
   giant packs amplify tail latency, cache pollution, and retry waste.
2. `PackManifest`: one compact record per pack containing pack ID, byte length, object-count,
   min/max content key, directory checksum, body checksum/outboard ID, format version, and an
   optional bloom/range synopsis. It lets a query choose packs without downloading bodies.
3. `PublicationRecord`/`Head`: immutable append-only publication records and one conditional CAS
   head. A successful upload is not publication; acknowledge only after object durability and the
   typed head transition are both observed, matching `ARCHITECTURE.md:114-118`.

S3 layout should be deterministic (`packs/<content-id>`, `manifests/<generation-id>`,
`publog/<sequence>/<record-id>`, `heads/<logical-name>`), but keys are adapter details and must not
 enter `ObjectRef`/`GenerationId`. Store immutable data with write-once semantics; use conditional
 PUT/HEAD or equivalent for publication races. A retried upload is idempotent by content ID; a
 retried head update is idempotent by `(logical_head, expected_parent, publication_id)`.

### 2. Upload geometry and retries

The adapter should expose a typed operation approximately shaped as:

```text
PackUpload { pack_id, total_bytes, parts: bounded part plan, checksum, idempotency_key }
RangeRequest { object_id, offset, length, proof: optional range-proof id }
RangeBatch { object_id, sorted non-overlapping ranges, max_bytes, sequence }
```

Do not expose a raw S3 SDK request through core types. Multipart parts must be at least 5 MiB except
for the final part. Measure legal candidates such as 8, 16, 32, and 64 MiB and use multipart only
above a threshold where retrying one whole object is worse. For a retry:

* retry only transport/timeouts/5xx/429 under bounded exponential backoff with jitter;
* never retry integrity, schema, generation mismatch, or conditional-CAS failure as transient;
* checksum every complete pack and verify selected ranges with an outboard/tree proof before store
  admission;
* preserve the same idempotency key and part checksum across retries;
* bound simultaneous parts by both item and byte credits, with a separate remote-I/O budget from
  CPU validation budget.

The existing stream law requires exact terminal/cancellation and lease conservation
(`ASYNC_STREAMING.md:66-108`). Add fault tests for retry after response loss, cancellation during
multipart completion, duplicate part completion, stale head, and successful object upload followed
by failed publication CAS.

### 3. Range reads: avoid graph-edge serialization

The plan must not issue one remote request per `Fetch`. `ARCHITECTURE.md:119-123` already identifies
this failure mode. Coalesce canonical requests by pack ID, sort by offset, merge gaps below a tunable
threshold, and issue a bounded `RangeBatch`. Begin with `max_ranges=32`, `max_bytes=8 MiB`, and a
merge-gap of 64 KiB as benchmark defaults—not permanent protocol constants.

Use a two-stage fetch:

1. fetch/validate the small pack header + directory (or a manifest directory sidecar);
2. binary-search requested descriptors locally and fetch only merged body ranges.

Return a sequence-keyed owned lease for each merged range. Let consumers process ranges unordered;
only pay for a reorder ring when presentation requires canonical order. A range that spans several
objects should remain one physical lease while the borrowed pack view yields object slices. This
is where `ObjectPackIndex` and the planned zero-copy body view can remove both allocations and
copies.

For hot repeated reads, use a three-tier cache keyed by immutable `(pack_id, range)`:

* RAM: bounded admission by bytes, tiny hot directories and small bodies;
* local NVMe: whole packs or aligned extent chunks, disposable and checksummed;
* S3: authoritative immutable pack.

Do not cache arbitrary individual object copies by default: it loses pack locality and multiplies
metadata. Cache directory/manifest separately from body extents, and use a single-flight map for
the same missing extent so a thundering herd produces one request plus borrowed waiters.

### 4. Backpressure and parallelism

Use two queues only: a bounded demand queue and a bounded completion queue. Every item owns an item
lease and byte lease, as required by `ASYNC_STREAMING.md:34-45`; no detached task owns a lease.
Admission should account for `inflight_request_count`, `inflight_bytes`, `validation_bytes`, and
`indexing_bytes` independently. Otherwise a fast S3 source can fill RAM while the CPU/indexer is
still merging.

The safe high-throughput topology is:

```text
plan -> coalesce by pack -> bounded I/O lanes -> owned range leases
     -> borrowed validation/hash -> per-shard sealed delta -> one index writer
     -> immutable segment -> upload -> CAS publication
```

Partition by content-key hash for cache ownership and by pack ID for I/O coalescing. Use per-worker
local queues and batch stealing; reserve a global queue for overflow/coordination only. A lock-free
queue is justified only if a benchmark shows the bounded owner queue is the top wait source; it must
not replace the single-owner publication/index-writer rule. Cross-shard ordering is not needed for
immutable deltas; publication order is explicit in the head record.

## Tantivy/database integration (future adapter)

There is no Tantivy code today, which is good: no accidental storage contract has leaked. The
adapter must treat Tantivy as a disposable *projection* over immutable Nudox facts, consistent with
`ARCHITECTURE.md:248-253`, not as the identity or publication authority.

### Tantivy write path

Use one `IndexWriter` owner per local index directory/shard. Producers never share it and never call
`commit` per document. They emit compact borrowed/owned `IndexDelta` records into bounded per-shard
batches. The owner adds documents in batches, commits on either a byte/doc/time threshold, then
publishes a segment manifest only after the commit is durable. Merge policy is adapter-configured:
avoid foreground large merges on the query path; schedule merges under a separate byte/CPU budget,
and upload/reuse sealed segment artifacts rather than recompiling every replica.

Index fields should be chosen to avoid storing canonical object payloads twice. Store the minimum
search projection (typed key, generation, object ID, selected scalar fields) and retain the canonical
object/pack ID as the fetch pointer. If a field is only used for filtering, prefer a compact indexed
representation; if it is only returned, do not index it. Materialize strings only at the boundary
where Tantivy requires them; do not construct a parallel `GenerationEntry` forest for an entire root.

The ideal future API is a typed `IndexProjection<'view>` iterator over a sealed `GenerationView` or
pack view, with a `ShardKey` and explicit `ProjectionVersion`. That makes generation binding and
schema versioning compile-time-visible while keeping Tantivy types in an adapter crate.

### Tantivy read path

Query planning should first consult the immutable manifest/shard routing projection, then issue local
Tantivy queries only to candidate shards. Do not fan out to every shard or fetch S3 objects before
the index has narrowed candidates. Return stable object IDs and generation IDs first; hydrate bodies
in a second bounded batch using the same range coalescer as normal hydration. This prevents a search
hit from turning into N serialized object GETs.

Pin a projection to a generation/head snapshot. If a segment is stale, either answer from the pinned
snapshot or return a typed stale-projection result; never silently mix generations. On restart, replay
the publication log and verify segment-manifest checksums before admitting a projection.

### Database/catalog path

If a mutable catalog is eventually needed, use it only for compact mutable metadata (head pointers,
leases, cache inventory, segment state). Canonical packs, roots, and locality artifacts remain
streamable/range-verifiable bytes, as `PRODUCTION_RUST_NOTES.md:57-68` requires. A database adapter
must expose snapshot/transaction/conditional-CAS capabilities as a small typed trait; do not make
the core depend on a database page format or transaction lifetime.

## APIs to add before choosing an SDK

Prioritized, with the highest-leverage contracts first:

1. `nudox-object-pack`: typed `PackDirectory::locate(ObjectRef) -> BodyRange`, borrowed body view,
   exact selected verification, and a manifest/pack range plan. This is the direct prerequisite for
   avoiding per-object S3 GETs.
2. New nested remote adapter crate: owned `BufferLease`, bounded `RangeBatchSource`, retry/error
   taxonomy, cancellation, checksums, and physical counters. Keep all SDK/runtime types here.
3. `nudox-hydration`: a pure `RangeCoalescer` over `Fetch`/`ObjectRef` that emits caller-scratch
   batches; benchmark merge-gap, max-ranges, and max-bytes. It should be usable against an in-memory
   fake before S3 exists.
4. Publication adapter: immutable record upload plus typed conditional-head CAS and stable receipt.
   Add crash/retry/reopen scenarios before adding horizontal replicas.
5. Index adapter: `IndexProjection`/`IndexDelta`, shard routing, projection version, sealed segment
   manifest, and snapshot-pinned query result. Tantivy belongs behind this boundary.
6. Cache adapter: RAM/NVMe extent cache with single-flight and checksummed promotion/eviction; no
   cache policy in `nudox-object` or `nudox-root`.

Do not add `FetchRoute::S3` or `FetchRoute::Tantivy`; those are physical policies and would freeze
the semantic plan. Keep `Promised(ProviderSet)` and let a provider capability map it to one or more
physical range/object/index routes.

## Benchmarks and release gates

Every adapter benchmark must report p50/p95/p99 latency, throughput, allocated bytes/allocations,
in-flight peak bytes, request count, range coalescing ratio, retry count, checksum CPU, validation
CPU, cache hit rates by tier, and publication/index commit/merge time. Compare:

* one-object GET vs pack directory + merged range reads;
* 1/2/4/8/16 I/O lanes and 1/2/4/8 validation/index workers;
* 4/8/16/32/64 MiB packs and 8/16/32/64 MiB multipart parts;
* cold S3, warm NVMe, warm RAM, and mixed hot/cold traces;
* 1, 10, 100, and 1,000 requested objects with clustered and adversarially scattered offsets;
* Tantivy batch sizes and commit/merge thresholds under concurrent query load;
* stale head, duplicate publication, object loss, partial range corruption, retry storms, and
  cancellation at every lease phase.

The acceptance gate is not “10x” in the abstract. Require a measured end-to-end win against a
single-object baseline while preserving exact object IDs, root identity, generation pinning, byte
conservation, and terminal semantics. A 10x request-count reduction with unchanged p99 may be a
win; a SIMD or lock-free microbenchmark that increases S3/Tantivy tail latency is not.

## Recommended order of implementation

1. Finish the object-pack zero-copy directory/body/range API and its scratch verifier.
2. Implement the pure coalescer and fake leased range source; establish counters and fault tests.
3. Add the immutable publication log/head adapter and replay/crash harness.
4. Add a local NVMe extent cache and then one real object-store adapter behind the same source.
5. Add sealed-delta `IndexProjection` and a local Tantivy adapter with one writer per shard.
6. Only after the above has data, consider lock-free multi-producer submission, io_uring/Compio,
   NUMA pinning, or exotic unsafe storage. These can optimize a proven bottleneck; they cannot repair
   per-object request geometry or a projection that materializes the entire graph.

This order makes the external tiers part of the performance proof rather than an afterthought, while
preserving the current no-std, borrowed, content-addressed core.
