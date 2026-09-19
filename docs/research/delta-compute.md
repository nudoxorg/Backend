> **Research, not the final specification:** use the [final v2 index](../architecture/README.md). These reports preserve alternatives; final v2 equations, package boundaries and corrections take precedence.

# Delta-native compiler and workspace computation

This design makes incremental change propagation the primary compute model for the versioned compiler object store and all derived consumers: documentation, search, graph, vector, and UI projections. It is grounded in the current repository and in differential dataflow, DBSP, and self-adjusting computation literature. No production files were changed, and no performance claim is made without the measurements described below.

The repository already exposes the right seed. `compiler/ir/vcs.rs` represents snapshots as a generation identity plus borrowed canonical `Ir` (`:15-40`), merges stable entity identities across two images (`:82-149`), and emits allocation-free stable link deltas (`:180-323`). `compiler/ir/reader.rs` exposes typed iterators and direct keyed access over entities, links, occurrences, docs, and seven language extension planes (`:102-223`). The missing layer is a durable change algebra that consumes these deltas once and feeds every downstream view.

## The design decision

Use a versioned weighted relation store. A revision is a set of immutable object and relation rows. A change is a batch of `(row, time, weight)` updates, where `+1` inserts or retains a row and `-1` retracts it. Derived relations are maintained by differential operators; they do not each compute a before/after diff. A revision frontier makes a result visible only after all required updates have arrived and durable side effects have crossed an explicit fence.

This chooses the algebra used by Differential Dataflow: collections are update triples `(data, time, diff)` and nested iteration supports incremental recursive computations. The original paper reports incrementally maintained strongly connected components as a motivating case ([Differential Dataflow, CIDR 2013](https://www.microsoft.com/en-us/research/publication/differential-dataflow/)). DBSP provides the complementary formal model: incremental view maintenance for relational queries, grouping/aggregation, monotonic and non-monotonic recursion, and streaming aggregation ([DBSP paper](https://arxiv.org/abs/2203.16684)). Self-adjusting computation supplies the trace perspective: dynamic dependence graphs record data/control dependencies and change propagation reexecutes affected portions, while memoization reuses unchanged work ([Acar et al., experimental analysis](https://www.cs.cmu.edu/~blelloch/papers/ABBHT09.pdf)).

The system should borrow these ideas without pretending native compiler authorities are pure relational operators. The authority boundary emits a versioned fact batch when it can prove a delta; otherwise it emits a replacement snapshot for the affected authority scope. Every consumer still sees one common weighted change stream.

## Repository mapping

The current pipeline has one coherent lowering transaction. `compile_semantic` collects once, builds owned `Ir`, and writes a compact fragment from the same fact set (`compiler/driver/types/compile.rs:49-94`). `FactSet::build_ir` materializes identity, types, docs, occurrences, and extensions (`compiler/driver/lower.rs:1824-1914`). `CorePayloadHash` is intentionally partial: declaration shape, type structure, ordered product children, and local member bases are included, while docs, visibility, extensions, provenance/spans, opaque parentage, and occurrences are excluded (`compiler/ir/semantic.rs:2326-2373`; `compiler/driver/README.md:25-37`). The delta system must preserve that distinction.

Publication currently produces immutable fragments/images, manifests, generation roots, bindings, and journal receipts. `publish_semantic` measures/encodes images, verifies provenance, stores artifacts, verifies a complete generation, and submits a durable publication (`compiler/publication/publication/publish.rs:100-295`). Application then reopens the same durable generation and revalidates all artifacts (`compiler/application/compiler.rs:317-367`; `compiler/publication/publication/open.rs:137-283`). In the delta design, publication becomes one sink of a shared change batch. The immediate in-memory verified result can feed downstream consumers; durable reopen remains a recovery/integrity operation.

## Object and relation model

Objects are immutable content-addressed values. Relations are typed rows whose keys use stable identities, never generation-local IDs:

```text
Object(object_id, schema, bytes, length)
Source(scope, source_id, profile, stage, toolchain, authority_key)
Entity(scope, declaration_identity, family, variant, core_payload, availability facets)
Type(scope, type_key, canonical shape)
Member(owner_identity, member_identity, role, ordinal)
Link(from_identity, target_key, kind, evidence)
Occurrence(owner_identity, occurrence_key, target_key, confidence, span)
Documentation(owner_identity, doc_key, ordered fragment)
Extension(language, owner_identity, extension_key, payload)
Embedding(owner_identity, embedding_key, model, vector)
IndexRow(lane, key, rendered/tokenized payload)
```

`DeclarationIdentity`, `DeclarationLinkTarget`, and `StableLinkKey` already provide suitable stable endpoints (`compiler/ir/vcs.rs:52-69, 180-195`). Local `EntityId`, `TypeId`, `AtomId`, and list IDs remain local coordinates inside one immutable `Ir`; they are never external delta keys. A relation update may carry a local ID only as an opaque evidence coordinate tied to the source generation, then normalize it to stable identity before entering the shared store.

Every row carries a `valid_from` revision and is changed by weighted batches. Retractions use the exact prior row key and a negative weight. Updates to evidence are a `-1` of the old `(key,evidence)` row plus `+1` of the new row; a consumer can derive `EvidenceChanged` as the paired lifecycle event. This generalizes the current link classifier (`compiler/ir/vcs.rs:245-323`) without losing its API.

Facet availability is data, not absence. A missing documentation row can mean “known empty,” “unavailable authority,” or “not requested”; encode that state in the relation or a side fact. This is essential for correct negative dependencies and for preventing an incomplete authority image from retracting known data.

## Change capture and full diff

Change capture is preferred. A compiler authority or object writer emits the exact before/after row changes at the moment it commits a new immutable object. For a source edit, the source store emits a `Source` replacement and the lowering stage emits changed entities, members, links, occurrences, docs, extensions, and facet availability. Captured deltas preserve deletions that a new partial image cannot infer.

Full diff remains the correctness fallback and audit path. Given two complete `Ir` snapshots, `Diff::between` already performs stable ordered entity and link merges in linear time over canonical indices (`compiler/ir/vcs.rs:82-149, 262-370`). Extend this to every relation plane and compare facet availability explicitly. Full diff is needed after crash recovery, import of an older store, authority upgrades without change logs, and verification of capture implementations.

Do not derive a deletion merely because a new authority report omitted a row unless the report proves complete coverage for that relation and scope. The report/image owner must provide a coverage certificate: source binding, authority configuration identity, schema, and complete/partial scope. Partial coverage produces additions/updates only and leaves prior rows intact.

## Weighted batch algebra

Let `R` be a relation represented by weighted rows. A batch `ΔR` is a finite multiset of `(row, time, weight)`. Consolidation sums equal keys and removes zero totals. The core operators are:

```text
map_f(ΔR)       = {(f(row), t, w)}
filter_p(ΔR)    = {(row, t, w) where p(row)}
concat(A, B)    = A ∪ B, then consolidate
join(A, B)      = {(a ⨝ b, max(t_a,t_b), w_a*w_b)} on join key
group_sum(R)    = grouped rows with additive weights
distinct(R)     = presence derived from nonzero total weight
```

Joins naturally retract: if either input row receives `-1`, the matching output contributions receive negative weights. Aggregates maintain a group accumulator; an update from count 3 to count 2 is `-1` for the removed input, and the old/new materialized aggregate can be represented as a replacement row. Non-additive aggregates such as min/max need a differential arrangement or a bounded multiset per group; do not implement them as “subtract the old scalar” without retaining enough evidence.

The compiler relation graph can be expressed as:

```text
Source
  -> syntax declarations
  -> authority declarations/types
  -> stable entities and members
  -> links/occurrences/docs/extensions
  -> canonical image and publication objects
  -> docs/search/graph/vector/UI projections
```

Each arrow is a relation operator with an explicit dependency key. Lowering may emit a batch directly or replace a scope snapshot. Canonical image encoding is a sink that consumes a logically complete frontier, not an operator that mutates partially visible durable files.

## Frontiers and visibility

Use a product time `(revision, phase, scope)` with a partial order: a frontier is an antichain of incomparable minimal times. A downstream operator may finalize a key only when its input frontiers dominate the key’s required time. This is the same purpose as Timely/Differential frontiers: progress knowledge allows compaction and prevents emitting results that may still be invalidated.

Recommended phases:

```text
Captured < Parsed < Authority < Lowered < Canonical < Published < Indexed
```

A source edit may have several in-flight times. Documentation can consume `Lowered` if it needs only owned IR; vector indexing may require `Published` if it reads immutable images; a remote memoized result may be admitted only at `Published` with a matching `WorkKey`. The revision is visible to UI only when the selected query’s demand frontier is complete.

Frontier rules:

* A negative update must carry the same or later revision as the row it retracts; reject a stale retraction instead of deleting a newer row.
* A batch cannot be compacted across a frontier until no future update can arrive at an earlier time.
* Publication is a side-effect fence: durable journal receipt and immutable artifact sync must precede advancing `Published`.
* Index and UI acknowledgements advance independent downstream frontiers; failure of a vector provider must not retract compiler truth or block lexical/search lanes that have already advanced.

## Recursive joins, aggregates, and SCC invalidation

Recursive semantic relations include module containment, type aliases, inheritance/implements edges, call/reference reachability, and package dependency closure. Maintain recursive results by SCC. The algorithm is:

1. Convert changed stable links and entity membership into weighted edge deltas.
2. Recompute the affected SCC condensation region, including predecessors that can reach changed SCCs and consumers that depend on their summaries.
3. Apply edge retractions and insertions to the SCC relation until the local fixed point is reached.
4. Emit changed SCC summaries and downstream reachability rows at a later iteration timestamp.

An SCC key is content-addressed from sorted member stable identities, sorted internal edge keys, sorted external dependency keys, and authority/schema facets. A member addition or edge change invalidates the SCC and all recursive aggregates that depend on it. A change in a leaf outside an SCC does not rebuild unrelated components.

For aggregates such as “public members,” “incoming references,” search term counts, graph degree, or package declaration counts, use weighted grouped relations. Keep witness rows for non-monotone aggregates and confidence/availability facets. A visibility change can retract an index row while leaving the declaration row present. A documentation deletion retracts exact doc fragments and embedding tokens; it does not change the declaration’s core payload.

Negative dependencies are first-class. Record both positive evidence (`depends_on(A,B)`) and a coverage/witness fact (`authority_scope_complete(A,scope)`). An absence-dependent result, such as “no override,” is valid only when the complete witness frontier has advanced. If a later authority image expands scope, retract the old negative result and recompute. This avoids stale “no result” caches.

## Demand-driven scheduling

The scheduler starts from demanded sinks: a UI page, search query, graph neighborhood, vector refresh, or durable publication. It walks dependency edges backward to identify required relation keys and phases, then schedules only missing operators. A compiler request creates demand for its source/SCC and any explicitly requested publication/index outputs.

Use bounded work queues per phase and per authority class. A ready item contains `(WorkKey, input frontier, demand priority, cancellation token)`. After each batch, consolidate and enqueue only changed downstream keys. No eager future for every file/chunk is created. No global cache is consulted without an explicit runtime owner and complete key.

When multiple sinks demand the same relation, share the immutable batch result through the owner-local object store and advance each sink frontier independently. If the query is cancelled, remove only its demand; retain already committed immutable objects if other demands reference them. A failed side-effect sink records a retryable fence failure and leaves upstream semantic rows available.

## Native authority limits and side-effect fences

Native authority is not generally incremental. `libclang` collection builds bounded authority arrays and the Clang projector performs representative selection, anchors, members, topology, occurrences, and docs in ordered passes (`compiler/driver/lower/clang.rs:260-289, 533-590`). Rust analysis runs a project authority callback and then a multi-pass emitter over HIR, macros, occurrences, and docs (`compiler/driver/lower/rust.rs:130-158, 257-320`). TypeScript combines OXC syntax with a checker report across seven ordered passes (`compiler/driver/lower/typescript.rs:1593-1657`). These authorities may invalidate large scopes when a header, Cargo graph, macro expansion, checker configuration, or package image changes.

Incremental lowering can still avoid downstream recomputation. If an authority cannot emit a trustworthy delta, rerun it for the smallest source/package/SCC scope, then emit a replacement batch for that scope. Never splice authority rows across incompatible report/image identities. Native child processes, Java harnesses, Rust databases, and external checker invocations are side-effect fences: their output becomes visible only after source binding, toolchain identity, cancellation, diagnostics, and schema validation succeed. The current native work protocol requires exclusive empty directories and cleanup after child reaping (`compiler/driver/native/frontend.rs:172-237`); a delta worker must preserve that fence.

Authority sessions may be reused only when their configuration and dependency graph are immutable and the crate proves safe reuse. A reused session still emits a new authority identity or revision binding. Do not memoize a partial native parse under source bytes alone.

## Stable keys versus local IDs

Stable declaration identity is scope plus family/variant. The driver explicitly keeps source content, row order, and spans out of declaration identity (`compiler/driver/types/request.rs:15-84`; `compiler/driver/lower/identity.rs:1-7`). This is correct for cross-generation joins. `EntityId` and other dense coordinates are generation-local and must be remapped at every object boundary. The canonical image planner already creates sorted order and raw-to-canonical remaps for atoms/entities/externals (`compiler/ir/semantic_image/canonical.rs:78-162, 257-277`), while full semantic pools similarly sort keys and build remaps (`compiler/ir/semantic_image/full/model.rs:198-234`).

Delta records should carry stable keys plus optional `(generation, local_id)` evidence. A join on local IDs is illegal unless both rows name the same generation object. Canonical merge must sort stable keys, reject duplicate keys, and rewrite every local target before emitting a new object. Completion order from parallel workers must never influence canonical row order.

## Remote memoization

Remote memoization stores immutable successful relation/object batches keyed by complete `WorkKey` and schema. A remote hit is accepted only after validating source/authority/toolchain/dependency facets and object bytes. Remote negative results are short-lived and tagged with the exact complete coverage frontier; transient failures are never cached as empty truth.

Remote storage is a content-addressed object map plus a small signed/indexed key manifest. It should exchange weighted batches or immutable scope snapshots, not mutable pointers to local IDs. A remote batch can be merged only after stable-key normalization and schema validation. Publication receipt identity remains local durable authority; a remote object is a computation memo, not proof that this workspace committed it.

## Downstream consumers

Documentation consumes entity/member/type/doc/link relations. A documentation page update is the delta of affected stable entities and fragments. It may preserve a page when only an excluded core payload facet changes, but it must rebuild when docs, visibility, extensions, or graph context change. This directly follows the current documentation reader’s typed doc/link access and the payload coverage restriction.

Lexical search consumes token/document relations and posting-list aggregates. An entity rename retracts old tokens and inserts new ones; a visibility change retracts or inserts access-controlled rows. Graph search consumes stable link rows and SCC/reachability aggregates. Link evidence changes update evidence rows without pretending endpoint identity changed.

Vector search consumes a separately keyed embedding relation. The embedding key includes model/version, tokenizer policy, and all text facets used. Since `embedding_text()` includes signatures, docs, and typed graph context (`compiler/ir/README.md:73-82`), core payload equality is never sufficient for vector reuse. A docs-only change can require vector retraction/reinsert even if `CorePayloadHash` is unchanged.

UI state consumes revisioned query results and frontiers. It should receive a coalesced delta stream keyed by stable identity, with tombstones for removed rows and a frontier watermark. UI can show a previous committed revision while a new one is incomplete, but must not mix rows from incomparable revisions in one snapshot.

## Cold and warm planning

Cold planning reads durable object manifests, reconstructs relation arrangements, verifies generation roots, and establishes frontiers. It may use the current full reopen path as a correctness bootstrap (`compiler/publication/publication/open.rs:137-283`). Warm planning starts from retained arrangements and owner-local dependency indexes, applies captured weighted batches, and schedules only affected keys.

The cold path should produce checkpoint objects: relation segment IDs, frontier, schema, dependency summaries, and arrangement statistics. Warm restart loads checkpoints, validates their object identities, and replays batches after the checkpoint. If validation fails, fall back to cold full diff/rebuild. Do not let a warm cache silently substitute an incomplete relation.

## Memory, compaction, and traces

Each worker records memory traces: live arrangement bytes, batch bytes before/after consolidation, retained object references, frontier count, queue depth, per-operator allocations, and bytes pinned by active readers. Trace spans should identify `(WorkKey, operator, revision, phase)` without retaining source or authority bytes in telemetry.

Compaction is frontier-driven. Before a frontier advances, retain enough historical weights to answer active before/after queries and to retract rows from all downstream arrangements. Once no reader or pending demand can observe earlier times, consolidate equal rows, merge immutable segments, and discard zero-weight entries. Keep a bounded rollback window for UI/session snapshots; make its size explicit.

An arena pool should use size classes and trim oversized failed requests. Native authority memory and parser sessions are released at their side-effect fence; immutable relation batches can outlive the worker only through content-addressed object references. Never retain an entire prior `Ir` solely because one UI reader still needs one page; materialize a page batch or hold an explicit immutable object lease.

## Migration that makes deltas core

1. Define `Revision`, `Frontier`, `StableRowKey`, `WeightedUpdate`, `CoverageFact`, and `WorkKey` in a dependency-light crate.
2. Add a complete all-plane `Ir` change capture adapter beside the existing `Diff::between`; use full diff as the oracle and compare captured batches against it.
3. Store one immutable relation batch/object manifest per committed compiler generation. Continue emitting the existing compact/semantic artifacts as publication sinks.
4. Convert documentation, lexical search, graph, vector, and UI adapters to consume stable weighted batches. Keep their current APIs as snapshot projections over the relation state during migration.
5. Add owner-local arrangements and frontier scheduler. Start with compiler entities/members/links/docs; then extensions/occurrences; then search/vector/UI.
6. Add SCC maintenance and grouped aggregate operators. Test insertion, deletion, evidence replacement, partial authority coverage, and late negative dependencies.
7. Add remote memoization for validated immutable relation/object batches keyed by complete `WorkKey`.
8. Make publication and external indexing explicit side-effect fences. Only advance their frontiers after durable receipts or provider acknowledgements.
9. Delete per-consumer before/after rescans only after differential results match full diff on a corpus and after crash/replay tests pass.

## Non-negotiable tests and measurements

Correctness tests must compare weighted incremental state against full rebuild for random source edits, declaration moves, overload additions, member reorderings, docs/visibility-only changes, occurrence evidence changes, extension changes, package dependency updates, SCC edge edits, and authority scope expansion. Test duplicate stable keys, stale retractions, late updates, zero-weight consolidation, and incomparable frontiers.

Measure cold full rebuild versus warm delta application by changed source bytes, changed declarations, affected SCC size, downstream fanout, batch size, arrangement bytes, compaction time, queue latency, and peak retained memory. Measure authority rerun scope separately from downstream propagation; otherwise a native full rerun can hide large incremental savings below it. Measure side-effect fence latency and recovery replay time.

The design is successful only if it preserves exact stable-key results and improves work for small changes. It is acceptable for a broad authority invalidation or a large SCC edit to approach full rebuild cost; the scheduler should detect that condition and choose a full scope replacement instead of paying differential overhead.

## Sources and evidence boundary

The differential algebra and frontier claims here are based on the primary Differential Dataflow paper ([McSherry, Murray, Isaacs, Isard, CIDR 2013](https://www.microsoft.com/en-us/research/publication/differential-dataflow/)), DBSP’s incremental view maintenance paper ([Budiu, McSherry, Ryzhyk, Tannen, arXiv:2203.16684](https://arxiv.org/abs/2203.16684)), and self-adjusting computation work ([Acar et al., ACM paper PDF](https://www.cs.cmu.edu/~blelloch/papers/ABBHT09.pdf); [consistent semantics](https://arxiv.org/abs/1106.0478)). The repository-specific claims are tied to the paths and lines cited above. Whether the native authority crates can expose stable change capture, whether all authority sessions are reusable, and whether warm arrangements fit memory limits remain implementation questions requiring source inspection and benchmarks.
