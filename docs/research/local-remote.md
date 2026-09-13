> **Research, not the final specification:** use the [final v2 index](../architecture/README.md). These reports preserve alternatives; final v2 equations, package boundaries and corrections take precedence.

# Latency-first local/remote placement for the versioned compute graph

Repository inspected: `/Users/mileswirht/Downloads/backend` (read-only, 2026-09-07). This is a design proposal; no production code, builds, or broad tests were run. It makes no absolute latency guarantee. It defines reservations and fallback behavior so a local-capable interaction never depends on remote availability.

## Direction

Use one differential compute graph whose nodes consume immutable versioned inputs and produce immutable versioned results. Each node has a local execution candidate and, when authority and trust permit, remote candidates. Placement is a scheduler decision attached to a graph node, not a second platform architecture.

Local execution owns the user-visible critical path whenever the required inputs and capability are locally verified and the local reservation succeeds. Remote work can warm caches, prefetch missing immutable chunks, execute remote-only nodes, or serve as a bounded hedge after a local p95 deadline. A missing remote result never blocks a local-capable interaction. If the node cannot execute locally, the UI receives an explicit `RemoteOnly` or `UnavailableOffline` label rather than an apparently local result.

The latency policy is reserve based: reserve local CPU/memory/scratch and the complete dependency closure first; only then start a remote attempt as optional work or as an explicitly remote-only fallback. A remote hedge has its own contention and egress budget, cancellation/fencing token, and result authority check. It cannot consume the reservation needed by the local primary.

## Existing capabilities to compose

`heart-adaptive` already provides a pure local-first placement vocabulary. `Pin` couples immutable `GenerationId` and `IndexSnapshotId` (`heart/adaptive/lib.rs:28-44`); local and remote facts carry that pin and exact bytes (`lib.rs:67-87`); demand records a latency budget (`lib.rs:89-109`); remote health includes observed pin and measured latency (`lib.rs:173-190`). Its policy is bounded, deterministic, and stateless (`lib.rs:583-592`).

The current policy is useful groundwork but is a storage-contraction planner, not the complete interaction scheduler. It prioritizes preservation, RAM contraction, bundle acquisition, remote recovery, healthy fetch, NVMe eviction, and bundle release (`heart/adaptive/lib.rs:597-659`). Fetch requires matching remote authority, a hot demand, acceptable measured latency, and RAM/operation budget (`lib.rs:923-971`). Recovery consumes operation and retry budgets (`lib.rs:896-921`). Extend this vocabulary with execution candidates, p50/p95/p99 estimates, network queue/upload costs, dependency closure, freshness, and explicit local/remote-only outcome labels rather than replacing the pure policy.

`heart-root` gives the graph its immutable inputs. `GenerationRoot::diff` streams two canonical roots in O(old + new) descriptor work and O(1) extra memory (`heart/root/diff.rs:43-58`, `159-181`); `ValidatedRoot` borrows exact canonical bytes and a generation identity (`heart/root/root_view.rs:22-64`). `GenerationView` composes root and locality with generation/count checks (`heart/root/locality/view.rs:287-310`). These are the right inputs for incremental scheduling: changed objects become invalidation seeds, unchanged objects keep their result identity and can be reused.

The existing routing plane already carries an immutable snapshot, query digest, route identity, segment/range assignment, worker identity, and attempt ordinal (`server/index/routing/worker.rs:31-64`). `Coordinator::plan` produces snapshot/query-bound assignments, and retries are bounded by `RetryPolicy` (`server/index/routing/planning.rs:51-91`, `191-242`). Reply merge validates route, snapshot, query, assignment, range, and segment before accepting rows (`server/index/routing/merge.rs:17-18`, `429-474`). Preserve this as the remote candidate protocol foundation. A remote result must pass the same authority validation as a worker reply.

`server-runtime` supplies physical work permits, byte budgets, ABA-safe handles, cancellation, one-owner execution, terminal retention, and register/arm/recheck waiters (`server/runtime/admission.rs:110-135`; `server/runtime/owner.rs:244-335`; `server/runtime/async_admission.rs:113-135`). Use it for local scheduler reservations or adapt its contracts. `compiler/application/runtime.rs` currently uses a separate one-slot command channel and an `active` bit (`compiler/application/runtime.rs:438-473`, `550-667`), so integrating this scheduler into compiler runtime is future work rather than an existing critical path.

## Unified graph model

Represent every computation as a node keyed by immutable input identities, capability identity, algorithm/version, and requested output kind:

```rust
pub struct ComputeKey {
    pub graph: GraphId,
    pub node: NodeId,
    pub inputs: SmallVec<[ObjectVersion; 4]>,
    pub capability: CapabilityId,
    pub algorithm: AlgorithmVersion,
    pub output: OutputKind,
}

pub struct ObjectVersion {
    pub pin: Pin,
    pub object: ObjectId,
    pub schema: SchemaVersion,
}

pub enum PlacementClass {
    LocalPreferred,       // complete local path is allowed and reserved first
    RemoteOptional,       // background warm/prefetch or hedge
    RemoteRequired,       // no verified local implementation/input closure
}

pub struct ImmutableResult {
    pub key: ComputeKey,
    pub input_digest: Digest,
    pub result: ObjectId,
    pub authority: ResultAuthority,
    pub produced_at_version: Version,
}
```

`ComputeKey` is the deduplication/fencing identity. Results are content addressed and immutable; a late remote result can be safely discarded or retained as a cache entry only if its key and input digest match. A new generation or algorithm version creates a new key. Deltas reference parent versions and changed object identities; they never mutate an already published result.

The graph maintains one node record per `ComputeKey` with states such as `MissingInputs`, `ReadyLocal`, `LocalRunning`, `RemoteWarming`, `Hedged`, `Committed`, `Stale`, `FailedRetryable`, and `FailedPermanent`. State transitions are event records over immutable facts, not mutable “latest result” guesses. A materialized index may be mutable, but replaying the event/delta history must produce the same node state.

## Placement protocol

### Admission and reserve order

For a latency-critical local-preferred node:

1. Pin the graph generation, index snapshot, schema, capability manifest, and input closure.
2. Resolve the closure from local RAM/NVMe. If a chunk is absent, consult local durable object storage before remote.
3. Reserve local work slots, RAM/scratch bytes, and output bytes as one `LocalReservation`. The reserve includes all known dependencies, not just the root node.
4. If the local closure is complete, dispatch local work immediately. Record `LocalPrimary` and its reservation deadline.
5. Optionally reserve a separate hedge token and remote egress/CPU budget. Do not borrow local reservation credits for it.
6. Start remote only if the primary exceeds its adaptive hedge deadline, local capacity is unavailable, or the node is remote-only. A local-capable node can return a local result without waiting for remote.
7. Commit the first authority-valid result through a single winner CAS; release loser reservations and cancel loser transport where possible.

For remote-only work, surface `RemoteRequired { reason, expected_freshness }` before dispatch, show progressive state, and retain the request in an offline branch if connectivity fails. Never silently route a local-capable node into this class merely because the remote estimate is lower.

The protocol prevents nested-budget deadlock by acquiring in this order: graph-node reservation, local closure reservation, optional hedge reservation, transport reservation, then execution. A remote prefetch must never hold a graph-node execution permit while waiting for network bytes. If a remote chunk is needed by local execution, represent it as a separate prefetch node and let the UI observe `WaitingForLocalInput`; this avoids a local permit being held across an unbounded network wait.

### Cost model

Keep estimates in a short-lived `CostSnapshot` keyed by `(node kind, input bytes, capability version, locality class, device/network profile)`:

```rust
pub struct CostEstimate {
    pub local: Distribution,  // p50/p95/p99 + queue delay
    pub remote: Distribution, // network RTT + remote queue + service time
    pub upload: ByteCount,
    pub missing_local: ByteCount,
    pub confidence: Confidence,
    pub sampled_at: MonotonicTime,
}
```

Local estimate = local queue delay + missing chunk read/decode + compute + materialization. Remote estimate = upload of missing inputs + client/network queue + RTT + remote queue + service + result download/validation. Include p95 network queue, not just RTT. Missing chunk bytes must be counted once across a dependency closure; shared chunks become graph edges with one fetch reservation.

The scheduler chooses local when `local.p95 <= interaction_budget` and the local reserve is available. If local p95 misses the budget but local p50 is healthy, keep local primary and arm a hedge at `max(local.p95, min_hedge_delay)` subject to hedge contention budget. If local reserve cannot be acquired within a small admission window, start remote only when policy marks it optional and remote reserve exists; the UI remains responsive with a partial/progressive result. These are policy thresholds, not guarantees.

### Hedged pure work

Hedging is valid only for pure work over immutable inputs and deterministic output identity. The primary and hedge carry the same `ComputeKey`, input digest, algorithm version, and authority manifest. Their attempts differ by `AttemptOrdinal` and placement.

The primary is local. The hedge starts after an adaptive deadline derived from the node's recent p95, with a lower priority and a strict global hedge fraction/CPU/egress budget. A result winner is selected by authority-valid completion, not arrival alone. On first valid result, issue cancellation/fencing to the loser and release its reservation. If both finish, identical content is one result; a digest mismatch is a typed integrity failure and neither result becomes visible.

Dean and Barroso's *The Tail at Scale* describes hedged requests as a way to reduce latency variability by sending a secondary request after a delay and cancelling outstanding work when one result arrives; it also reports that a p95-triggered hedge can substantially reduce tail latency while adding modest load when applied selectively. The paper explicitly warns about duplicate work and presents tied requests as a way to close that window. See the [research paper](https://barroso.org/publications/TheTailAtScale.pdf) and [Google publication record](https://research.google/pubs/the-tail-at-scale/). Apply the result only to pure immutable nodes; never hedge a side effect or a durable commit.

### Leases, idempotence, fencing

Every attempt receives:

```rust
pub struct AttemptLease {
    pub compute: ComputeKey,
    pub attempt: AttemptOrdinal,
    pub owner: WorkerId,
    pub input_digest: Digest,
    pub authority: AuthorityManifestDigest,
    pub fence: FenceToken,
    pub expires_at: MonotonicTime,
}
```

The local scheduler owns the canonical lease epoch. A remote worker may execute only with a manifest signed by an accepted authority and a lease whose fence token is current. Commit accepts `(ComputeKey, input_digest, fence)` exactly once; a stale or expired attempt can return a cacheable result but cannot publish it. This is the same role played by route/attempt validation in `server/index/routing/merge.rs:429-474`, strengthened with an execution fence.

Lease expiry must not imply deletion. Keep a remote GC pin for every input/result object referenced by a live lease, plus a grace interval for in-flight cancellation and retry. GC may reclaim only objects with no local retention, no remote pin, no published generation reference, and no active lease. A pin is version-specific: pinning generation G does not pin G+1 unless the delta explicitly references it.

## Versioned sync and offline branches

Use an append-only signed delta stream. Each delta contains parent version(s), changed object IDs, operation/schema version, author/device identity, signature, and a causal/vector summary. The local device may create a branch while offline; branch results are valid under that branch pin and are never relabeled as the canonical remote head until reconciliation succeeds.

Sync exchanges version summaries and missing chunk/object identities first, then transfers only deltas/chunks absent from the receiver. `heart/root/diff.rs:84-137` is a useful linear merge model for root deltas; preserve O(1) cursor memory and emit changed-only work. For convergent data, use a CRDT or a domain-specific merge whose invariant is explicit. CRDT research establishes that replicas can accept local operations without remote synchronization when operations commute, while delta-state synchronization reduces the cost of shipping full state. See [Shapiro et al., Conflict-free Replicated Data Types](https://arxiv.org/abs/0907.0929) and [Gomes et al., Efficient Synchronization of State-based CRDTs](https://arxiv.org/abs/1803.02750).

For non-commutative authority decisions, retain a server/authority arbitration node. The local branch remains readable and editable, with a UI state of `DivergedPendingSync` or `ConflictNeedsReview`; do not apply last-writer-wins to security, publication, or capability authority. The local-first paper frames client-side storage as primary and servers as secondary relays while emphasizing data ownership and offline operation; see the [accepted paper](https://www.repository.cam.ac.uk/items/525725e6-1a1d-46ce-a5a3-d0d9b1beeee1).

## Authority manifests and trust

An authority manifest is an immutable, signed object naming the generation/snapshot, schema and algorithm versions, capability bundle IDs, acceptable worker identities, input digest rules, and result validation rules. Local manifests are validated before a capability is activated; remote manifests are checked against a trust root and requested `Pin`. A healthy remote under a different pin is `Inconsistent`, matching the existing adaptive vocabulary (`heart/adaptive/lib.rs:173-190`), and cannot satisfy the node.

Remote execution should return the result object plus manifest digest, input digest, algorithm version, worker identity, attempt ordinal, and proof/checksum. The local client validates all fields before winner CAS. Transport authentication is necessary but insufficient: authority and content identity are semantic checks.

## Failure partitions and UI behavior

Partition failures into four user-visible classes:

* `LocalReady`: local inputs/capability verified; execute now.
* `LocalBusyRemoteOptional`: local reserve is temporarily full; show progressive local queue, optionally hedge under budget.
* `RemoteOnly`: no verified local capability or input closure; remote is required and freshness/trust are stated.
* `OfflineUnavailable`: required remote authority is unreachable or inconsistent; preserve the request/branch and offer cached prior version if freshness permits.

Remote outage, stale authority, missing chunks, lease expiry, digest mismatch, and capability rejection remain distinct telemetry/failure causes. A stale result may be shown as `CachedAt(version)` only when the product freshness contract allows it. Progressive UI events should identify phase and version: `ResolvingLocal(version)`, `RunningLocal`, `WarmingRemote(version)`, `HedgeArmed(p95)`, `Partial(result_version)`, `AwaitingRemote`, `Committed(result_version)`, `StaleDiscarded`, or `OfflineBranchCreated`.

Freshness is a typed requirement (`Exact(Pin)`, `AtLeast(GenerationId)`, `AllowStale(max_age)`, `BestEffort`). The scheduler may use an older local result only under the request's freshness policy. It must never satisfy an exact pin with a newer or differently configured result.

## Differential graph scheduler state

Keep scheduling inside the graph runtime as a pure decision over an immutable snapshot plus an adapter-owned ledger:

```rust
pub struct PlacementInput<'a> {
    pub node: &'a ComputeNode,
    pub pin: Pin,
    pub local: LocalAvailability,
    pub remote: &'a [RemoteCandidate],
    pub costs: CostEstimate,
    pub budgets: ReservationSnapshot,
    pub freshness: Freshness,
    pub network: NetworkSnapshot,
}

pub enum Decision {
    RunLocal(LocalReservation),
    RunLocalAndArmHedge { local: LocalReservation, hedge: HedgeReservation },
    PrefetchLocal { chunks: ChunkSet, reservation: PrefetchReservation },
    RunRemote(RemoteReservation),
    RemoteOnly(RemoteReservation),
    ShowCached { result: ImmutableResult, freshness: FreshnessStatus },
    WaitLocalCapacity,
    OfflineBranch(BranchId),
}
```

`Decision` is pure and replayable. The mutable adapter owns leases, timers, transport, cancellation, and event delivery. The graph itself owns deduplication by `ComputeKey` and result publication by a winner CAS. This keeps placement beside differential invalidation and immutable result reuse rather than building a separate local/remote subsystem.

## Migration plan

1. Extend `heart-adaptive` with cost distributions, freshness, local/remote execution classes, and reservation-only decisions. Keep `next_action` pure and preserve existing storage contraction actions.
2. Add a graph node identity and immutable result ledger around `heart-root` generation IDs, root diffs, operation/capability IDs, and existing index snapshot/query identities.
3. Adapt `server/index/routing` replies to carry authority manifest, input digest, and fence token while preserving route/snapshot/assignment validation.
4. Build a local adapter on `server-runtime` permits and waiters. Do not hold a local permit during remote fetch; model missing chunks as separate prefetch nodes.
5. Add remote adapter with bounded upload/download bytes, p95 network queue estimates, warm-cache/prefetch priorities, and offline branch persistence.
6. Add hedges only for deterministic pure nodes, initially behind a low hedge fraction and explicit metrics. Verify duplicate work and loser cancellation before raising the budget.
7. Add GC pins and lease expiry/reconciliation, then make result publication fence-checked and idempotent.
8. Integrate compiler/application only after measuring its existing one-slot worker path (`compiler/application/runtime.rs:438-473`, `831-890`).

## Verification and measurement

Prove with model/property tests: no local-capable decision waits on remote; reserve rollback on every rejection; no dependency holds an execution permit over network wait; winner CAS publishes at most one result; stale/foreign fence cannot publish; duplicate deltas converge or enter typed conflict; GC never removes a live lease/published input; exact freshness never accepts another pin; cancellation releases the last reservation only after terminal ownership is settled.

Measure p50/p95/p99 end-to-end interaction latency, local queue delay, missing chunk bytes, remote queue/upload/download time, hedge rate, duplicate CPU/egress, loser cancellation latency, cache hit rate, branch reconciliation time, stale-result rate, and GC pin retention. Compare local-only, remote-only, local-primary with hedge, and local-primary with warm prefetch across offline, high RTT, packet loss, local CPU saturation, and remote queue saturation. Report distributions and reserve failures; never claim a fixed latency bound.

The existing routing benchmark (`server/index/routing/benches/routing.rs`) and root capacity-planning benches (`heart/root/benches/capacity_planning`) are natural starting points. Add an interaction harness that replays immutable version/delta traces and injects network queue distributions. Use the Tail-at-Scale paper's p95 hedge threshold only as an initial hypothesis, not a constant.

## Proven facts and design hypotheses

Proven by source: typed generation/snapshot pins; immutable root and streaming diff; pure bounded adaptive policy; local/remote residence and remote health vocabularies; fixed resource/retry budgets; snapshot/query/route/assignment validation; bounded retries; runtime permits, waiters, cancellation, and terminal ownership; and the compiler's separate one-slot worker channel.

Design hypotheses: local execution will meet the interaction budget often enough to justify local primacy; p95-triggered hedging will reduce tail latency under this workload; warm remote caches will reduce upload time; a unified graph ledger will reduce duplicate work; remote prefetch will improve future local latency; and branch/delta sync will be cheaper than full version transfer. Validate each with the measurements above.

## Research notes

The placement design follows the selective, delayed, cancellable hedging tradeoff in Dean and Barroso's *The Tail at Scale*. The synchronization design follows local-first principles and delta-state/CRDT work: local operations can remain available during disconnection when the data type's merge law supports it, while non-commutative authority decisions require explicit reconciliation. These sources support the protocol constraints; they do not prove this repository's latency or consistency behavior.
