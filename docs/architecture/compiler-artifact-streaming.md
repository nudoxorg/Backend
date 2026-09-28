# Compiler nodes and portable IR generations — proposed target

Status: cutover contract, updated 2026-09-28. The Iroh/Bao CAS transport has passed two-process encrypted transfer, interruption, restart, and tamper tests. The end-to-end compiler assignment and index-head cutover remain under implementation; design requirements below are not claims of shipped behavior. This refines [versioned-engine](../architecture/versioned-engine.md), [layout](../architecture/layout.md), and [local-remote](../architecture/local-remote.md) around the compiler's dual deployment and artifact transport.

## The ownership rule

The **compiler node** is the same kernel when embedded in the GUI and when installed as a service. It owns retained, incrementally advanceable language sessions and produces immutable, version-bound IR facts. Nodes may keep caches and native processes, but have no authority to select an index root. The **index owner** owns source and semantic publication, package metadata, durable catalog, and derived search projections. It can run locally for the GUI or remotely at larger scale. Local-first means a disconnected GUI has a full local index owner, not an RPC-dependent facade. The remote service increases capacity without forcing an interactive edit through a network round trip.

```text
GUI / CLI / MCP ── local index owner ── embedded compiler node
         │                 │                  │
         │                 ├── selected IR root and derived views
         │                 └── remote sync queue (durable)
         └── optional remote index owner ── compiler-node fleet
                                             │
                                local CAS / direct S3 artifact sink
```

Both compiler deployments speak one versioned request and output protocol. A node receives `CompileAssignment { package_key, target, recipe, exact_input_manifest, base_generation, changed_inputs, attempt_fence, output_budget, sink }`. It must either advance a retained session from the named base or explicitly report that a rebuild is required. Rebuild is allowed, hidden reuse is not. Sessions are keyed by stable package/build-target/toolchain *lineage*, while a specific invocation is keyed by the complete input manifest and recipe. Keying the process only by the exact revision defeats stateful incremental compilation because every edit creates a new key.

The GUI does not own a separate compiler implementation. It owns a node lifetime, reserves a small interactive budget, and can request a previous session's advance as soon as a source delta is durable locally. The standalone service exposes the same kernel with service auth, admission, quotas, placement, and a stronger persistent session catalog. The index can route dependency-closed package/target cells among nodes; intra-package parallelism is permitted only where a language authority proves independent scopes. A TS program, Rust crate, Java module, C# project, or C++ translation unit is a real compilation boundary, not an arbitrary list of files sharing a language enum.

## One logical IR, multiple physical layouts

`ObjectVersion<T>` is the domain-separated hash of a complete canonical logical value. `StateRoot<R>` names the visible ordered relation. `GenerationRoot` binds all semantic relations, exact input manifest, recipe, toolchain and coverage. Physical encodings never enter those identities. The local IR-VCS compares borrowed validated readers and their stable keys; the same generation may be read from a local mmap/pack, an in-memory editor overlay, or fetched remote extents. A new remote format is **a layout variant**, not a second semantic format or a lowering/raising path.

Split the full IR image into semantic planes with separate update costs: stable declarations and containment; signatures and types; references/occurrences/graph links; docs/examples and large strings; diagnostics/source provenance; language-specific facets. Each plane has a canonical sorted relation and a coverage witness. A generation manifest names the root of each plane and the package/target scope it covers. Complete, empty, partial, unavailable, and stale are distinct typed values. Every field used by GUI/search/diff is covered by a complete object version or a versioned facet reference; core-only fingerprints cannot silently stand in for docs, visibility, occurrences, or diagnostics.

Small records are sorted into key-anchored immutable microsegments. Choose boundaries from stable key hashes, with a byte ceiling; do not choose boundaries from changing payload bytes or fixed file offsets, which can ripple every later chunk on a small edit. Directory entries carry first/last key, cardinality, plane-specific fingerprints, compressed/uncompressed sizes, and canonical segment ID. A changed declaration should rewrite its segment and a logarithmic set of map nodes; unchanged docs, type dictionaries, and adjacent key ranges remain address-identical. Oversized docs/source blobs use a separately versioned content-defined chunk recipe with min/target/max byte limits and explicit fallback for adversarial or incompressible data. A source file can be large without becoming one enormous allocation or one enormous network object.

Initial *hypotheses to benchmark*, not promises: 64–256 KiB uncompressed semantic segments, 256 KiB–1 MiB target large-blob chunks with a hard maximum, and 8–32 MiB remote packs. Keep small interactive writes in a bounded hot tier, then compact them into larger immutable packs. S3 multipart parts have a 5 MiB minimum except the final part, so the application-level chunks are **not** S3 multipart parts. Packs batch many independently addressed logical chunks. S3 range GET retrieves a physical extent; pack manifests map logical IDs to extent ranges. Because one GET cannot request discontiguous ranges, make pack affinity follow query locality: package + semantic plane + nearby keys, with page/block integrity so a single read need not decompress the whole pack. Cold global compaction may change `LayoutId`/`PackId` while leaving all logical roots untouched. [AWS multipart limits](https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html) and [range-read behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/download-objects.html) constrain the adapter.

Keep the physical lookup replaceable: `ObjectVersion -> { layout_id, pack_id, offset, length, codec, integrity }`. Rebuild it from immutable pack catalogs if necessary. A reader pins an exact layout manifest while reading; the compactor only retires it after the last pin and a safe GC epoch. The local store favors mmap, cheap fsync-batched object admission, and a small hot index. Remote storage favors larger pack PUTs, range reads, geographic caches, and parallel prefetch. Both use the same canonical object decoder/admission rules. Existing `backend-store` canonical nodes, object pack, closure validation, and `backend-replication` checkpoints are the right low-level seed; current full-image publication and in-memory product CAS must stop being mandatory intermediates.

## Stream once, publish once

The compiler emits a borrowed or leased `ArtifactBatch` to a bounded `ArtifactSink`; it never needs to build a full `Vec<IR>` plus fragments plus a full semantic image before sending output. The type boundary should make unpublished bytes hard to confuse with admitted facts:

```rust,ignore
trait ArtifactSink {
    type Session;
    type Receipt;
    fn begin(&mut self, claim: CompileClaim, budget: Budget) -> Result<Self::Session>;
    fn have(&mut self, session: &mut Self::Session, ids: &[ObjectId]) -> Result<HaveBitmap>;
    fn put(&mut self, session: &mut Self::Session, extent: LeasedExtent<'_>) -> Result<ChunkReceipt>;
    fn finish(self, session: Self::Session, closure: CheckedClosure) -> Result<Self::Receipt>;
}
// Only the index owner can turn a checked closure and exact input fence into
// a selected semantic publication. A storage receipt is not that authority.
```

Use lending cursors/GATs for compiler microbatches so the caller cannot recycle scratch before a sink finishes borrowing it. Reuse buffers by size class when the last lease drops. A node sends stable logical IDs and canonical schema/length/hashes in an initial manifest, then transmits only missing chunks. A transfer frame has transfer ID, logical object ID, byte offset/length, sequence and chain digest; a durable checkpoint supports sparse resume. The final manifest is a Merkle closure over all required segment IDs, coverage, exact input reads (including negative/range/config reads), and recipe. The receiver validates each chunk and the whole closure. It is safe for frames to arrive out of order, duplicate, or be retried: the admitted immutable ID either matches or is corruption.

The sink may target (1) the local IR store, (2) the index ingest endpoint, or (3) a scoped S3-compatible object-store route. These are placement choices for the same logical output. A remote node uploading directly to S3 receives only per-object `StoredObjectReceipt`s. The index must read and verify the complete closure and its object receipts before it can seal a `StoredClosureReceipt` and select a generation. An index-receiver can stream into its own CAS and perform that verification locally. Give workers only scoped, expiring upload capability for an authorized work key and prefix; never give them the semantic-head writer credential. If direct S3 is unavailable, a local GUI may still durably publish its local generation and queue remote replication. The user-facing state must distinguish `local-complete/remote-pending`, `remote-complete`, and `remote-rejected`, without revoking the working local view.

An object-store ACK proves storage, **not** compiler correctness or index publication. The owner checks `{ package, target, recipe, input_manifest, selected_base, attempt_fence, closure_root, coverage, schema, authority_proof }` and verifies required closure objects are durably reachable in the chosen store. A rejected stale attempt leaves harmless immutable orphans for GC; it never advances semantic head. The owner can accept independent completed target cells concurrently but atomically selects only a coherent package/target frontier. Per-scope early publication is allowed only with an explicit complete-scope certificate, so readers do not mistake a partial stream for a package generation. Failed/cancelled compilation writes a terminal with reason and retry eligibility; it does not fabricate a completed generation.

For remote S3, upload immutable extents/packs first, then immutable layout/closure manifests, then transact the semantic binding in the **index's** authoritative store. A remote-head CAS can use a conditional S3 write only in a deployment where S3 itself is explicitly the head authority. Do not have both a Turso semantic head and an S3 semantic head: S3 offers atomic update per key, not transactions across keys, while conditional writes have 409/412 retry cases. Turso/index selection after durable S3 closure avoids cross-service atomicity: a crash before selection leaves collectable orphans; a crash after selection sees a complete closure. Use a durable index-side intent/outbox for retries and notification replay. Do not treat S3 ETag as a content hash, especially for multipart uploads; verify application-level canonical IDs and an S3-supported checksum independently. [AWS consistency](https://docs.aws.amazon.com/AmazonS3/latest/userguide/Welcome.html), [conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html), [upload integrity](https://docs.aws.amazon.com/AmazonS3/latest/userguide/checking-object-integrity-upload.html).

## Private compiler cluster transport

The local index owner may place an independent package/target cell on another
compiler node without changing the request or publication authority. Use Iroh
for mutually authenticated, encrypted node connections and Bao-verified BLAKE3
ranges for content transfer. A known endpoint ID proves a transport peer, but
does **not** authorize work: every control stream must also carry a bounded,
short-lived capability bound to `{tenant, package, target, recipe, input root,
attempt fence, allowed object IDs/ranges, byte budget, expiry}`. The receiving
node checks that scope before revealing whether an object is present or opening
a blob stream. Key rotation and revocation must invalidate future requests
without invalidating already admitted immutable objects.

The control protocol has asymmetric roles and closed messages: the owner sends
`Offer`, input `GrantPage`, `Cancel`, `ResultAck`, and
`ResultRetirementConfirm`; the worker sends `Accept`, `WorkerReject`,
`ExecutionFailed`, `ResultReceipt`, result `GrantPage`, `ResultRetired`, and
`ResultRetirementApplied`. A provisional `Accept` authorizes input transfer, not execution
success. `WorkerReject` reports failed admission before compiler execution;
`ExecutionFailed` reports a terminal compile or output-store failure after
admission. Neither can be interpreted as an empty successful result. Both
sides finish the same control stream with a bounded two-sided FIN/EOF
handshake after the terminal retirement exchange; a local flush is not a
durable acknowledgement. An
`Offer` binds the exact
input `ClosureId`, typed compiler-input-manifest `ObjectId`, attempt fence,
recipe, input/read roots, byte budget, page count, and deadline. The manifest is
itself a member of that closure and names the package, target, exact toolchain,
environment, platform, and sorted workspace entries. Its identity must be
verified before any compiler invocation. A complete immutable workspace tree
permits a conservative fresh compile when the execution policy allows it and
a language cannot prove a precise positive and negative read set; it does not
itself provide a sandbox or qualify for a delta/no-op reuse claim. The
manifest cannot name its own object or enclosing
closure, which would create a hash cycle.

Bulk content travels on separate bounded Bao streams. The owner issues signed,
expiring per-object range grants; a worker checks issuer, peer, scope, fence,
closure, range, and expiry before exposing object presence or opening the CAS.
The source serves payload ranges directly from the existing CAS and persists
only Bao outboards and small typed-ID mappings, avoiding a second payload
copy. A receiver persists verified ranges and their exact object hash before
acknowledging progress. On reconnect, it advertises durable coverage and asks
only for missing ranges. Duplicate frames are idempotent; truncated or invalid
proofs cannot advance coverage. The worker's `ResultReceipt` is a claim, not
publication. The owner admits all result objects and the closure, verifies the
typed semantic envelope against the fenced attempt, and selects the exact
head in Turso before authorizing a `Stored` ACK. A checked preselection
rejection has its own durable owner intent. Before either terminal ACK is sent,
the owner fsyncs the exact disposition, peer, assignment scope, and closure.
The worker fsyncs a small retirement tombstone, removes its running and
pending payload records, and replies `ResultRetired`. The owner fsyncs that
receipt before sending `ResultRetirementConfirm`; the worker durably removes
its tombstone before replying `ResultRetirementApplied`. A lost connection at
any cut resumes the same exact disposition or confirmation after restart.
An authenticated duplicate confirmation can be applied without a tombstone
only when no conflicting live state exists. A worker's retained result cannot
be silently discarded on a lost ACK, and a terminal tombstone need not pin its
output payload or consume the pending-result quota. The owner reserves journal
capacity before an Offer can reach a worker and never ages away an ambiguous
offered assignment without an authenticated terminal proof.

Discovery is an explicit trust and deployment choice. A same-machine test uses
known loopback endpoint addresses and no public discovery or relay. A LAN or
remote private cluster may use a configured peer directory or a private relay,
but membership is an allowlist plus scoped capabilities, not possession of a
content hash or mere reachability. The code must allow relay-disabled operation
and expose which path served a transfer so latency and billed bytes can be
measured. The data plane can use a Bao implementation compatible with Iroh's
current blob protocol; pin a production-supported version deliberately rather
than assuming the newest pre-1.0 release is production-ready.

Execution policy is a separate trust boundary from encrypted transport. A
verified input closure proves which bytes the worker received; a temporary
workspace directory is not an OS sandbox and cannot prove what native tools
read or write. Pure in-process parsers may run under bounded admission.
Host-executing language authorities require an explicit, persisted trust grant
for the coordinator and the exact recipe, toolchain, environment, platform,
and workspace scope. A worker lacking that grant rejects the offer before
running a native tool. The owner can compile locally and keep the interactive
path independent of cluster availability.

Acceptance requires real independent processes: wrong endpoint or work scope
is rejected before `Have`, an interrupted range resumes after receiver restart,
tampered and truncated ranges remain absent, cancellation returns credits,
and a stale completed attempt cannot publish over a newer head. Measure direct
and relayed p50/p95/p99 transfer time, bytes sent/resent, peak in-flight memory,
and the closure verification cost. A local interactive compile must continue
when every peer is unreachable.

## The Git-at-scale adaptation

Cursor's [Continuity account](https://cursor.com/blog/git-at-any-scale) demonstrates a useful publication pattern: make data durable before advancing one visible reference, keep local fast replicas, allow replicas to rehydrate, and make compaction a physical operation that followers can consume. The part to borrow is the immutable data/atomic selection discipline and replica-rehydration model. Its Git-pack/WAL format and reported throughput do not transfer to IR workloads; we need key-range query locality and an index-owner transaction that spans multiple semantic relations. A WAL may record remote storage-layout and index-intent transitions, but it is not a second source of semantic truth.

East River's [“What comes after git”](https://ersc.io/blog/what-comes-after-git) argues for one storage engine serving multiple protocols, while retaining Git protocol compatibility and allowing a future jj-oriented interface. Applied here: IR-VCS, GUI, CLI/MCP, compiler transport, and object-store replication are protocol views over one typed object/commit substrate. It does **not** mean replacing the user's Git or adopting a proposed jj-native wire protocol. Their public article does not specify a concrete chunking algorithm; the IR-specific key anchoring above is our design, to be measured and falsified.

## Delta-aware work and vertical indexing

The source owner commits a file/manifest/config delta promptly and records `semantic=pending(input_root)`; it does not hold a global owner lock while discovering files, fetching a registry, running Cargo, compiling, or waiting for S3. A scheduler derives affected target cells from the exact read manifest. It shares unchanged source AST chunks, dependency graph results, type pools, and previous semantic segments where their read sets remain valid. Negative dependency reads and toolchain/environment identity are part of the recipe, not invisible process state. A native authority can declare a conservative rebuild barrier, but the barrier is explicit and measured.

Partition compiler placement by a stable package/target key, with affinity for a retained session and its input closure. Steal work when predicted completion and transfer cost justify it; hedge only bounded stragglers and count both attempts against CPU/memory credits. The index side scales *vertically* through one coherent publisher, shared arrangements, bounded sort/merge operators, and byte-budgeted caches; it can shard read-only projections or package-key ranges only with an explicit global frontier and publication law. Turso, Tantivy, and ANN are projections of selected typed relations. Stable document IDs derive from workspace identity + symbol key, never the changing workspace root. Same docs/search/graph object IDs are reused across GUI, CLI, MCP, and remote subscribers.

Demand-driven hydration requests the smallest authenticated key range and plane needed to render the visible GUI state; adjacent docs and source chunks prefetch under bandwidth credits. The local GUI keeps hot fragments and an exact delta overlay; it can answer instantly from its local selected root while a remote index catches up. `NoChange`, `Changed`, `Unknown`, and `Unavailable` need separate terminals. Subscription cursors bind branch/log/frontier and can reset from a selected root after a gap; a scalar generation is insufficient.

## Concrete crate boundaries

`version` defines IDs, canonical encoding, schema and scope laws. `store` owns logical CAS, Merkle map, layout/pack catalogs, pins and GC, with `store-local` and `store-s3` physical adapters. `semantic` defines IR relations, borrowed readers and complete facet-aware IR-VCS deltas. `compile` owns retained language sessions, exact read manifests, native authority contracts and batch emission; `compiler-node` wires this into either embedded GUI or standalone service. `replication` owns authenticated streaming/checkpoint protocol and storage-independent receipts. `index` owns selected source/semantic/metadata roots, work admission, derived catalog and shared arrangements. `local-service` composes `index + embedded compiler-node` for GUI/CLI/MCP. `worker` executes compiler assignments and pure derived recipes through the same protocol; it is no longer only a summarizer of already-published compiler output. `extensions/{turso,tantivy,qdrant}` remain disposable typed projections. Dependency edges run inward; no compiler crate reads a GUI DTO or writes a Turso semantic head.

## Acceptance laws and measurable thresholds

1. Two edits to one member file retain byte-identical unaffected segment IDs, session process identity, and source/semantic stable keys; changed bytes, node visits, allocations, compiler time, remote bytes, and query latency are reported separately. Compare against cold full rebuild on each language.
2. Source-only, manifest-only, toolchain-only, lockfile-only, missing-import-created, and failed-toolchain-recovered transitions each schedule the necessary work. A no-op does not produce a fake generation. Multi-file TS, Rust workspaces, JS build trees, C#/Java projects, C/C++ headers, Python modules, and Go modules are live fixtures.
3. Embedded and standalone compiler nodes consume identical assignments and produce identical canonical generation roots for the same captured input/toolchain/recipe. Vary host platform deliberately and require different environment inputs where semantics differ.
4. Send the same compiled closure by index stream and by direct S3 upload, including duplicate/out-of-order chunks, interruption, crash/restart, stale attempt, missing extent, checksum mismatch, credential expiry, and index outage. Visible selected roots must be identical where inputs are identical; no incomplete closure may be advertised.
5. Drop/reorder an S3 ACK, kill the writer at every durability boundary, repack while an old reader is pinned, prune while a cursor is slow, and hydrate onto an empty node. The model is old-or-new selected root, never mixed. Verify object-store bills, PUT count, range amplification, compaction write amplification, and cold restore time rather than reporting only a warm median.
6. Fuzz canonical encoders and dependency identities with ambiguous adjacent fields; run a small independent reference model for state/delta/coverage laws, mutation-test the assertions, and use real process/network fault injection. Dan Luu's [agentic testing findings](https://danluu.com/agentic-testing/) are a reminder that naming “fuzzing” or “property testing” is not evidence that an agent selected strong properties.

This is a cutover specification, not a performance claim. Initial chunk sizes and cache policies must be selected from cold/warm traces across small projects, million-symbol monorepos, package-registry corpora, constrained laptops, and remote high-latency stores. The governing result is the measured changed-work ratio with correctness under failures.
