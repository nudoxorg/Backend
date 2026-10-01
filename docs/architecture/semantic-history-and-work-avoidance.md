# Semantic history and work avoidance: V2 cutover contract

This is a design and implementation gate, not a declaration that V2 is selected.
The existing compiler, canonical IR, NXFI image, semantic snapshot diff, typed
input tree, and segment hydration remain the foundations. V2 changes the
physical layout and durable lineage without pretending the current snapshot
diff is a persistent repository or the current ordinal chunker has edit locality.

## Three distinct identities

- A **content root** commits a canonical image profile and the exact, ordered
  roots and row counts of all seven normalized row families: Core, Types,
  Relations, Occurrences, Documentation, SourceProvenance, and
  LanguageExtensions. Empty families have explicit descriptors. It excludes
  compiler provenance and input reads, so identical semantic output can reuse
  the same content across different builds.
- A **generation root** commits the content root, canonical build identity,
  and an owner-verified read-frontier closure. A workspace snapshot ID is not
  a read frontier. Evidence of *how* a complete frontier was observed is
  separately audited; it must not perturb the generation identity.
- A **history commit ID** commits the generation root plus ordered parent
  commits and commit provenance. Branch and tag names are mutable refs updated
  by compare-and-swap under an interprocess lock; the underlying commit and
  payload closure are immutable.

This is a new wire/profile domain: `NormalizedReachable`. It is not NXFI-byte
equivalence. Types include exactly one root row per canonical declaration
(including an absent type), reachable typed/list/atom closure, canonical
externals, and type references seeded by all language extensions. Legacy
unreachable pool slots are not semantic facts in this profile. The aggregate
verifier must check exact family census and every cross-family reference before
it can mint an opaque publication token. A family decoder's local syntax check
is necessary, but never sufficient to select V2.

## Producer-owned change frontier

A stable-key row stream removes ordinal byte-shift churn, but a full scan of the
reader remains O(image size). The compiler must additionally provide a
**complete changed-key frontier** under the exact previous and next generation
authority: Insert, Replace, Delete, and Unchanged-range proofs. The producer
may only take the fast path if the compiler can prove completeness; an
unproven frontier falls back to a full canonical scan. The complete frontier
must include declaration shape, type dependencies, graph/occurrence facts,
source provenance, docs, extensions, and embedding inputs. The row index is a
persistent ordered tree keyed by (family, stable row key), with content-addressed
row payloads, subtree counts, and subtree commitments. Updating k keys costs
O(k log n) tree work and hashes only changed rows plus affected tree paths;
snapshot publication stores one new root and reused nodes, not a copied map.
A no-op edit should hash zero semantic payload bytes after an admitted input
frontier comparison. This is a target requiring production counters, not a
claim about today's producer.

The index supports borrowed immutable segment reads, small mutable overlay
batches, and explicit flush/compaction. Avoid an Arc per row: own immutable
slabs or mmap-backed blocks per generation and lend row slices to scoped
workers. A lane owns its mutable scratch and local allocator; workers emit
sorted run files/segments and return immutable handles to one sequencer that
publishes roots. This gives parallel encoding without a global row mutex.
Contention is acceptable at coarse publication/ref CAS, never per row.
SIMD belongs behind measured byte-search/compare/hash kernels with scalar
equivalence tests, tail/unaligned tests, and a dispatch threshold; it is not a
reason to weaken canonical byte order or identity rules.

Rows under the segment byte ceiling are grouped by stable key prefixes and
size bounds, so inserts cannot shift every later segment. A single oversized
documentation or source value needs a typed descriptor containing owner,
field/ordinal, total length, leaf count, and an ordered rope root. Its
content-defined leaves (bounded min/target/max) and interior hash nodes permit
partial fetch and independent verification. The descriptor is one semantic
row; the leaves are transport/storage objects. Text boundaries and UTF-8
validity are checked after ordered reassembly. A leaf hash alone cannot prove
its position, so a transfer must include its authenticated rope path and a
complete closure check before publication.

## Durable history and storage

The retained history is an append-only commit DAG plus ref CAS, not a second
IR representation. Each published commit names an immutable closure object
covering all segment/rope/object IDs needed to replay that commit after a
restart or garbage collection. Ref publication follows verified closure
persistence and fsync; readers observe either the old ref or a fully durable
new one. A crash between these steps is recovered by idempotent intent
reconciliation. Mark/sweep roots include every retained ref and in-flight
pinned reader; losing a mutable current-image file must not make the third-old
commit unreadable. Reopen, third-old replay, cross-process CAS, partial send,
and forced GC are release-gate tests. A history record without payload
retention is only a provenance log and must not be presented as replayable.

The same content-addressed closure can live in local disk, a remote index
disk, or S3. Location and replication policy are separate from identity.
Streaming transfer is by authenticated missing-object set plus resumable
range grants. ACK means durable admission of the exact closure, not receipt of
a packet; a partial send cannot advance a ref. Index machines can serve IR
directly to clients, while compiler machines can stream to the index or a
storage backend. Read routing uses local verified objects first, then remote
peers, without weakening the selected-generation proof. The local client
retains bounded hot, warm, and cold residency over the same immutable IDs.

## Admission and cutover gate

The compiler read frontier must be *complete* for its process tree: positive
file reads, missing path probes, directory listings, environment, toolchain,
generated files, and classified external mounts. A workspace snapshot or a
source-file crawl is not enough to claim this. Until a trusted broker or
complete frontend/process observer exists, V2 input admission remains
`Unproven` and production selection stays on V1. A lost event, unsupported
path, unobserved child, or unverifiable non-workspace read fails closed.
Compiler/session cache retention may use a weaker heuristic only if it cannot
authorize persistent IR reuse or silently stale results.

Cutover runs both layouts from the same established compiler output. The
oracle re-encodes every family from the canonical reader; the candidate
must match exact row counts, order, payloads, roots, and cross references.
Measure cold-open, no-op, early/middle/tail edit, insert/delete/rename,
documentation jumbo edit, dependency change, graph occurrence change,
multi-generation replay, forced GC, and remote interrupted transfer.
Candidate selection requires equal semantic query results and user-visible
source navigation across CLI, MCP, and GUI. Benchmark total work: compiler
read/validation, lowering, row discovery, row encoding, hashing, allocation,
resident bytes, transfer bytes, hydration reads, and end-to-end latency.

The synthetic 8,192-entity experiment in
`crates/semantic/benches/ir_delta_locality.py` shows why both halves are
needed: prefix-key segments fetch roughly 23–53 KiB for ordinary edits in
that fixture, versus 0.6–6.9 MiB for ordinal chunks, but all full-scan
planners still read/hash about 13.78 MiB per compared pair. A modeled
compiler-supplied changed-key frontier reduces target row hashing to 0
bytes on no-op and about 0.8–4.9 KiB for one-row edits, excluding its
unimplemented discovery oracle. These are synthetic layout experiments,
not production throughput claims.
