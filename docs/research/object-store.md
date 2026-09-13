> **Research, not the final specification:** use the [final v2 index](../architecture/README.md). These reports preserve alternatives; final v2 equations, package boundaries and corrections take precedence.

# Versioned object substrate: concrete end state

This is the second redesign direction for the heart and semantic storage layers. It deliberately chooses one substrate: a persistent, content addressed ordered tree with deterministic content defined chunking, chunked columnar leaves, and immutable base plus bounded delta overlays. Every semantic object, root, index, and transport artifact is an object in one object table. Scheduling is local first when local execution is cheap and capable; a cached remote result may win when it is already available, and a local fallback reservation may be held while remote work runs.

The objective is a million entity semantic image where a one entity edit usually rewrites a small set of leaf and ancestor objects, while logical identity remains stable across packing and compaction. The end state is not a menu of tree options: use a prolly style ordered Merkle tree for all ordered maps, and use the same object substrate for its nodes, column chunks, semantic graph chunks, root manifests, deltas, and proofs.

## Why this shape fits the current code

`GenerationRoot` currently owns one boxed array of fixed 64 byte rows and hashes a canonical 63 byte wire row stream (`heart/root/packed.rs:213-302`, `heart/root/encode.rs:55-101`). `GenerationRootBuilder::finish` sorts all rows, resolves parents by binary search, validates cycles/depth in phase reused row state, and freezes the arena (`heart/root/builder.rs:77-118`). `RootDiff` already provides an O(old + new), O(1)-extra-memory linear merge and classifies additions, removals, content changes, and reparentings (`heart/root/diff.rs:43-57, 84-157, 159-181`). These are the right semantics, but a million-row root edit still rebuilds and rehashes the whole flat root.

The compiler already carries typed content and artifact identities. `GenerationId` is a domain separated `ContentId<RootDomain>` alias (`heart/identity/generation.rs:5-14`); semantic fragments carry `SourceIdentity` with a typed source content ID and length (`compiler/ir/model.rs:9-16`); recipes bind source, toolchain, language profile, and stage into a typed identity (`compiler/vocabulary/lib.rs:2152-2180`); artifact IDs bind encoding and domain and can hash chunks incrementally (`heart/identity/artifact.rs:163-208`). The semantic reader deliberately separates a portable core capability from a complete image capability (`compiler/ir/reader.rs:77-108`). The substrate should preserve these authorities and make their values the leaves of one versioned map rather than inventing another identity kernel.

## Chosen physical and logical model

The durable unit is:

```text
ObjectId  = domain || logical_kind || content_digest
StateRoot = canonical visible key -> complete version-reference map
CommitId  = parents || StateRoot || provenance/recipe authorities
DeltaId   = base StateRoot || target StateRoot || canonical patch
LayoutId  = base+delta arrangement || physical codec/pack metadata
ObjectRecord = ObjectId, logical_len, schema, kind, physical_extent
```

`ObjectId` remains a stable content identity. `StateRoot` is the history independent identity of the visible map: equal visible key-to-complete-reference maps have equal StateRoots regardless of how they were reached. `CommitId` records parents and provenance, so two commits can have the same StateRoot while retaining different history. `DeltaId` names one canonical patch between two StateRoots. `LayoutId` names a chosen base-plus-delta/materialization and physical arrangement. A `PackId` or extent ID identifies one physical placement and is never a semantic reference. Repacking, compression, encryption, page migration, and slab relocation change LayoutId/physical IDs only; they cannot change StateRoot, DeltaId, or CommitId. This is the required separation between visible state, history, patch, and layout.

Every ordered collection is a persistent prolly tree: immutable internal nodes point to child object IDs; leaves contain sorted key/value records. The canonical StateRoot tree maps each logical key to a complete version reference, so a visible-state rewrite is path copied even when the physical implementation also retains base and delta layers. Boundaries use one persisted deterministic key-only cut policy at every parent level, with minimum, target, and hard maximum encoded-byte bounds. Key-only boundaries are the default for semantic maps so value edits do not intentionally seed neighboring cuts; value participation is reserved for byte streams where deduplication requires it. Content-defined boundaries usually resynchronize after edits, but ripple is not guaranteed bounded: an adversarial or unlucky sequence can require O(N) leaf/parent rewriting. Dolt’s primary explanation covers history-independent boundaries ([Dolt prolly chunker](https://www.dolthub.com/blog/2022-06-27-prolly-chunker/)); the maintained Rust implementation documents deterministic structure, sharing, range diff, and persisted chunking configuration ([prolly-map design](https://github.com/crabbuild/prolly)).

Leaves use a chunked columnar layout rather than an array of opaque row structs:

```text
LeafHeader { schema, row_count, key_codec, column_count }
Keys       : prefix/delta encoded sorted logical keys
Parent     : validity bitmap + delta encoded parent key/ID
Descriptor : content ID, length, schema, kind columns
Residence  : compact locality/delta tags
PayloadRef : logical object IDs, never native pointers
```

Fixed width columns permit projection and SIMD friendly scans; variable atoms, documents, and large blobs remain separate immutable objects referenced by ID. A leaf’s canonical bytes include schema and column codec authorities, so physical compression can vary behind the same logical object only when the compressed bytes are represented as a separate artifact. For hot local execution, decode leaf columns into a reusable slab-backed `LeafBatch`; for cold access, retain validated encoded leaf bytes and project columns directly.

## Base plus delta versions

Each physical layout may point to a base tree root plus an ordered delta manifest, but the canonical logical StateRoot is always materialized as a prolly key→complete-version-reference tree. Folding overlays may produce a new LayoutId and a faster arrangement; it must not change StateRoot. `DeltaId` is a canonical patch between StateRoots, represented as a prolly map keyed by stable logical key and carrying `Put`, `Delete`, `Reparent`, or `PatchColumn` operations. A read may resolve a LayoutId’s newest delta to oldest, then base, while checking that the resulting visible references equal the requested StateRoot. Deltas are immutable and content addressed, so branches share them. A write sorts/coalesces mutations by key, creates new delta leaves, path-copies the logical StateRoot tree, and publishes a CommitId atomically.

The read path has three states: local LayoutId hit, local miss with a complete proof of the missing subtree, and remote delegation. A delegation request carries StateRoot/CommitId, tree path/range, required schema/recipe authorities, and child IDs already present locally. The scheduler may race a cached remote result against a locally reserved fallback when latency or locality policy says that is cheaper. The remote returns missing immutable objects plus a signed or content-authenticated result; local publication verifies IDs before inserting them. Remote work never receives mutable local references or an unpinned request.

For a tiny edit among 1,000,000 entities, the normal work is: sort the small mutation batch, rewrite affected key-only leaves and O(log N) internal nodes in the StateRoot, create a DeltaId, and update one CommitId. Because content-defined boundaries can ripple, worst-case rewrite is O(N), so the implementation needs a work budget and can retain a delta-only LayoutId when the canonical path-copy would exceed it. Reads touching unchanged ranges use old leaf IDs directly. A background materializer may fold deltas into a new physical layout only when thresholds are exceeded; it must prove that the folded visible map hashes to the exact existing StateRoot. Readers can continue using the old LayoutId plus deltas until a new LayoutId is published.

## SCC and cyclic semantic graphs

The ordered entity map handles names, declaration identities, containment, and versioned facts. Cyclic type/link graphs must not use recursive content IDs that require an impossible “hash while referencing self” construction. Assign stable logical node keys from the existing declaration identity authority (source identity, recipe, module path, declaration key). Store graph edges as symbolic logical keys in an edge map. SCC IDs are derived only after resolving the finite member/edge dependency set: canonicalize each SCC by sorted member keys and sorted labeled symbolic edges, then hash the completed descriptor. The graph StateRoot references SCC IDs and the ordered entity StateRoot.

An SCC ID is therefore a content ID of a canonical finite component descriptor, while entity logical IDs remain stable across versions. A changed edge can rewrite its SCC and condensation DAG ancestors, but dynamic deletion or an edge that joins components may require recomputing a whole affected region, potentially much larger than the directly edited subgraph. During construction, a temporary logical-key graph is allowed in a bounded mutation arena, but no temporary pointer or native address may escape into canonical bytes. Existing root cycle validation (`heart/root/builder.rs:217-277`) remains the containment check for parent hierarchy; SCC canonicalization is the separate mechanism for general semantic cycles.

## Memory layout, reuse, and reclamation

Use one `ObjectArena` abstraction with three explicit classes:

1. `HotSlab`: mutable scratch and decoded leaf batches, owned by a request lease and reset only after all borrowed cursors drop.
2. `PinnedBytes`: immutable mmap or slab extents holding encoded object bytes, reference counted or epoch pinned at the physical extent level.
3. `ColdObject`: durable object records addressed by IDs, fetched through a bounded cache with both object count and byte limits.

The current memory store already separates payload bytes from fixed index metadata and returns borrowed views (`heart/memory/store.rs:142-151, 264-283`; `heart/memory/capacity.rs:191-205`). Extend accounting into `logical_payload_bytes`, `encoded_bytes`, `slab_reserved_bytes`, `decoded_hot_bytes`, `index_bytes`, and `scratch_peak_bytes`. A 4 KiB object in a 2 MiB mmap must charge the pinned extent according to policy; otherwise zero copy hides retention. A slab handle must keep the entire slab live until the last object/view lease ends. Scratch bytes are not cache bytes and must be returned on cancellation.

Reclamation is independent of logical identity: mark reachable object IDs from retained CommitIds/StateRoots, active leases, remote transfer pins, and LayoutIds still serving readers; sweep physical extents not containing reachable IDs; rewrite survivors into new packs; atomically replace physical location records and publish a new LayoutId. No StateRoot, DeltaId, or CommitId changes during compaction. A physical index may be rebuilt from object records, while Merkle proofs continue to use logical child IDs.

## Unified mutation and execution paths

All mutations enter as `MutationBatch { base_version, operations, recipe_authorities }`. The batch validates stable key order, coalesces duplicate operations with deterministic last-writer rules, resolves local base/delta state, emits changed leaf objects, and publishes a new version manifest. Root changes, semantic IR changes, locality transitions, and pack additions are all the same operation shape; only schemas and merge policies differ.

All reads enter as `ReadPlan { version, key/range, projection, authority requirements }`. The planner first checks hot slabs and local object cache, then walks local tree nodes. If a node is missing, it emits a remote delegation containing the missing child proof and exact range. Hydration becomes execution of a `ReadPlan` over object IDs; current `Need`, `BoundNeed`, `HydrationPlanView`, and `VerifiedGeneration` become typed compatibility views over this plan. Presence checks no longer need to rescan a flat root and separate store: the tree walk yields object IDs and the object cache resolves them.

Current `RootDiff` becomes a tree diff: equal node IDs prune entire subtrees, unequal leaves merge sorted keys, and deltas are emitted without materializing unchanged rows. Its existing change taxonomy remains exactly the public semantic result. The current linear diff is retained as the leaf and compatibility implementation while the tree root uses subtree ID equality.

## Complexity and tradeoffs

Lookup and point mutation are expected O(log_B N) node visits; range iteration is O(log_B N + output), and tree diff is proportional to unequal subtrees plus boundary paths. A normal tiny edit avoids O(N) root hashing, but content-defined ripple has an O(N) worst case and must be budgeted. Base materialization costs O(N) in the affected tree and is amortized by a threshold such as delta bytes > 5% of base bytes or delta count > 32; these are starting policy constants to benchmark, not correctness assumptions.

The trade is more metadata objects and a more complex read merge than the current flat root. Content-defined chunking can cause unbounded boundary ripple in the worst case, requires persisted chunk policy in the tree schema, and costs rolling-hash CPU. FastCDC’s primary USENIX paper reports that normalization and skipped sub-minimum cut points address CDC CPU overhead while retaining similar deduplication ([FastCDC paper](https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf)). The implementation should use a key-aware rolling boundary detector for ordered leaves; FastCDC is evidence for the CPU techniques, not a drop-in algorithm for semantic key/value boundaries.

A Merkle radix trie gives excellent prefix proofs and predictable byte-prefix locality, while a classic fixed fanout persistent B-tree gives good ranges but fixed split points can cause insertion cascades and history-dependent shapes. Radix range scans are possible with ordered byte encodings and subtree interval metadata, so radix is a viable secondary index or specialized prefix family. The chosen prolly tree is primary because it combines ordered scans, deterministic history-independent cuts, and subtree IDs for authentication and pruning. The maintained prolly design explicitly records persisted chunking policy because changing it changes tree identity ([prolly configuration notes](https://github.com/crabbuild/prolly)).

## Migration sequence

1. Add the object table and stable `ObjectId`/`StateRoot`/`CommitId`/`DeltaId`/`LayoutId` distinction beside current `ContentId`, `GenerationId`, and `ArtifactId`. Store canonical object descriptor and physical location separately.
2. Implement canonical leaf/internal node schemas and a local memory object cache with byte/count budgets. Add a loader that can read current flat root and semantic image artifacts as synthetic one-version trees.
3. Implement deterministic key-aware prolly construction, subtree-ID diff, and local mutation batches. Verify that a flat-root import and a tree export preserve `RootChange` classification.
4. Move semantic core entity rows, declaration identities, atoms, types, links, occurrences, and language extension lanes into separate typed column families under one semantic image manifest. Keep large atoms/docs/blob payloads as object references.
5. Add SCC graph objects and condensation-root authority. Reject recursive self-addressing encodings; publish only fully canonicalized SCC descriptors.
6. Replace hydration’s flat closure scan with `ReadPlan` execution and remote delegation proofs. Keep existing capability types as adapters until all callers use version manifests.
7. Add base materialization, reachability GC, and pack compaction. Validate that moving every physical object leaves all logical IDs and version roots byte identical.

## Validation and benchmark gates

Before claiming the redesign, measure 1e6 entities with 1, 10, 1,000, and 100,000 edits; random and clustered keys; 0%, 50%, and 100% local cache hit rates; and cold mmap versus hot decoded slabs. Record bytes written, changed object count, hash CPU, node visits, range throughput, peak pinned bytes, decoded slab bytes, scratch peak, cache eviction, remote bytes, and p50/p95 latency. Compare flat root rebuild, persistent fixed B-tree, radix trie, and the chosen prolly implementation under identical canonical schemas. Test history-independent convergence by applying the same mutation set in multiple orders and compare version roots. Test compaction by comparing logical roots before/after physical rewrite.

Correctness gates must cover duplicate keys, deletes followed by puts, overlapping deltas, stale base versions, concurrent branches, merge conflict determinism, missing remote subtrees, cancellation during fetch and hashing, SCC self loops and mutual cycles, schema/recipe authority mismatch, proof rejection, and physical extent reclamation while readers hold leases. A cancelled or partially delegated operation must never yield a verified version capability.

## Sources and limits

Primary frontier sources used here are the FastCDC USENIX paper ([Xia et al., USENIX ATC 2016](https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia)) and the maintained prolly implementation/design ([crabbuild/prolly](https://github.com/crabbuild/prolly)), with the history-independent boundary rationale documented by Dolt ([How to Chunk Your Database into a Merkle Tree](https://www.dolthub.com/blog/2022-06-27-prolly-chunker/)). These sources support properties of CDC/prolly structures; they do not establish performance for this repository. All complexity and threshold statements above are design targets requiring the benchmark gates.

Reviewed heart files: `heart/identity/{content,artifact,generation}.rs`; `heart/object/{descriptor,canonical,provider,residence}.rs`; `heart/memory/{store,index,capacity,backing,view}.rs`; `heart/root/{builder,packed,encode,diff,closure,locality,overlay,root_view}.rs`; `heart/hydration/{need,plan,publication}.rs`; `heart/frame/encode.rs`; `heart/object-pack/{index,write,view}.rs`; `heart/view/validate.rs`; `heart/schema/{ids,limits,vocabulary}.rs`; `heart/adaptive/lib.rs`; `heart/observe/flight.rs`; `heart/telemetry/{batch,probe,dispatch,metrics}.rs`. Semantic files: `compiler/ir/{model,reader,semantic_discovery}.rs`, `compiler/vocabulary/lib.rs`, `compiler/application/compiler.rs`, and `compiler/publication/generation.rs`. Vendor `server/index/turso` was skipped as requested.
