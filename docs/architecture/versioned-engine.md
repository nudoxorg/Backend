# A versioned object engine for local-first, incremental computation

This is the replacement architecture, not an addendum to the first plan. The first plan treated persistent objects and incremental computation as later optimizations. Here they define the system from the beginning. Local-first execution and remote acceleration are part of that same engine.

The recommendation is to build **one versioned object and relation engine**. Its shared currency is an immutable state root and a typed, version-bound change batch. Storage preserves unchanged subtrees and column segments; computation propagates changed facts through shared arrangements; synchronization transfers missing immutable objects; queries and UI subscribe to derived version changes; local and remote workers execute the same pure recipe against the same input versions.

```text
                versioned inputs / user intents
                            │
               atomic local commit + typed Δ
                            │
             shared incremental computation graph
                  │                    │
             local workers       remote workers
                  └──────────┬─────────┘
                     result objects + Δ
                            │
         semantic facts / indexes / documents / UI views
                            │
                  versioned subscriptions
```

This is a commitment to a delta-native end state. Migration keeps existing code as adapters and correctness oracles, then removes the duplicated mechanisms. It does not postpone the central engine until after dozens of unrelated cleanups.

## 1. What becomes one mechanism

| Today | Target |
|---|---|
| Root comparison, IR VCS diff, index shadow handling, shelf refresh, UI invalidation, remote synchronization each discover change differently | Typed change batches from version commits; Merkle diff only when provenance is absent or roots diverge |
| Whole semantic images and generation-local coordinates dominate reuse boundaries | Stable logical object keys, complete facet versions, and immutable ordered object maps; dense coordinates stay local to a loaded batch |
| Separate exact/lexical/graph/document projections and caches | Shared versioned arrangements over typed fact relations, with domain-specific operators |
| Compilation, local admission, remote routing, cache lookup, and prefetch are separate policies | One demand graph choosing reuse, delta update, scoped recomputation, and placement |
| Storage packing, publication state, and semantic identity are easily coupled | Logical state identity separated from commit history, transition identity, and physical layout |
| Every surface reconstructs local state and performs broad reads | One local workspace engine; CLI, MCP, and GUI consume bounded view deltas |

The unification is in **state representation, delta algebra, retained arrangements, and work placement**. Compiler semantics, conflict resolution, transaction effects, and ANN search do not become one generic function. They become typed recipes and effects on the common substrate.

## 2. The existing code already exposes the seam

`heart/root/diff.rs` performs a correct linear merge over two complete roots; `changed_diff` still visits unchanged rows. It should become a Merkle subtree diff over a persistent ordered root, retaining the existing merge as the unequal-leaf oracle. [Current root diff](/Users/mileswirht/Downloads/backend/heart/root/diff.rs:159).

`compiler/ir/vcs.rs` supplies stable entity/link comparisons, but `retained_change` compares variant, core payload, and parent. Documentation, visibility, extensions, source evidence, and occurrences are not a complete change feed through that API. The new engine therefore needs complete typed facets, not a wrapper around the existing partial diff. [Current entity changes](/Users/mileswirht/Downloads/backend/compiler/ir/vcs.rs:152), [documented hash coverage](/Users/mileswirht/Downloads/backend/compiler/driver/README.md:28).

The owned IR already has columnar storage, interned pools, typed IDs, sparse extensions, and precomputed CSR. Those become the hot semantic kernels and batch layouts inside the engine. We keep their useful physical properties while replacing the whole-image ownership boundary. [Existing reader contract](/Users/mileswirht/Downloads/backend/compiler/ir/reader.rs:83), [existing adjacency](/Users/mileswirht/Downloads/backend/compiler/ir/semantic.rs:6149).

The current application runtime has a serial compiler owner; routing operates on selected index segments; the new library facade remains partly disconnected. The new engine supplies an actual shared local owner and unified work graph rather than just renaming those components. The source evidence and detailed laws in the first audit remain useful, but its top-level architecture and folder map are superseded.

## 3. Identities that make the design possible

These are typed, domain-separated canonical hashes or stable scoped keys; none is an alias for another.

| Identity | Meaning | Changes when |
|---|---|---|
| `ObjectKey<T>` | Logical identity within a namespace/schema | Domain identity changes, such as an authority-established rekey; ordinary value replacement keeps the key |
| `ObjectVersion<T>` | Complete canonical value of one object/facet, including availability and reference semantics | That value changes |
| `StateRoot<R>` | Canonical visible mapping/relation contents at a snapshot | Visible content changes, regardless of edit history |
| `WorkspaceRoot` | Canonical root-of-roots for an atomic authoritative snapshot, including schema, selected relation roots, provenance and coverage | Any selected authoritative state or its basis changes |
| `CommitId` / `DeltaId<R>` | Commit ancestry/authority, or a checked transition with exact base and target roots | History or transition changes even if final state converges |
| `LayoutId` / `PackId` | A materialization/trace checkpoint or physical encoded pack | Compaction, packing, codec, encryption, or placement changes |

Two edit histories that produce the same typed relation under the same canonical schema must produce the same `StateRoot`. They may have different `CommitId`s. Compaction must preserve state and commit identities while changing physical layouts. A delta is bound to `base_root` and `target_root`; it is not a free-floating patch that can be applied to a vaguely similar image.

### The workspace is an atomic root-of-roots

`WorkspaceRoot = H(schema_versions, ordered_authoritative_relation_roots, basis_manifests, coverage)`. A `CommitId` binds its parent commit(s), exact base/target workspace roots, authority and transaction metadata. A `WorkspaceDelta` binds all affected relation deltas to that same atomic transition. Publication changes one selected workspace-head pointer; it never publishes documentation from one authority transaction alongside entity/type facts from another by accident. Unchanged relation roots are reused in the manifest.

Source edits can be acknowledged before semantic analysis completes. The new workspace root then contains the new source root and explicitly pending semantic coverage with the previous semantic plane's actual source basis. A query requiring fresh semantics waits for that coverage or returns a typed pending/unavailable result. A query accepting stale semantics pins the prior coherent semantic view and exposes its basis. Arrival of accepted compiler facts advances the relevant entity/docs/type/edge/evidence relations together after checking the expected source manifest and authority fence.

Derived arrangements and UI views have independent `ViewRoot`s bound to exact input roots and completed frontiers. Caching or scrolling does not create workspace commits. A work key includes its actual input manifest, not blindly the entire workspace root including its own output; otherwise output publication would invalidate its producer. A complete workspace root is the coherence context, while declared/read-validated facet dependencies determine reuse.

Canonical root computation is part of committing. If a pathological boundary update exceeds its resource budget, retain the old committed head and retry as a budgeted rebuild, or reject the transaction. A provisional delta/layout may be staged, but cannot claim a final `StateRoot` or become authoritative until canonical nodes and the root are finished. There is no delta-only committed-root shortcut.

An entity is a small immutable **facet manifest**: declaration identity, header/type facts, documentation, visibility, language extension, containment, edge/occurrence evidence, and source-location facet roots. A docs edit changes its docs facet and enclosing manifest, but type/graph consumers subscribe to the facets they use and therefore see no type/graph change. Every field belongs to a facet and every unavailable/captured-empty state is represented. This replaces the incomplete general-purpose payload hash with complete, selectively consumed versions.

Do not promise stable declaration keys across every rename or ambiguous overload edit. Preserve the existing authority's actual identity laws; represent genuine identity changes as delete/insert. An explicit rename/alias operation can preserve user intent when the authority establishes that correspondence. Local `u32` coordinates are never persisted as cross-version logical identity.

## 4. The storage structure: persistent ordered Merkle maps with columnar payloads

Choose a **key-anchored prolly-style Merkle B+ tree** for canonical ordered maps. Internal nodes contain separator keys and child logical hashes; leaves contain sorted stable keys and immutable value-version references. Boundaries are deterministic from keys and a versioned chunking policy. A value-only edit should not move neighboring key boundaries. Point updates path-copy changed leaves and ancestors; equal child hashes let diff and synchronization skip complete ranges.

This choice supplies ordered scans, range partitioning, structural sharing, deterministic reconstruction, and subtree comparison together. Ordinary mutable B-tree split history is not a canonical state identity. A Merkle radix trie remains a useful measured alternative, but the selected ordered tree serves both entity maps and ordered query arrangements. Deterministic content-based splitting is supported by the original implementers' [prolly-tree description](https://www.dolthub.com/blog/2024-03-03-prolly-trees/).

Persist the key encoding, ordering, hash domain, cut policy, minimum/target/maximum leaf metadata size, and internal-node rule. Use small fixed-size value references so a huge documentation string cannot blow up the directory leaf. Large values live in separately versioned objects. For raw source/blob bytes, use byte-content-defined chunks rather than key boundaries; [FastCDC](https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia) provides relevant chunking techniques, not a complete ordered-map implementation.

Boundary changes are **expected local**, not a hard constant-time guarantee. Adversarial keys, long forced-cut runs, a large range edit, or a changed chunking policy can rewrite much more. The complexity claim is proportional to changed boundary regions plus ancestors and emitted output, with a worst case that may approach the whole map. Hash quality is not a substitute for quotas on hostile input.

### Logical trees, physical traces, and packing are different layers

The canonical logical map is updated by the small commit mutation set. It does not identify itself by “base root plus every historical delta.” The latter describes a physical reconstruction plan and changes during compaction. Base segments and delta runs are storage/execution choices under a stable logical `StateRoot`.

Physical packs group many same-schema object values and relation rows into columnar segments. A location index maps logical versions to `(PackId, segment, row/extent)`. Repacking changes that index and pack identity; it does not change object versions. A physical pack is hashed and validated independently, and decoding must reproduce the referenced canonical values. Location hints are never semantic proof.

Hot execution uses a dense local object dictionary and typed 32-bit row handles. Full hashes sit in boundary dictionaries and manifests, not in every inner-loop edge. Read-only batches carry a dictionary/segment brand so handles from different owners cannot be joined accidentally. Large immutable owners are pinned at segment/page granularity; mutable scratch and output leases are separate.

## 5. One delta algebra, with two explicitly connected levels

An authoritative object-map change is:

```text
MapChange<T> = (ObjectKey<T>, before: Option<ObjectVersion<T>>,
                              after:  Option<ObjectVersion<T>>)
ObjectDelta<T> = (base StateRoot<T>, target StateRoot<T>, ordered MapChange<T>[])
```

Applying it checks the exact base and expected before-values. Composition requires adjacent roots; inversion swaps before/after and roots. Repeated delivery is recognized by transition/commit identity and does not apply twice. A stale patch is rejected or explicitly rebased, not accepted through last-writer convenience.

`PatchColumn`, `Reparent`, rename, and range replacement are mutation intents or compact physical encodings. Commit normalization resolves them into exact before/after object versions and a checked workspace transition. They are not additional incompatible canonical delta algebras. Atomic validation includes unchanged roots in the workspace manifest, so independently valid per-relation patches cannot be mixed across transactions.

The compute engine lowers those changes to consolidated typed **weighted relation updates**:

```text
remove old row => (old row, logical time, -1)
insert new row => (new row, logical time, +1)
```

This supplies cancellation of equal insert/retract work, incremental joins, aggregate support, and shared arrangements. A committed map must still contain at most one value per key; intermediate relational weights do not replace object-map conflict/validation rules. Use checked integer weight arithmetic and explicit bag/set semantics.

For a join, using old states on the right-hand side:

```text
Δ(A ⋈ B) = (ΔA ⋈ B) + (A ⋈ ΔB) + (ΔA ⋈ ΔB)
```

Or use a sequential update convention with one updated input and exactly two terms. Do not mix conventions and double-count the cross term. Distinct, min/max, top-k, joins, anti-joins, and recursive rules need retained support state; a negative row is not enough by itself to implement them.

This uses the foundations of [Differential Dataflow](https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf) and [DBSP](https://arxiv.org/abs/2203.16684). The architectural decision is to use their change-oriented execution model for this product's typed relations. It is not to wrap every scalar helper in a database engine or claim an off-the-shelf runtime solves native compiler invalidation.

### Capture deltas at the owner, not at every consumer

1. Local edits and transactions record their mutation sets directly.
2. Incremental authorities emit facts for their changed scopes.
3. Non-incremental authorities emit a scoped replacement; the compiler adapter compares that scope once, yielding complete facet deltas.
4. Imports and disconnected peers compare Merkle roots, skipping identical subtrees.
5. All downstream consumers use that one complete transition.

Rebuilding two complete IRs and diffing them in every renderer/index is not delta-native execution. The native authority may still need to reanalyze a package; the engine cannot invent internal incrementality it does not expose. It can prevent that boundary from causing a global downstream rebuild.

## 6. Shared arrangements replace independent index machinery

An **arrangement** is a retained ordered index of typed relation rows and their updates, shareable by many operators. It has a recipe, input root/frontier, key/value layout, immutable base segments, consolidated delta runs, statistics, and explicit retention leases. A common spine merges sorted batches and supports seeks, range joins, and scans.

Canonical relations include:

```text
Entity(key, header_version)
Facet(entity, facet_kind, value_version, availability)
Contains(parent, child)
Name(normalized_name, entity)
Term(term, entity, field, position)
Edge(source, kind, target, evidence)
Occurrence(edge, source_anchor, evidence)
Document(entity, render_recipe, fragment_versions)
Embedding(input_version, model_recipe, vector_version)
ViewRow(query_key, row_key, position_key, projected_version)
```

Exact lookup, lexical membership, forward/reverse graph adjacency, document projections, source joins, and visible UI rows become arrangements or recipes over these facts. Domain-specific schemas remain typed. The common engine owns batch admission, version binding, ordering, delta merge, retention, checkpointing, and placement. It replaces separate lifecycle/cache/shadow implementations for each consumer.

Store both useful graph directions as shared arrangements, retaining CSR-like contiguous hot segments for scans. A batch of changed edges updates only touched key ranges. Top-k retains candidates/refill state rather than rescanning the world whenever one visible row disappears. Global ranking statistics are explicit dependencies: a corpus-size/IDF change can alter scores broadly, so late score calculation or bounded approximate policies must be a deliberate query contract.

Tantivy/Trustfall/Qdrant become query-language, specialized-index, or remote-service extensions to this engine, not alternate identity and snapshot authorities. Their useful specialized execution stays available. Common exact/name/edge/document operations should not require three independent persistence stacks.

### Demand is an ephemeral versioned control relation

Maintain `Demand(consumer, recipe, key/range, freshness, priority)` as explicit state. A visible UI page, active query, sync interest, or background policy contributes demand. Removing the last demand releases unnecessary traces and cancels or defers pure work. Changes may invalidate a cold derived view without immediately materializing it; the engine records its dependency transition and catches up when demanded.

This is bounded, leased control state with its own execution epoch, not durable replicated workspace history. Scrolling, transient subscriptions, and scheduler priorities must not churn commits or synchronize personal activity. Durable user intent such as a pinned package or offline availability rule is a separate authoritative relation; it produces ephemeral demand when an engine opens that workspace. Restart reconstructs demand from live clients and durable policy.

Share common arrangements and subexpressions across consumers. A consumer does not own a separate full name index simply because it has a different output format. Selection/key/recipe identity decides reuse. Backpressure bounds demand rows, subscriber queues, arrangement memory, and materialization work.

## 7. Versions and progress are not the same clock

Keep three concepts distinct:

- **Commit ancestry/state roots:** immutable application history and selected state.
- **Execution logical time/frontier:** which updates can still arrive in a dataflow, including iteration time for recursive computation.
- **Physical layout/checkpoint epoch:** which retained runs and packs materialize state.

For one selected branch subscription, map admitted commits to a monotone execution epoch with an explicit epoch→commit/root binding. Recursive operators can add an iteration coordinate. Partition/worker IDs are routing identities, not automatically timestamp dimensions. Offline branch heads are not made comparable by numerically sorting hashes or branch IDs.

Use separate Rust types: `HeadSet<CommitId>` for commit DAG heads, `EpochBinding { epoch, commit, workspace_root }`, `Time { epoch, iteration }`, `Upper`/`Since` antichains for execution progress/retention, and `LayoutEpoch` for physical replacement. Phase and authority scope are metadata unless an operator has a proved reason to make them logical-time coordinates. Branch reconciliation is a schema-specific three-way merge against a common base followed by a new commit; it is never a weighted union of two entire histories, which would double-count their shared ancestor.

An antichain frontier describes incomplete logical times; it is not a single global “latest version.” Logical trace compaction may discard temporal distinctions only when all relevant consumers/joins release them. Physical merging has a different permission. The [Differential Dataflow trace documentation](https://timelydataflow.github.io/differential-dataflow/chapter_5/chapter_5_3.html) makes this distinction explicit.

A derived view publishes `ViewRoot { recipe, source_roots, completed_frontier, coverage }`. GUI and query clients advance atomically from one valid view root to another. Old results may remain readable while new work proceeds; they are labeled with their actual source root. Progressing one search lane never falsely marks every other lane current.

## 8. Cycles, recursive types, and complete dependency capture

Do not define a semantic node version by recursively hashing the current versions of every neighbor. Cycles make that definition ill-formed, and unrelated neighbor changes can trigger enormous hash cascades.

Store semantic edges as typed **logical references** interpreted under a pinned state root. The edge object hashes its own logical endpoints and evidence. A computation that reads the target records the target facet version in its dependency read set. The state root binds the whole resolution environment.

For recursive type computations that require atomic component identity, materialize an SCC result object with members ordered by stable logical keys, internal symbolic edges, and external input versions. A changed edge can split or merge a large SCC, so maintenance cost can be the affected component or more—not merely the number of edited edges. Cyclic query deletion needs a correct fixed-point/rederivation algorithm; positive support counts alone can retain self-supported cycles.

For finite graph reachability, start with set semantics and seminaive iteration over newly discovered facts, not unbounded path-count bags. Retain sufficient derivation support for an appropriate deletion/rederivation algorithm (for example DRed), or recompute the affected closed scope exactly. Publish exact coverage only after the relevant fixed point completes; a work limit produces an explicitly incomplete result, never a partial set labeled exact.

Every recipe has an input manifest recording positive and negative reads, range/directory membership, toolchain/configuration, generated-source and environment dependencies, and unavailable facts. Pre-discovery request keys select candidates; validating a candidate's manifest is itself budgeted work. Inseparable discovery/compile uses one combined envelope. Identical output versions stop downstream propagation even when an upstream authority had to rerun.

## 9. Local-first placement, as a property of the same graph

Every ready recipe shard can be satisfied by:

1. An already valid local output.
2. A missing local arrangement updated by its retained delta.
3. A valid remote immutable result whose missing objects can arrive quickly.
4. Local recomputation of the necessary scope.
5. Remote delta update/recomputation with a complete input manifest.
6. A bounded race of equivalent pure local and remote work.

The planner estimates **completion time**, not compute time alone. Remote cost includes queueing, RTT, missing input object transfer, worker warmth/capability, execution, missing output transfer, and local result validation. It selects locality and delta-versus-rebuild together. Recent work on [Enzyme's cost-based incremental refresh planning](https://arxiv.org/abs/2603.27775) supports the importance of choosing among refresh strategies rather than assuming an incremental plan always wins; our placement extension is a proposed design for this backend.

Local capability and reserved resources protect the interactive path. A local-capable request does not wait indefinitely for an optional remote reply. If we delay local start while trying a fast remote hit, it has a latest-start deadline derived from reserved local capacity and the interaction budget. That protects an SLO under stated estimates; it cannot promise physical no-regression versus starting local at time zero. Strict baseline protection starts local immediately and lets remote race on separately bounded resources, or leaves remote entirely off that interaction's critical path.

Remote work is especially valuable **before demand**: populate popular package/toolchain/embedding versions, maintain expensive derived arrangements, compact cold history, precompute predicted view ranges, and return changed chunks ahead of user navigation. When the user arrives, the local engine usually reads already-present results. Remote CPU savings are real only when upload/download/validation and local contention are charged.

Offline behavior reads committed local roots, applies local intents, maintains locally supported derivations, and queues replication. Unavailable remote-only capabilities remain explicit. There is no synchronous cloud authority for opening a local package, reading a page, or acknowledging a local edit.

## 10. One edit, traced through the architecture

Assume a workspace with one million declarations, and a user changes documentation on one declaration.

1. The source object transaction commits locally. Source chunking reuses unchanged byte chunks. No remote dependency is introduced for the acknowledgment.
2. The authority adapter reuses its edit-aware session if supported. Otherwise it reprocesses its required source/package scope. This authority cost is reported separately from incremental engine cost.
3. Complete semantic facet comparison finds changed docs, possibly affected source-location/evidence facets, and the enclosing entity manifest. If a prefix edit changes many absolute offsets, those location changes are real; syntax/rope anchors can reduce physical churn only under an explicit source-map contract.
4. The object map path-copies changed Merkle leaves/ancestors. Unchanged objects, type pools, relation partitions, and index blocks are retained.
5. A single consolidated delta updates the docs relation. The type and graph recipes see unchanged subscribed facet versions and do no work. Lexical work occurs only if the changed documentation is indexed. Embedding work is keyed by the actual changed normalized input.
6. The document recipe reuses unchanged signature/member fragments and generates the changed prose fragment. The active view emits a replacement for the stable visible row/fragment, not a whole library refresh.
7. The planner sees an embedding cache miss and delegates that background shard with the changed input object and model recipe. The local document is already visible. A valid result later advances the semantic-search coverage frontier without rewriting the document.
8. Synchronization exchanges root summaries and missing objects. Equal subtrees/chunks are not uploaded. The peer applies the exact base-bound transition or obtains a checked root diff if it missed the base.
9. Background compaction merges physical delta runs under reader/frontier leases. The logical state root and user's history are unchanged.

For a rename, the name arrangement and dependent address/view rows change. For a body edit whose exported semantics are unchanged, semantic output versions stop propagation. For a dependency/configuration or SCC-wide edit, broader recomputation may be necessary. The common engine handles all four cases without pretending every edit is small.

## 11. What “frontier” means here

The novel proposal is the combined architecture: **persistent content-addressed state + differential arrangements + demand-aware work avoidance + data-local remote placement + versioned UI subscriptions**. These are usually separate systems. Here the same immutable objects, typed transitions, input roots, recipe identities, and frontiers cross every boundary.

The concrete benefits to pursue are structural: one changed fact is captured once; unchanged ranges are not rehashed/reserialized/transferred; consumers share indexed state; no-op derivations stop propagation; worker messages carry immutable IDs and missing deltas; memory is reclaimed at actual observation frontiers; physical compaction does not invalidate semantics.

No benchmark result is claimed. The design deliberately changes the architecture enough that meaningful risks remain: metadata size, history/arrangement retention, dynamic query fan-out, authority granularity, remote transfer cost, and canonical chunking. Those are implementation parameters and validation obligations within the chosen design, rather than reasons to defer the core unification.

Read [the physical layout](layout.md) for the exact storage and Rust contracts,
[the semantic ledger](../schemas/semantic-ledger.md) for complete facet and authority
ownership, [the local/remote protocol](local-remote.md) for placement and sync,
and [the migration sequence](../operations/migration.md) for the package graph and
replacement order.
