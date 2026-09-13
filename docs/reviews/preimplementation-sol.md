# Sol adversarial review: v2 versioned object and relation engine

Status: final review of `v2/ARCHITECTURE.md`, `v2/LAYOUT.md`, `v2/LOCAL-REMOTE.md`, `v2/SEMANTICS-AND-LAWS.md`, `v2/STRUCTURE-AND-MIGRATION.md`, the five v2 research reports, and selected source claims in `/Users/mileswirht/Downloads/backend`. The P0/P1 sections preserve findings against the initial draft; **Current disposition** records how the completed plan resolves them. This is a mathematical and architectural review. It does not report production builds, benchmarks, or implementation success.

## Current disposition

The revised architecture and layout documents address P0.1 through P0.8 in the plan. They now define an atomic `WorkspaceRoot` and `WorkspaceDelta`, distinguish authoritative roots from provenance-bound derived `ViewRoot`s, prohibit delta-only committed roots, separate canonical tree policy from physical packing, normalize domain mutation intents into checked before/after deltas, state weighted-relation constraints and join conventions, give ancestry/progress/layout separate types, enumerate facet availability states, and require finite fixed-point/rederivation semantics for cyclic deletions. These are sound plan contracts; their implementations and law tests remain unverified.

`LOCAL-REMOTE.md` addresses P0.9 through P0.11 and the related operational portions of P1. It treats an authorized execution receipt as a trust decision rather than proof that computation was correct, prevents stale attempts from publishing, names the limits of late mismatch detection, charges both hedge attempts and held fallback capacity, separates hard admission from soft prediction, and keeps transient demand outside durable workspace history. It also supplies concrete local durability, cursor reset, replication, cancellation, GC, privacy, source/semantic freshness behavior, and targeted provenance invalidation for compromise revocation while allowing an explicit historical-validity rule for ordinary key rotation.

The layout plan addresses the shared-arrangement, ANN, typestate, buffer ownership, trace compaction, location/pin, and bounded-memory concerns. The semantic ledger assigns every current `SemanticEntity` field, all seven language extension families, occurrence multiplicity, captured-empty/partial/unavailable states, replacement authority, native invalidation scope, and recursive deletion behavior; K0 is correctly required to expand nested type and foreign schemas mechanically before format freeze. The architecture retains the necessary four semantic planes while sharing the object/schema/envelope/recipe/cursor machinery across them; collapsing those planes further would erase different authority, progress, retention, and compaction laws.

The structure plan supplies an acyclic nine-core DAG with language/provider/application leaves, a staged K0–K12 replacement, and an exact 53-row ownership map. I independently expanded the current Cargo workspace globs: the JSON map contains every one of the 53 unique product crates once, contains no extra source crate, and points to existing manifests. Its 9 core + 7 language + 3 optional extension + 5 application target totals 24 packages.

No material internal contradiction remains in the completed v2 plan. The unresolved risks are execution risks: the nested schema ledger must be generated and audited in K0; the algebra, crash, trust, lifetime, concurrency, and compatibility laws must pass; and the measured memory, write-amplification, local-latency, remote-cost, and build-size results must support the chosen physical policies. Those are explicit gates rather than hidden assumptions.

## Addendum: factorized and self-maintaining plans

The later LAYOUT sections 11–12 strengthen the design without creating a second framework. Factorized view DAGs avoid constructing Cartesian products that a downstream aggregate or bounded projection does not observe. Selected higher-order derivative views trade additional retained support for cheaper future updates under the same optimizer and budget model. Reverse dependency arrangements and Merkle read-footprint summaries make invalidation proportional to intersecting readers/ranges and emitted invalidations rather than requiring a scan of every cached result; a hot key covered by many broad ranges still has real fan-out. Finite fused stateless kernels remove queue and materialization boundaries while preserving yields around stateful, authority, resource and publication operations. These are appropriate K5/K6/K11 plan families over the existing objects, batches, arrangements and scheduler.

The canonical-state boundary remains sound: a factorized representation has a recipe/layout-bound derived materialization identity and an explicit expansion/equivalence contract. It is not a canonical flat `StateRoot<R>` until the canonical logical result is actually produced and hashed. Changing factor order or derivative hierarchy can therefore change physical/derived identity without changing an already established logical result root.

The revised text also closes the one identity hazard in this family. `FactorRoot` binds factor schema and key order, support semantics, recipe, exact source roots, completed execution frontier, coverage, child factor roots and payload versions; its retained trace carries separate `Upper` and `Since`. Remote factor deltas name exact base/target factor roots and the input transition/frontier. An incomplete or stale factor therefore cannot be reused merely because its structure and source-root names match. I find no new core contradiction.

## Verdict

V2 has the right level of ambition. A canonical versioned state, one captured transition, shared arrangements, demand-driven materialization, local-first placement, and versioned views can delete major portions of the current root/IR/index/interface lifecycle duplication. This is a coherent end-state direction, not merely a cache added to the existing system.

The initial architecture was not safe to implement literally because it sometimes used one word—root, frontier, delta, proof, or relation—for values with different laws. The revised plan now makes those distinctions explicit. They are the algebra that lets one new engine replace the old systems.

The most compact coherent kernel has four planes over one object substrate:

1. **Authoritative state plane:** typed functional maps and relations selected atomically by one workspace root.
2. **Derived trace plane:** weighted relation changes, arrangements, logical progress, and view roots computed from authoritative roots and recipes.
3. **Demand/control plane:** bounded ephemeral subscriber and scheduler relations; durable only when a product intent explicitly requires it.
4. **Physical layout plane:** logical Merkle nodes, packs, locations, base/delta runs, caches, and compaction under an already established logical root.

All four use the same canonical object IDs, typed schema registry, transition envelope, recipe/input manifests, validation, retention accounting, and cursor transport. This is stronger unification than treating every event as the same `Delta`: each plane has one lawful mapping into the next.

The core equations should be executable specifications:

```text
WorkspaceRoot = H(workspace_schema,
                  sorted(relation_id, RelationRoot),
                  basis_manifests, coverage)

apply(MapDelta { base, target, changes }, state(base)) = state(target)

lower_recipe(delta) = F_recipe(state(target)) - F_recipe(state(base))

trace(base) + lower_recipe(delta) = F_recipe(state(target))

materialize(StateRoot, LayoutRecipe) = LayoutId
decode(LayoutId) = exact logical state named by StateRoot
```

If an implementation cannot demonstrate one of those equalities, it has created a second authority rather than an optimization of the shared engine.

## P0 findings against the initial draft

### P0.1 Add one atomic workspace root above per-relation roots

`StateRoot<R>` is useful for an individual typed map or relation, but a product commit changes several relations together: entity manifest, facet values, containment, edges, source evidence, availability/coverage, shelf selection, and possibly publication metadata. If readers independently select relation roots, they can observe a docs facet from one commit with an entity/type/coverage root from another.

Define a canonical `WorkspaceRoot` (or `StateRoot<WorkspaceSchema>`) whose sorted manifest names every authoritative relation root, schema/recipe authority, and coverage certificate. `CommitId` binds parent commit IDs plus this workspace root and provenance. The canonical global `DeltaId` binds exact base and target workspace roots plus the ordered per-relation map changes. Relation-local deltas remain useful shards inside that envelope.

Derived arrangements and views do not have to join the authoritative workspace root. They publish separate `ViewRoot`s containing recipe, exact source workspace root(s), trace upper/since frontiers, coverage, and result relation roots. This prevents an unavailable vector lane from blocking semantic truth while still preventing a query from mixing source commits.

Tighten the first two identities as well. `ObjectKey<T>` remains stable when the value at that logical key is replaced; it changes only when the domain identity itself changes, such as a true rename/overload identity change. `ObjectVersion<T>` should be the typed canonical content identity of that value, or an exact alias/wrapper around `ObjectId<CanonicalValue<T>>`, rather than a second independently computed digest. A map replacement is normally “same key, new version.”

### P0.2 A delta-only layout cannot create an unmaterialized canonical root

The object-store report says the canonical logical `StateRoot` is always materialized as a prolly tree, but also says an update that exceeds the path-copy budget can retain a delta-only `LayoutId`. This is contradictory if the target root and `DeltaId` already claim the canonical tree result. The engine cannot authenticate an unknown canonical root merely from “base plus patch” unless that patch representation itself is the canonical state-root construction—which would make compaction/layout part of semantic identity again.

Choose one rule:

- authoritative commit completes the canonical tree update and hashes the target root, or rejects/remains pending under a work limit; or
- an explicitly named `ProvisionalState` stores base plus pending mutation but is not a `StateRoot`, cannot satisfy exact reads/remote cache keys, and becomes visible only after canonical materialization.

The first rule is simpler. Logical node creation may be streamed and physical packing deferred, but every child needed by the root hash must exist and validate before the manifest selects it. Background compaction may change `LayoutId` and `PackId`; it cannot finish semantic work that a supposedly committed `StateRoot` skipped.

### P0.3 Make the canonical tree policy semantic and the pack policy physical

A key-anchored prolly root is history independent only under one exact persisted tree schema: key encoding and comparison, domain hashing, cut predicate, min/target/max node constraints, separator rule, internal-level construction, duplicate-key rule, and empty root. Changing any of those changes logical node boundaries and the root hash for equal key/value contents.

That is acceptable if the policy is explicitly a versioned **logical encoding authority**, not ordinary compaction. A schema migration then produces a new relation/workspace root and compatibility transition. Compression, encryption, location, run merging, and pack grouping remain physical layout choices and may change without changing logical roots. `LayoutId` should be a function of `(StateRoot, layout_recipe, run/pack/location manifest)`, never a competing definition of visible state.

The implementation needs a canonical bulk builder and point updater that provably produce byte-identical trees for the same map. Test every mutation permutation, leaf boundary, forced cut, duplicate, deletion, and empty-tree case. Full key bytes decide equality/order; a truncated key hash may accelerate but cannot decide correctness.

### P0.4 Normalize typed mutation intents before canonical deltas

`Put`, `Delete`, `Reparent`, and `PatchColumn` do not form one closed canonical delta algebra. `PatchColumn` depends on a prior schema/layout and is not safely invertible or composable after schema migration. `Reparent` is a domain operation whose actual effect can touch containment, entity/facet manifests, ancestry validation, and derived relations.

Use three stages:

1. `MutationIntent<S>`: ergonomic domain commands such as rename, reparent, patch docs, select publication.
2. `MapDelta<S>`: normalized, exact `(key, before: Option<ObjectVersion>, after: Option<ObjectVersion>)` changes bound to base and target relation/workspace roots.
3. `RelationBatch<R>`: consolidated `-old/+new` weighted rows emitted by a recipe projection.

Only stage 2 defines `DeltaId`, composition, and inversion. Composition is legal only for adjacent roots; for each key it retains the first before and last after and removes an equal net result. Repeated application is acknowledged only when the exact transition is known or current root already equals its exact target under the protocol; a digest alone is not an eternal deduplication database.

For every derived recipe `F`, test the homomorphism `F(target) = F(base) + lower(delta)`. Current full recomputation is the oracle. This one law can replace many bespoke index invalidation tests.

### P0.5 Specify relation constraints in addition to weights

An `i64` weighted multiset is an execution representation, not the schema invariant. At a closed visible frontier:

- functional maps require exactly zero or one value per key;
- set relations require consolidated weights in `{0,1}`;
- bag relations require nonnegative multiplicity;
- unique indexes require one owner per unique key;
- availability/coverage rows obey their own exactly-one state law.

Use checked wide accumulation and fail/rebuild on overflow; never saturate. Consolidate an atomic batch before exposing it so temporary negative or duplicate support is not visible as state. A retraction identifies the exact old row including value/evidence, not merely its lookup key.

The architecture's join formula is correct when `A` and `B` denote old states:

```text
Δ(A ⋈ B) = ΔA ⋈ B + A ⋈ ΔB + ΔA ⋈ ΔB
```

If implementation processes one input after updating its arrangement, use the corresponding two-term sequential convention. Name the convention per operator and test simultaneous changes; mixing them double counts the cross term.

### P0.6 Use different types for ancestry and dataflow progress

V2 verbally distinguishes commit ancestry, dataflow logical time, and layout epoch, but the reports still overload `Frontier` for a set of offline branch heads and an antichain of incomplete dataflow timestamps. Those values cannot be compared or advanced by the same operations.

Use distinct public types:

- `HeadSet`: bounded commit DAG heads used for branch ancestry and synchronization;
- `Epoch`: monotone ordinal assigned to admitted commits on one selected lineage/dataflow instance, with an exact `Epoch -> CommitId -> WorkspaceRoot` binding;
- `Time { epoch, iteration }` initially for differential operators;
- `Upper<Time>`: times at or beyond which updates may still arrive;
- `Since<Time>`: times before which distinctions may be compacted;
- `LayoutEpoch`: physical locator/pack generation only.

Phase (`Parsed`, `Lowered`, `Published`) is operation state or a typed derived dependency, not automatically a timestamp dimension. Worker/partition is routing metadata unless the operator proves it needs a partial-order dimension. Starting with `(Epoch, Iteration)` is sufficient for one atomic input commit and recursive rounds and avoids needless antichain growth.

A result for time `t` is complete only when the input upper frontier is beyond `t` in the formal partial order—equivalently, no frontier element is less than or equal to `t`. Avoid prose such as “strictly greater” unless the timestamp is total.

Offline branches do not enter one trace by sorting hashes or unioning deltas. Resolve a common ancestor, perform the schema-specific three-way merge/conflict decision, commit a new workspace root, then admit that merge commit to the selected lineage's epoch. Weighted replay must not double count common ancestry.

### P0.7 Define a complete facet and coverage ledger

“Every field belongs to a facet” needs an executable schema inventory, not only an entity manifest sketch. Cover image/source/recipe provenance, package and declaration identity, parentage, source file/span, types and type parameters, members/order, documentation, attributes, visibility, links, link occurrences/evidence, externals, every language extension, diagnostics, and authority availability. Some facts are package-, type-, edge-, or source-scoped and should not be forced into an entity manifest.

Each relation declares:

- stable key and canonical value schema;
- owning authority and scope key;
- complete/captured-empty/unavailable/not-requested/partial states;
- cross-relation constraints checked in the same workspace commit;
- recipes that read it, including negative/range reads;
- full-snapshot oracle and replacement semantics.

A scoped replacement may retract omitted rows only with a completeness witness for the exact `(authority, recipe, scope, base root)`. Partial reports add or replace observed facts but cannot infer deletion. A docs facet update changes the enclosing directory/manifest root, but type/graph recipes avoid work only if their dependency read sets name the specific child facets rather than the whole entity manifest.

### P0.8 Make recursive semantics finite and exact

The symbolic-edge SCC design correctly avoids impossible recursive content hashes. Tighten it as follows:

- SCC descriptors contain sorted stable member keys and labeled internal symbolic edges. External dependencies name stable keys plus the exact external facet/version inputs read by the SCC recipe.
- SCC objects are derived results keyed by graph source root and SCC recipe, not stable entity identities. Split/merge legitimately changes SCC IDs.
- Dynamic insertion/deletion may affect a much larger region than edited edges. Define an exact fallback: recompute the affected weakly connected/authority scope or the full graph under a budget and compare with a full Tarjan/Kosaraju oracle.
- Reachability operates on finite distinct `(source,target)` support, not unrestricted path counts, which are infinite in cycles. Use seminaive/differential iteration with a distinct threshold and a closed iteration frontier.
- Negative edges require differential retraction with retained provenance/support or DRed-style rederivation. Positive support counts alone permit self-supporting cycles after deletion.

If iteration/work limits are exceeded, publish `Incomplete/Unavailable` coverage at the prior exact view root. Never label a partial fixed point exact.

### P0.9 A hash is not proof of remote computation

Content hashes prove that received bytes match an ID. A Merkle path proves membership or absence under an already trusted root. Neither proves that a remote worker executed the requested recipe correctly to discover a new output.

Define remote result authority classes:

- **locally verifiable:** proof/certificate can be checked more cheaply than recomputation;
- **trusted executor:** an authorized worker signature binds `ComputeKey`, full input manifest/root, recipe/capability version, attempt/fence, output IDs, coverage, and terminal class;
- **replicated/audited:** quorum, redundant execution, or sampling supplies the product's confidence rule;
- **untrusted memo:** bytes may be stored quarantined but cannot advance a view root.

Structural schema validation and content hashes are mandatory in every class. Transport authentication merely identifies the peer. Name the trust root, key rotation/revocation rule, and whether results remain reusable after worker authorization changes.

The hedge rule also needs trust ordering. A low-trust remote mismatch must not veto an independently valid trusted local result and create a denial of service. Select the first result that satisfies the requested authority policy. If two equally authoritative deterministic executions disagree, quarantine both outputs for that recipe/input, preserve the previous valid view, and surface an integrity fault. If authorities differ, retain the higher-authority valid result and fault/quarantine the other according to policy.

### P0.10 Make latency reservations honest

The placement policy correctly admits that p95 estimates cannot guarantee no regression. Its resource mechanics still need exact rules:

- hard memory/output/process/file/network limits use reservations; latency remains a probabilistic estimate;
- a local fallback reservation held during remote delay has a deadline, priority, reclaim rule, and charged opportunity cost, otherwise it can idle scarce capacity and create head-of-line blocking;
- a hedge reserves both attempts' live inputs, scratch, outputs, terminal capacity, network buffers, and loser cleanup, while shared immutable input bytes are charged physically once;
- closure metadata can be inspected before placement, but do not hydrate and pin the entire local closure merely to decide remote placement;
- remote-only work does not consume local execution slots; a prefetch node does not hold a compute permit during network wait;
- winner selection does not release loser resources until cancellation/terminal ownership is settled, and a late stale result cannot publish.

Use admission feasibility first, then minimize predicted completion time among feasible candidates. Cost snapshots have expiry/confidence and fall back to conservative policy when stale. Reserve an immediate local primary for strict interaction protection; delayed local fallback is a distinct policy with a measurable opportunity cost.

### P0.11 Demand is a shared relation, but usually not durable product state

`Demand(consumer, recipe, key/range, freshness, priority)` is an excellent unifying control relation. It should live in the engine's ephemeral control namespace with session-scoped logical time, bounded leases, and no inclusion in authoritative `WorkspaceRoot`, commit ancestry, offline replication, or durable delta log. Otherwise scrolling a page or opening a query creates permanent product history and pins arrangements after a crashed client.

Explicit user intents—saved views, subscriptions, requested package additions, settings—are durable authoritative objects. Runtime interest derived from a visible window, active RPC, prefetch prediction, or waiter is ephemeral demand. A durable intent recipe may generate ephemeral demand after restart. Lease expiry removes demand but is never used to retract committed semantic state.

## P1 findings against the initial draft

### P1.1 Separate authoritative, derived, and control logs without duplicating mechanics

One envelope/codec/replay mechanism can back multiple logs, but one globally serialized journal for source edits, compiler publication, every arrangement update, UI hover, and remote attempt would become a contention and recovery bottleneck. Use authority-class namespaces:

- authoritative workspace commits select `WorkspaceRoot`;
- derived view commits select recipe/source-root-bound `ViewRoot`s;
- scheduler attempts and ephemeral demand use bounded in-memory/event traces, persisting only recoverable jobs/results;
- physical layout manifests select locations for already known logical roots.

They share record format, IDs, checksums, cursors, and recovery library. Only the authoritative manifest decides product truth. This deletes duplicated infrastructure without confusing retention or durability.

### P1.2 Specify cursor gap and compaction semantics

`DeltaId` deduplicates a known transition but does not tell a subscriber whether it missed another transition. A durable cursor must bind log/branch identity, next sequence or transition-chain digest, observed head/root, and supported schema. On a gap, pruned history, discarded branch, or incompatible schema, return a typed reset containing a snapshot/view root; never continue from a scalar sequence attached to a different head set.

Maintain trace `upper` and `since` separately. Consumer capabilities hold logical compaction leases; physical run merging can occur without advancing `since`. A slow/crashed subscriber has a bounded retention policy and eventually receives `ResetRequired`, rather than pinning all temporal distinctions forever. Deletion ledgers and remote replica watermarks cannot rely on TTL alone.

### P1.3 Give arrangements one canonical batch/spine, not one universal index shape

The shared arrangement kernel should own sorted consolidated batches, key/value/time/weight cursors, immutable runs, merge/compaction, statistics, retention, and checkpoint validation. Typed recipes choose keys, values, constraints, and operators. Exact lookup may use prolly/B-tree range structure; prefix lookup may add an ART-like transient accelerator; graph keeps forward/reverse arrangements and CSR-like sealed blocks; ANN retains its explicit base/delta/consolidation barrier.

An accelerator never becomes a second root authority. Tantivy/Trustfall/Qdrant can be deleted or retained as optional providers only after differential tests show the shared arrangements reproduce required membership, ordering, evidence, coverage, and performance. ANN approximate quality remains recipe/version/recall metadata, not a weighted relational exactness claim.

### P1.4 Keep view publication atomic and UI deltas stable

A `ViewRoot` should contain stable row IDs and order keys plus exact source roots, recipe, upper frontier, coverage, and continuation state. Build it privately while deltas arrive; publish atomically only at the requested complete frontier. The GUI may display an older root plus explicit progress, but it must not fold rows from incomparable roots into one claimed snapshot.

Stable row identity is independent of rank/position. Reordering changes order keys and produces moves/replacements without losing selection. Avoid assigning every row a dense absolute position that causes O(N) retractions on an insertion; use stable fractional/order-tree keys internally and materialize bounded window positions at the view boundary.

### P1.5 Define object closure, locations, and GC as separate proofs

Before selecting a logical root, validate that its complete required object closure is durably reachable or explicitly remote-only under the product contract. Location hints are not proofs. Physical location updates need an atomic layout manifest and reader pins so pack rewrite/delete cannot race a cursor.

GC roots include retained authoritative commits/head sets, view roots promised by cursors, reconciliation ancestors, active compute/transfer leases, and selected layout manifests. Mark against a stable root revision, quarantine across at least one recheck/epoch, then delete only unpinned physical extents. Pruning commit ancestry needs a checkpoint/summary that preserves branch reconciliation policy; otherwise “find common ancestor” stops being available.

### P1.6 Bound metadata and write amplification as first-class outcomes

Complete facets, Merkle nodes, delta batches, arrangement runs, dependency read sets, view roots, and signatures may turn a one-field edit into substantial metadata even when payload reuse is excellent. Every benchmark should report:

- logical changed payload bytes;
- canonical logical node bytes and count;
- transition/read-set metadata;
- arrangement input/output bytes;
- physical pack bytes and compaction debt;
- retained history/frontier bytes;
- remote proof/signature/transfer bytes;
- peak live and pinned bytes.

The target is not “one edit is O(log N)” universally. It is cost proportional to changed canonical boundary regions, recipe fan-out, and emitted output, with explicit O(N) fallbacks for chunk ripple, global statistics, SCC changes, and non-incremental authorities.

## Package graph and stronger condensation

The completed structure plan resolves the research report's ten-core arithmetic into the accepted nine-core graph:

```text
version
├── store
│   ├── flow
│   │   ├── execution (also replication)
│   │   ├── semantic
│   │   │   ├── compile
│   │   │   └── library
│   │   └─────────────┘
│   └── replication
└── engine (the sole core composition crate, over all lower cores)
```

This diagram is schematic; the authoritative dependency table in `STRUCTURE-AND-MIGRATION.md` gives every allowed direct edge. The boundaries are justified: `version` owns canonical meaning; `store` owns durable logical/physical storage; `flow` owns weighted traces and arrangements; `replication` owns bounded object/envelope transport; `execution` owns placement and resource attempts; `semantic` owns language-independent fact schemas and recipes; `compile` owns authority/discovery/extraction contracts; `library` owns portable product intents, queries, views, and DTOs; `engine` alone composes concrete services. Merging any of those pairs would save a manifest but blur a distinct effect or dependency direction.

The seven language crates are leaves implementing `compile` contracts; `compile` never imports them. The Tantivy, Trustfall, and Qdrant extensions remain optional leaves under the common input-root/coverage/recipe contracts. Desktop, CLI, MCP, `locald`, and worker are five application binaries. Core crates never import application types, and provider/native selection occurs only at composition.

I independently expanded the workspace's Cargo member globs and compared their package names to `crate-map.json`: there are 53 unique current packages, 53 unique mapping rows, no missing or extra package, and every recorded manifest path exists. The end state has exactly 9 core + 7 language + 3 extension + 5 application packages = 24. The K0–K12 sequence starts with version/store laws and an end-to-end local product slice, then crosses authority, shared arrangements, planning, replication, remote execution, vector/recursion, native sessions, layout optimization, and final contraction. That is a replacement sequence rather than a folder-only reorganization.

One wording detail found during final review must remain precise: physical packs/materializations implement storage for canonical objects and batches, while trace, view, and transport cursors belong to their respective progress/observation protocols. A cursor is not itself an implementation of the canonical object/batch artifact. Optional provider leaves may depend on the lower `execution` contract where they implement a placement/provider capability; that edge remains acyclic because only `engine` registers the leaf.

## Verified source evidence

The main source premises are accurate:

- `heart/root/diff.rs:159-181` streams a canonical O(old+new) merge; `changed_diff` filters unchanged results but still visits them.
- `compiler/ir/vcs.rs:15-40,82-149` uses generation-bound borrowed IR snapshots and stable ordered entity merging. `retained_change` at `:152-176` compares only variant, core payload, and stable parent.
- `CorePayloadHash::COVERAGE` in `compiler/ir/semantic.rs:2326-2373` explicitly excludes documentation, visibility, language extensions, source provenance, occurrences, and opaque parentage. V2 correctly refuses to use it as a complete facet version.
- `compiler/ir/semantic.rs:6145-6177` already provides canonical links plus forward/reverse CSR-like adjacency and occurrence offsets. V2 should reuse these as batch kernels/oracles rather than recreating adjacency logic first.
- The immediate semantic publication/reopen path and single application compiler owner found in the first audit remain real seams for migration, but they do not themselves prove the v2 engine's performance.

One evidence wording should be corrected: current `GenerationRoot::diff` is explicitly O(old+new) descriptor work, but calling it “O(1) extra memory” depends on counting iterator state only and excluding the already owned roots. That is reasonable, but the compatibility claim should say “O(1) iterator scratch over the two retained roots.”

## Concrete deletion opportunities

V2 earns its complexity only if it deletes these mechanisms after parity:

- full flat-root rebuild/diff as the normal change detector, retaining it as import/audit oracle;
- whole semantic image as the only reusable ownership boundary, retaining a compatibility export;
- separate exact/lexical/graph/document shadow and invalidation stores where arrangements cover the same semantics;
- interface epoch file as a second authority;
- GUI shelf/page/search caches that mirror authoritative state rather than pinning view roots;
- protocol-specific command metadata duplicated across CLI and MCP;
- compiler/application dependence on interface types;
- hydration closure machinery that independently rediscovers object presence already proven by tree traversal;
- per-consumer before/after diffs after the owner has emitted a complete transition;
- mutable “latest” cache entries unbound to source root, recipe, and coverage.

Do not promise deletion of specialized ANN, native compiler authority state, canonical wire compatibility, source grammar validation, or transport framing. They become typed leaves over the engine because their laws remain distinct.

## Required proof and benchmark program

The checked-in Python prototype now supplies a useful first slice: canonical key-anchored maps with a real minimum/maximum policy and canonical empty root, immutable supported values, exact-base delta apply/composition/inversion, an atomic relation-only workspace root, signed old-state join terms, one-dimensional trace compaction, physical pack round trips, and honest broad-fallback traversal accounting. Its saved run reports 12 passing tests. The model deliberately rebuilds maps in O(N), accepts only a small JSON-like value schema, omits production workspace schema/basis/coverage headers, and does not model crash durability, full antichains, SCC deletion, remote trust, concurrency, or performance. It validates equations in isolation; it does not prove the implementation or the remaining laws.

Before any durable format is frozen:

1. Build a small reference interpreter for authoritative maps, workspace root construction, normalized delta application/composition/inversion, branch merge, and facet constraints.
2. Build a canonical prolly bulk builder and point updater; prove byte/root equality across mutation order and compaction/layout changes.
3. For each recipe, compare full recomputation with weighted incremental application under random inserts, deletes, replacements, simultaneous join changes, overflow attempts, partial coverage, and replay permutations.
4. Model commit crash points: object write/sync, delta append/sync, root manifest swap, notification, derived view publication, layout swap, and remote receipt. Reopen sees the old exact root or the new exact root, never a hybrid.
5. Test commit DAG divergence/common ancestry/three-way merge, duplicate delivery, cursor gaps, history pruning/reset, and schema migration.
6. Compare SCC/reachability incremental results to a full finite-graph oracle under self loops, mutually recursive components, split/merge, edge deletion, and bounded-work fallback.
7. Test remote result trust, revoked workers, mismatched deterministic outputs, stale fences, late losers, missing Merkle nodes, invalid membership/absence paths, and low-trust denial attempts.
8. Simulate reservations under local saturation, remote saturation, stale estimates, fallback hoarding, hedge storms, output growth, cancellation, and slow subscribers. Prove hard resource conservation; measure latency distributions.
9. Validate one all-facet edit matrix. Mutate each semantic/source/authority facet independently and assert the exact authoritative and derived relations that change or remain identical.
10. Run the complete offline product trace: local intent, branch commit, compiler replacement scope, derived views, cursor/UI observation, sync divergence, explicit reconciliation, restart, and physical compaction with unchanged logical roots.

Measure at least payload and metadata write amplification, tree nodes touched, hashing, arrangement work, trace/frontier retention, compaction debt, cache/pin memory, remote bytes/validation, p50/p95/p99 interaction latency, and exact/approximate result quality. Compare against current full rebuild/reopen and current specialized index behavior. These measurements tune the committed architecture; they do not decide whether its central version/delta engine exists.

## Final assessment

The v2 design can deliver the requested unification if it makes one atomic workspace root the semantic authority, treats map deltas and weighted relation changes as connected algebras, keeps ancestry distinct from logical progress, and makes physical layouts prove an already known root. Demand and remote execution then become native engine capabilities without becoming semantic authorities.

The completed plan now makes the original P0 contracts explicit across its architecture, layout, local/remote, semantic-law, and structure/migration documents. I find no remaining material contradiction and approve it as the end-state direction. Durable format freeze still depends on completing the K0 schema inventory and the law/crash/compatibility gates above. Runtime performance remains an empirical question, but the deletion and ownership gains are concrete enough to justify the migration.

## Agent execution and cutover review

The agent plan has the right unifying shape. A cell is a versioned recipe/input assignment, a candidate is a Git-tree result, evaluator runs are evidence relations, and Sol advances one integration ref after an independent decision. Those are control-domain schemas over the existing Nix/Nu policy compiler and, after K1, the same v2 object/relation store. They should not become a second scheduler, object store, hash family, or Rust orchestration service. `PlanRoot`, `ContractRoot`, `EvidenceRoot`, `EvaluatorRoot`, `PolicyRoot`, and run/decision roots should be domain-separated specializations of the common canonical object/state machinery; Git `IntegrationHead` and `CandidateTreeRoot` correctly remain distinct from product `CommitId`, `StateRoot`, and `WorkspaceRoot`. The revised `CellDescriptor` also correctly excludes its own `CellId` from the hashed bytes.

The authoritative four roles match the live control plane: Luna receives only opaque `test` feedback; Terra academic can inspect, write, format, lint, run affected proof, and commit but cannot measure, run workspace closure, or merge; Terra reviewer can independently inspect, semantically lint, run affected proof, and measure but cannot edit, promote baselines, close the workspace, or merge; Sol can integrate and close the workspace but cannot own measurements. The older research report's “five-role” wording is superseded by these four enforced actor roles; store, language, query, and other subjects are work lanes. The document now correctly says the Nix PATH and contract digest enforce command custody rather than process containment, and requires an external path sandbox plus an integration-ref compare-and-swap.

### Required corrections before control-plane cutover

1. **Separate implementation identity from evaluation campaigns.** A hidden holdout commitment is currently inside `CellDescriptor`, so rotating only a holdout seed creates a new `CellId` and makes an unchanged candidate look like new implementation work. Keep the production cell keyed by plan/contracts/scoped sources/parent/path/capability/acceptance inputs. Define an `EvaluationKey` over `(CellId, CandidateTreeRoot, evaluator root, holdout commitment, toolchain/environment root)`. A changed holdout invalidates its decisions and runs, while the candidate tree remains available for independent reevaluation. `AcceptanceContract` should own or reference the required `TestContract`s rather than duplicate their oracle, coverage, fault, and budget fields.

2. **Make evidence authority-enforced, not merely hashed.** A builder may propose a candidate and its own public-test receipt, but cannot admit reviewer evidence or a verified decision. Evaluator and reviewer runners must write immutable receipts into storage outside the candidate's write domain, with authority/fence identity and complete stdout/stderr/config/input digests; `Decision` references those roots. A digest in candidate-writable `.local` storage detects later byte changes only when a trusted party already holds the digest. Before K1, the JSON evidence tree therefore needs exclusive writer custody or an append broker and an independently retained manifest. After K1, use the same authority-qualified admission and revocation rules as other v2 results.

3. **Repair current policy coverage during bootstrap.** Direct comparison against `.config/nix/control.nix` found 17 current product crates unmatched by any Koji commit scope while `allow_empty_scope = false`: nine heart crates (`identity`, `hydration`, `object-pack`, `frame`, `schema`, `object`, `view`, `root`, `adaptive`), six interface crates (`core`, `identity`, `library`, `protocol`, `search`, `documents`), and `server-journal`/`server-operation`. Thus the whole current configuration is not yet “sound but concentrated”; its role/capability core is strong, while commit-scope coverage is incomplete. Bootstrap must cover those current paths as well as the proposed `crates/`, `frontends/`, `extensions/`, `apps/`, `.config/contracts/agent`, `.config/nu/cutover`, and control-plane fixture paths before K0 candidates depend on the commit command. Add a mechanical fixture requiring every workspace package and policy path to match exactly one intended scope.

4. **Name the hidden-evaluator authority.** Terra reviewer may use and reveal a committed holdout but the live role is forbidden to edit candidates or promote baselines; Terra academic both authors candidates and cannot be the hidden oracle custodian. A human/card authority or dedicated non-agent evaluator service must create the salted commitment, retain the secret nonce/corpus, authorize controlled reveal, and rotate a revealed corpus. The commitment and threshold policy need version, expiry, access log, backup, and compromise revocation. This preserves the four roles without inventing a fifth agent role.

5. **Prove rollback per durable intent and effect.** The new stable logical-intent journal is the right bridge, but “both writers understand it” must be an executable compatibility matrix. Before writer promotion, every acknowledged intent is either round-trippable through the old and new schemas, explicitly one-way with rollback disabled for that scope, or rejected before acknowledgment. Code/read rollback must retain the current data head or forward-replay all acknowledged intents; selecting an older root is allowed only at the declared data-loss RPO. External exactly-once behavior requires sink-side idempotency or reconciliation of the effect-before-receipt crash window, as the revised plan now states. Irreversible effects need a separate canary/compensation rule and cannot be rolled back by moving a head pointer.

These corrections do not weaken the migration or add another framework. They reduce it to three existing mechanisms: v2 typed objects/relations for facts, the Nix/Nu policy compiler for role capabilities, and Git plus compare-and-swap for candidate integration.

### Implementation gates

- Re-run the environment census at bootstrap. I verified the current snapshot's Rust 1.97.1 pin, nextest 0.9.138, zero retries/flaky-as-failure policy, `test changed` library-only filter, 106 registered worktrees, 81 prunable entries, 84 branch-backed entries, 22 detached entries, and dirty Downloads checkout. These are dated observations, not permanent constants. No pruning may occur from the census alone.
- Define canonical encodings for cell, evaluation, run, evidence, decision, supersession, and cursor records once and generate Nu/Rust/schema bindings. Prove history-independent IDs, unknown-field/version rejection, exact authority, and lossless JSON-to-v2-store import; retain the bootstrap export until its audited compatibility deadline.
- Make leases recoverable: one exclusive path writer, one in-flight owner per `AgentWorkKey`, bounded waiters, owner epoch/fence, crash expiry, cancellation, stale-result quarantine, and compare-and-swap integration. Coalescing equivalent cells must share work without giving multiple coordinators write custody.
- Mutate the evaluators deliberately and demonstrate rejection of missing retractions, stale facets, early frontiers, wrong authority, duplicate effects, skipped targets, forged evidence, and out-of-scope writes. A green candidate requires independent full-recompute/native/metamorphic evidence; model labels alone establish no independence.
- Run the proposed prepared-semantic-image pilot before K1. The repeated-plan premise is accurate: application length measurement, publication length measurement, and encoding currently rebuild the full plan. Require exact length/byte parity, immutable-image lifetime binding, stale-plan rejection, retained-memory accounting, and reviewer-owned measurements. Treat a failure to simplify this bounded seam as a control-process defect before scaling agent count.
- Exercise writer transition and rollback under concurrent clients, queued intents, crash points, old/new schema readers, cursor gaps, GC pins, native-process death, and every supported external effect. Shadow equality is checked only at coherent closed frontiers with the same coverage; zero mismatches on a finite campaign is evidence for that versioned workload, not proof of universal absence of regressions.
- Bind every performance claim to immutable workload, machine/toolchain, warm state, repetitions, statistic, coverage, and result roots. Hidden holdouts supplement public regressions; they cannot justify an unconditional “no regressions” claim. Network independence and local fallback gates apply to locally promised operations, while explicitly remote-only capabilities retain their typed unavailable/required behavior.
