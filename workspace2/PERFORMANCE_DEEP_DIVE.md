# Workspace2 performance deep dive

Date: 2026-08-29  
Host for local measurements: Apple M3 Pro, aarch64 macOS  
Scope: memory layout, SIMD, construction, lock-free coordination, object-pack I/O, and future
S3/search-index tiers.

## Executive result

This pass found real order-of-magnitude kernels, but it did **not** establish a 10x end-to-end
system result. The adopted production changes are deliberately those whose invariants and
correctness gates are complete:

1. Root construction now uses a private two-byte phase niche inside the existing 64-byte row.
   The streaming builder sorts, resolves parents, detects cycles, computes depths, and publishes
   from one allocation with no hierarchy sidecars. At 100,000 rows its reported construction
   payload peak is 6.4 MB instead of the owned-`Vec` path's 13.6 MB (-52.9%).
2. The object-pack writer can emit its canonical index prefix separately and lend original verified
   body slices as an allocation-free exact-size iterator. File/S3 adapters no longer need a
   full-pack staging allocation or a complete payload copy.
3. The byte-credit counter now uses relaxed numerical atomics and a zero-credit fast path. Payload
   publication remains synchronized by the slot/ready state machines. The isolated counter is
   1.7-3.7x faster in the tested contention profiles; alternating public-runtime A/B runs won 20/24
   cells with a 1.31x geometric mean, but payload-heavy cells were noisy and this is not a universal
   latency claim.
4. Existing production SIMD locality validation remains important: the measured long-row case is
   10.18x faster than scalar, with exact first-error equivalence and a scalar short-lane crossover.

The strongest next representation result is a root structure-of-arrays prototype: at one million
rows, a contiguous key lane made key-only scans 10.5x faster and binary lookup 3.7x faster with the
same aggregate 64 bytes/row. It is not yet production because arbitrary-order construction either
adds a second peak allocation or pays an unacceptable in-place transposition cost.

## Production changes

### Root row: padding becomes proof state

`ObjectRef` is 48 bytes but contains two bytes of trailing alignment padding. `RootRow` now makes
those bytes explicit as a private closed state:

```text
CollectedParent | CollectedRoot -> Unseen -> Visiting -> Published
```

The final 64-bit word carries an exact parent key during collection, then a checked parent index and
depth after resolution. During hierarchy validation the same row state supplies the DFS marks. A
two-pass functional-graph walk measures each unresolved chain, then publishes depths from child to
the already-published ancestor/root. No visit vector, path vector, root-key vector, sentinel key, or
unsafe cast remains.

Important properties:

- every `u64` remains a valid `EntryKey`; no public key value was stolen as a sentinel;
- `RootRow` remains exactly 64 bytes, aligned to 8, with state/key/payload offsets asserted at
  46/48/56;
- the public root contains only `Published` rows;
- cycle detection uses a closed state, not a magic depth value, so all `u32` depths remain available;
- `GenerationRootBuilder::finish` allocates nothing;
- `GenerationRoot::new(Vec)` preserves the convenient arbitrary-input API and honestly accounts for
  its overlapping 72-byte input rows plus 64-byte output rows.

The production benchmark over 100,000 shuffled rows produced identical canonical IDs:

| path | construction peak | retained | median |
|---|---:|---:|---:|
| owned `Vec<RootEntry>` | 13.6 MB | 6.4 MB | 18.25 ms |
| streaming phase-niche builder | 6.4 MB | 6.4 MB | 19.04 ms |

This is a memory/allocator win, not a speed claim: pushing from an already-materialized fixture was
about 4% slower than the bulk-owned path in this run. A naturally streaming producer avoids creating
that fixture in the first place.

### Object packs: scatter/gather without changing canonical bytes

`PreparedObjectPack` now exposes:

- the exact public `index_bytes` prefix extent;
- `write_index`, with the same transactional short-output law as the full writer;
- `body_segments`, an allocation-free exact-size iterator lending the original input slices.

Concatenating the emitted prefix and segments is byte-for-byte identical to `write`. Pointer tests
prove that each body segment still borrows the original owner. This changes the adapter cost from:

```text
full pack allocation + copy every payload + I/O
```

to:

```text
small index allocation + bounded coalescing/part lease + direct borrowed payload I/O
```

The core does not choose `writev`, multipart upload, an async runtime, or an SDK. Those policies stay
in a nested adapter while the canonical grammar remains one source of truth.

### Runtime byte credits: synchronization belongs to the publisher

The byte-credit atomic protects only `0 <= available <= capacity`. It neither initializes payload
memory nor publishes a slot. The payload write and coordinate reuse already synchronize through the
ready/slot state machines, so acquire/release on the numerical counter added fences without carrying
memory.

The revised linearization points are:

- non-zero reservation: successful relaxed CAS;
- non-zero restoration: relaxed `fetch_add` in the atomic modification order;
- zero reservation/restoration: no atomic operation;
- payload publication: unchanged release operation on ready state;
- payload acquisition: unchanged acquire/CAS on ready state.

Loom verifies capacity conservation, over-reservation rejection, zero-credit identity, publication,
retirement, cancellation, and waiter generation reuse. Two alternating end-to-end runs per version
showed all eight zero-payload profiles improving by 1.28-1.88x. Across payload/policy/producer cells,
20/24 improved and the geometric mean was 1.31x; the remaining heavy cells were scheduler-noisy, so
the raw tables remain the authority.

## SIMD findings

### Adopted kernel

Strict sorted-locality row validation is the correct SIMD shape: contiguous big-endian `u32` rows,
one comparison grammar, a mask that identifies the first bad ordinal, and a short scalar tail. The
cached `fearless_simd` level avoids repeated feature detection.

Existing M3 Pro measurements:

| rows/profile | scalar | SIMD | speedup |
|---|---:|---:|---:|
| 32 rows, 250,000 parses | 24.82 ms | 8.39 ms | 2.96x |
| 16,384 rows, 488 parses | 18.34 ms | 1.80 ms | 10.18x |

Counts 0-16 remain scalar. The accelerated path added 328 bytes of text and 16 bytes of constants in
the recorded release fixture. Differential tests cover short lengths, alignments 0-15, malformed
first/middle/last lanes, scalar fallback, and detected execution.

### Layout can unlock auto-vectorization

The root SoA prototype demonstrates why SIMD cannot be treated separately from data layout. Summing
a contiguous key lane is vectorizable and reads 8 bytes/row; the AoS version strides through 64
bytes/row and drags cold descriptors into cache.

| rows | layout | binary lookup | key-only scan | full-entry scan |
|---:|---|---:|---:|---:|
| 100k | AoS | 232 us | 60 us | 62 us |
| 100k | SoA | 126 us | 9 us | 176 us |
| 1m | AoS | 2.94 ms | 1.06 ms | 1.16 ms |
| 1m | SoA | 0.80 ms | 0.10 ms | 0.98 ms |

The one-million-row key scan is 10.5x faster and lookup is 3.7x faster. The 100k full-entry
regression shows why a single mandatory representation is wrong. A production design should be a
closed construction policy selected from measured workload facts, or direct SoA construction from a
sorted/bulk boundary—not an extra key copy hidden inside every root.

### Grouped SIMD content index: useful but not universal

A SwissTable-like scratch index groups sixteen 7-bit fingerprints with sixteen compact entry
ordinals. NEON compares the 16 control bytes and constructs exact candidate/empty masks before full
content-ID verification.

- metadata at 100,000 entries: 1,048,576 -> 655,360 bytes (-37.5%);
- hit-only queries: 8.34 -> 5.66 ms for two million lookups (1.47x faster);
- misses: 12.85 -> 18.01 ms (1.40x slower);
- mixed 50/50: 13.15 -> 14.02 ms (6.7% slower).

This is not adopted as the general store index. It may be correct for a read-mostly, hit-heavy server
profile after real `ContentId` integration and x86 measurements. The four-entry lean store should
instead delete its index and scan four inline IDs; paying grouped metadata there would be backwards.

### SIMD candidates that remain unproven

- batch independent BLAKE3 object hashes through a domain-aware API; BLAKE3 already owns ISA-specific
  compression and must not be reimplemented;
- rank-prefix reads over fixed 256-row blocks (too short to assume dispatch amortizes);
- fused overlay classification/write passes and packed mark words;
- SIMD block-final matching only after a contiguous block directory localizes the candidate.

No unsafe or unstable SIMD code was added merely to satisfy a vectorization goal.

## Parallel and lock-free architecture

The runtime is lock-free at its core state transitions, but three global words cap producer scaling:
work permits, ready publication, and byte credits. CAS retry is lock-free, not wait-free; one unlucky
producer can starve.

The next candidate is a fixed lane fabric:

```text
producer -> preferred lane {free bitmap, ready bitmap, payload slots}
         -> bounded steal snapshot only when local lane is full
owner(s) -> drain owned lane -> terminal lane -> ordered merge only if requested
```

Common-path linearization stays local to one cache line. A shared byte counter remains the global
physical-budget authority. Cold retirement words, metrics, and terminal bookkeeping must be isolated
from producer-hot words to avoid false sharing.

This is not yet production because it needs a changed owner contract and Loom models for claim,
steal, publication, retirement, cancellation, and terminal conservation. The present public runtime
has one owner; adding producers cannot scale the serial execute/drain stage. The end-to-end harness
confirmed that throughput generally stops improving beyond two producers.

For waiters, existing model evidence strongly favors an active `u64` bitmap at low density: a
capacity-64 dense scan always visits 64 records and about 40 logical cache lines, whereas the active
bitmap visits exactly 0/1/8/64 active records and retains 2,560 model bytes. It still needs a hardware
throughput benchmark and the existing register-arm-recheck stale-wake proof.

An append-only lock-free object index was rejected as a default: epoch-free reclamation is attractive
when nodes never delete, but one heap node per object worsens allocations and locality versus the
current single-owner packed store. Immutable sealed segments are the more promising parallel unit.

## S3, Tantivy, and database tiers

There is currently no S3, Tantivy, SQL, or remote adapter in the workspace. The performance contract
must be fixed before an SDK chooses it accidentally.

### One immutable physical grammar, several owners

The target data path is:

```text
root demand
  -> group by pack and coalesce sorted body ranges
  -> bounded I/O lanes return owned byte leases out of order
  -> borrowed validation/hash over those same bytes
  -> local immutable store and sealed index deltas
  -> Tantivy projection segments / NVMe extent cache
  -> immutable upload, then typed CAS publication head
```

The canonical object pack is usable as a caller buffer, mmap, leased range, local file, or S3 object.
Adapters must not deserialize it into parallel object forests. The newly added scatter/gather writer
is the upload-side prerequisite; the missing read-side prerequisite is a typed directory lookup that
returns exact body ranges without reparsing or allocating.

### S3 geometry

Use a two-stage read: fetch/validate the directory first, then coalesce demanded body ranges by pack.
Start range experiments around 8 and 16 MiB and use concurrent connections for independent ranges;
AWS explicitly recommends byte-range concurrency and alignment with multipart boundaries. Multipart
parts must be 5 MiB-5 GiB (except the final part), with at most 10,000 parts; AWS suggests considering
multipart upload around 100 MB. These are adapter constraints, not canonical format constants.

The essential win is request deletion: 100 clustered missing objects should become one directory hit
plus a few merged range requests, not 100 serialized GETs. A range lease may contain several objects
and should remain one owner while borrowed views expose its object slices. RAM caches hot directories
and small extents; NVMe caches checksummed whole packs/aligned extents; S3 is immutable durable truth.
Single-flight by `(pack_id, extent)` prevents a thundering herd from multiplying GETs.

Official references: [AWS S3 performance patterns](https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance-design-patterns.html),
[multipart limits](https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html), and
[multipart integrity/retry model](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html).

### Tantivy projection

Tantivy is a disposable derived view, never publication truth. Use one `IndexWriter` owner per local
shard/directory. Producers submit sealed `IndexDelta` batches under byte/doc credits; the owner uses
the batch API so operations receive contiguous opstamps and same-batch adds flush together. Commit by
sealed-delta/byte/time threshold, never per document. A commit blocks and publishes pending changes;
merge work therefore has its own CPU/I/O budget and cannot run uncontrolled on the query path.

Tantivy segments are immutable, which matches content-addressed segment manifests. Build/merge once,
checksum and publish the resulting segment artifact, then let replicas download it instead of
repeating compaction. Search returns stable object/generation IDs first; body hydration happens as a
second coalesced range batch. A query pins one projection snapshot and never mixes generations.

Current Tantivy documentation supports this shape: `IndexWriter` owns indexing threads and a shared
queue, `run` accepts an `ExactSizeIterator` batch, commits publish/persist pending work, and merge
policy is evaluated when segment lists change. See [IndexWriter](https://docs.rs/tantivy/latest/tantivy/indexer/struct.IndexWriter.html)
and [Tantivy architecture](https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md).

### Mutable database scope

A database may own compact mutable catalog facts—head pointers, leases, cache inventory, segment
state—but never canonical objects, roots, or identities. Its adapter exposes snapshot and conditional
CAS capabilities with typed receipts. Replacing PostgreSQL, SQLite, or another catalog must not
change the object/pack/root grammar.

### External-tier gates

Measure p50/p95/p99, throughput, request count, coalescing ratio, retries, in-flight bytes, checksum
CPU, validation CPU, cache hit rate by tier, index commit/merge time, and publication latency for:

- single-object GET versus directory + coalesced ranges;
- 1/2/4/8/16 I/O lanes and 1/2/4/8 validation/index workers;
- 8/16/32/64 MiB logical packs plus >=100 MiB multipart profiles;
- 1/10/100/1,000 clustered and scattered demanded objects;
- cold S3, warm NVMe, warm RAM, and mixed traces;
- stale heads, response-loss retries, partial corruption, cancellation, and merge storms.

## Rejected or deferred designs

- Caching the 32-byte overlay basis in each consumer did not show a stable win and could not remove
  the owned result copy.
- In-place AoS->SoA word permutation preserved 64 bytes/row but cost 122 ms at one million rows;
  constructing the wrong layout and paying to transpose it is rejected.
- AoSoA-8 added about 1.56% memory and lost current lookup/key-scan benchmarks without a separate
  directory.
- The grouped SIMD store index is hit-heavy only, not a universal replacement.
- SIMD binary search over sparse exception rows has insufficient gather locality.
- `portable_simd`, nightly intrinsics, and custom unsafe arenas were not adopted without a measured
  end-to-end advantage and a complete provenance/drop/panic proof.
- A multi-owner runtime is not a local patch; it changes terminal semantics and requires a new
  ownership model.

## Verification completed

Reproducible evidence is retained in the scratch corpus: [production root benchmark](.perf-scratch/root-production/raw.tsv),
[SoA/AoSoA measurements](.perf-scratch/memory-layout/soa_raw.tsv),
[in-place transposition rejection](.perf-scratch/memory-layout/inplace_raw.tsv),
[SIMD audit](.perf-scratch/simd-loops/AUDIT.md),
[grouped SIMD index measurements](.perf-scratch/swiss-index/raw.tsv),
[atomic-ordering measurements](.perf-scratch/atomic-ordering/raw.tsv),
[alternating end-to-end A/B summary](.perf-scratch/end-to-end/ab-summary.tsv), and the
[external-tier audit](.perf-scratch/pipeline-external/REPORT.md). Generated build directories and
benchmark executables are intentionally excluded.

- `cargo test --workspace --all-targets`: 165 tests passed.
- `cargo test -p nudox-runtime --features loom-model --lib`: 9 Loom/model/layout tests passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- standalone `layout-lab` all-target check: passed after removing one stale deleted type import.
- root production tests: canonical identity, duplicates, missing parents, cycles, depths, layout,
  million-row diff, and permutation properties passed.
- object-pack tests: contiguous/scatter reconstruction, exact source pointers, short-output rollback,
  allocation laws, and directory mutation/truncation passed.

## Ranked next implementation sequence

1. Add allocation-free object-pack directory lookup/body-range views and a pure caller-scratch range
   coalescer. This prevents per-object S3 requests before any SDK exists.
2. Build a fake leased range source with deterministic fault/cancellation schedules, then one real
   object-store adapter using the same contract.
3. Prototype direct sorted SoA root construction and projection-specific scans; require construction,
   full-entry, memory, and canonical-byte evidence before selecting a production policy.
4. Implement lane-local runtime bitmaps behind a separate experimental owner contract and Loom model.
5. Add sealed `IndexDelta`/`IndexProjection` types, then a nested Tantivy adapter with one writer per
   shard and explicit commit/merge budgets.
6. Publish immutable segment/pack manifests through a typed conditional head and add RAM/NVMe extent
   caching with single-flight.

The system-level 10x target should be attached to a named trace. The credible path is multiplicative:
10x fewer remote requests from frontier/range batching, 10x key-only memory scans from SoA, 10x long
row validation from SIMD, and no full-pack staging copy. Those gains do not multiply automatically;
the end-to-end gate must prove which remain after S3, indexing, merging, and hydration are combined.
