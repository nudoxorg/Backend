# One local engine, with remote work as an acceleration tier

The application owns a complete local commit and query path. Remote machines execute the same version-bound recipes, retain shared immutable results, and send missing objects or deltas ahead of demand. They do not own a second semantic truth. This is a deliberate replacement for separate compilation, index routing, synchronization, and cache policies.

The local-first principle is consistent with [Ink & Switch's original local-first work](https://www.inkandswitch.com/essay/local-first/): local data and offline use are product properties, not merely a cache setting. The concrete scheduler and protocol below are this plan's design.

## 1. One workspace owner across every surface

Run one local workspace service, `locald`, using the shared `engine` crate. Desktop, CLI, and MCP attach to it over a versioned local transport and bootstrap it if absent. A per-workspace owner lock and fencing epoch prevent two services from advancing the same durable head. The daemon owns storage admission, native sessions, arrangements, work deduplication, and subscriptions. Multiple clients do not each hydrate the IR, rebuild indexes, and spawn a compiler owner.

The service is a small supervisor around the engine, not the place where domain logic accumulates. Its lifecycle includes authenticated local endpoint discovery, startup recovery, idle policy, client disconnect cleanup, graceful cancellation, and durable commit completion. Library-level tests may embed the same engine directly. A GUI-embedded service and a standalone daemon must never simultaneously own a workspace; embedding is a deployment option only through the same ownership protocol.

The public API has four operations:

```text
commit(intent, expected_workspace_root, idempotency_key) -> CommitReceipt
query(recipe, parameters, source_selection, freshness) -> ViewSnapshot
subscribe(view, cursor, bounded_credit) -> ViewDelta | Reset | Progress
control(job_or_subscription, cancel_or_priority_or_release) -> Status
```

Commands resolve product identity and policy inside `library`; the transport carries stable versioned DTOs. No client receives raw pointers or generation-local integer IDs. Large local views can later use read-only mapped pack handles with explicit leases; the initial protocol can use bounded column batches. Shared memory is an optional physical transport, not a second data model.

## 2. Separate semantic work identity from execution attempts

```text
RecipeId = H(operator ABI, algorithm/configuration, capability revision,
             canonical schema rules, declared output contract)

InputManifest = sorted positive object/facet reads
              + negative/range/membership reads
              + source/toolchain/environment/authority dependencies

WorkKey = H(RecipeId, canonical parameters, validated InputManifest)
WorkAttempt = (WorkKey, scheduler owner epoch, attempt ordinal, resource lease)

ResultReceipt = (WorkKey, exact input manifest/root bindings,
                 output roots, coverage, worker authority,
                 attempt/fence, authenticated execution receipt)
```

The request before dependency discovery has a *candidate key*, not necessarily a final valid `WorkKey`. Candidate manifests are revalidated against selected input roots, using versioned directory/range summaries where possible. A cache cannot reuse a result merely because all previously observed positive files stayed equal: a new candidate file, missing import becoming present, generated input, feature flag, or environment change can invalidate it.

Do not include a random attempt ID, worker location, wall clock, physical pack ID, or current mutable cache contents in semantic work identity. Conversely, do include architecture/ABI or nondeterministic configuration when it changes observable output. A run that intentionally uses nondeterminism must have a declared result-equivalence contract or a pinned seed/authority result. Equal work keys imply the stated equivalence, not automatically byte equality for all external providers.

Maintain one in-flight record per admitted WorkKey and share its output among waiters. Different output projections reuse common recipe subgraphs and input arrangements. Cancellation removes a waiter's demand; it cancels the shared run only when its remaining consumers and background policy permit it.

## 3. Placement is joint incremental refresh planning

For each ready output shard, the optimizer enumerates valid ways to obtain it:

| Candidate | Main work charged | Useful when |
|---|---|---|
| Local memo hit | Manifest validation, pin, any local decode | Result and needed pages already present |
| Delta advance locally | Delta read, support probes, fan-out, output and compaction debt | A close checkpoint and warm arrangements exist |
| Fetch remote result | Lookup, missing output bytes, decode and admission | Remote has the exact already-computed work |
| Local scoped recompute | Authority/graph work, allocations, materialization | Cold state or broad delta makes maintenance expensive |
| Remote delta/recompute | Missing input bytes, remote queue/state warmup, execution, output transfer and validation | Remote has data/capability or sufficiently cheaper work |
| Bounded pure hedge | Both attempts' actual shared-resource use | Tail risk justifies duplication |

The unit of placement is a dependency-closed recipe shard or connected subgraph, not each trivial operator. Keep join partners, warm dictionaries, source authority sessions, and related arrangements together when splitting them would add transfer and coordination. Tiny map/filter work should remain with its input batch. Shard large independent packages/key ranges/model batches, and coarsen adjacent jobs when their shared inputs dominate cost.

The optimizer chooses *where to update which retained state*, not merely where to rerun a function. It can send a small input delta to a remote warm trace and receive only an output delta; fetch a cached output and skip compute; or rebuild a local scope if maintaining a huge support set would be worse. The recent [Enzyme refresh planner](https://arxiv.org/abs/2603.27775) is relevant to choosing refresh strategies; integrating placement and transfer into that decision is our proposed extension.

Use a bounded planning budget and a small candidate family. Planning a 1 ms query for 10 ms is failed work avoidance. Very cheap local operations take the local fast path. More expensive jobs permit richer statistics and subgraph grouping. Plan-cache validity follows recipe/schema/cost-profile changes; physical plan changes need not change equivalent logical results.

The implemented route learner stores one bounded row per `(RecipeId, input-size bucket)`. Fixed slots inside that row hold local-state and remote-state evidence independently. Remote warming therefore cannot erase the local compute baseline, local fallback cannot erase a warm transfer distribution, and the model avoids allocating the Cartesian product of both state axes. Each remote slot retains a bounded tail ring for p95 selection and its own failure circuit; percentile selection sorts a stack copy rather than allocating. Evidence expires under the owner clock, and Available evidence may seed a new Warm slot until the first exact Warm observation arrives.

## 4. Latency contracts that are actually enforceable

The remote completion estimate includes:

```text
client queue + missing input transfer + network delay + worker queue
  + capability/session warmup + compute/materialization
  + missing output transfer + local decode/validation/publication
```

Overlapped stages need an explicit critical-path estimate; do not add independently sampled p95 values and call the sum a calibrated end-to-end p95. Track end-to-end distributions by workload/locality class, confidence, and recent failures. Count shared missing chunks once across coalesced jobs. Cost includes CPU, bytes, energy and retention, not just remote execution milliseconds.

Three named policies make the tradeoff visible:

- **Local baseline:** start local immediately using reserved interactive resources. Remote may race only within separate CPU, network, disk, memory, and decode envelopes, or precompute off the critical path. This best protects the existing local path, though shared hardware interference still has to be measured.
- **Deadline optimized:** try an exact remote hit or remote warm execution while preserving a local reservation and latest local start time. This can save local work. It meets a budget only under calibrated estimates and admission assumptions; it can be slower than starting local immediately.
- **Background efficiency:** choose minimum total resource cost subject to freshness/energy deadlines, allowing remote queues and transfer batching. It never consumes the reserved interactive envelope.

For a local-capable interactive operation, remote unavailability does not block the local route. If local capability or input closure is absent, return an explicit `MissingLocalInput`, `CapabilityUnavailable`, or `RemoteRequired` state, with an optional pinned prior result if the freshness contract allows it. Installing a remote-only embedding model does not make ordinary local document viewing remote-dependent.

No architecture can guarantee a remote attempt both saves arbitrary local compute and never adds latency compared with starting that compute immediately. The major way to achieve both in practice is **pre-demand computation and replication**: the remote performs work while the local user is doing something else, and the resulting version is locally ready before it is requested.

## 5. Demand prediction without turning the client into a cloud terminal

Treat durable offline/pin policies and transient navigation interests as separate inputs to the local demand planner. Durable policies survive restart; hover, scroll, speculative search and waiter records are bounded ephemeral state. Only explicitly synchronized user intents enter workspace history.

Prefetch likely package versions, neighboring document fragments, repeated toolchain outputs, and expected next query ranges. Maintain remote package-derived facts and embedding outputs keyed by the same recipe/input identities used locally. Send compact version summaries first; download only missing outputs that are likely to become demanded. Confidence, byte budget, storage pressure, and observed usefulness decide admission. A failed prediction is charged as wasted bytes/CPU, not counted as a success because it hit a cache later during testing.

A remote warm arrangement can advance through many commits while the client is idle. On reconnect, local subscriptions request the delta from their actual pinned view root if retained; otherwise they receive a complete checked new root and bounded hydration. The user-facing local state does not advance to a root whose required locally promised objects are missing.

## 6. Worker protocol and result acceptance

The request includes recipe/capability IDs, input roots and manifest, output contract, key/range/SCC scope, existing checkpoint if any, resource/deadline class, expected authority, and attempt fence. The worker advertises permitted recipes, supported schemas, session warmth, root availability, resource limits, and expiring health observations. A healthy worker with incompatible facts is not a valid candidate.

Use the same pack/batch format and decoder admission rules locally and remotely. Exchange exact missing IDs; probabilistic availability filters can reduce lookup messages but false positives require an explicit missing-object repair response. Transfer bounded, resumable hashed extents with a manifest that binds complete logical objects. Never deserialize an unbounded advertised allocation before length/schema checks.

The implemented Product slice makes the canonical Merkle node preimage the
remote executable object. A leaf preimage already authenticates its rows, so
the receiver does not persist a second object for every row. It records the
bounded proof first and atomically publishes a content-addressed completion
marker only after admission. Branch preimages carry typed child commitments;
the worker then walks those commitments depth first with a bounded frontier and
feeds rows directly into `ProductProjectionBuilder`. It never hydrates the
relation into a second collection. Local fallback calls the same projection
builder, and result admission compares the exact canonical bytes and semantic
coverage bound to the requested source root.

This layout turns an edit into a transfer of its changed Merkle frontier. A
worker that retains sibling node digests across roots reuses their admitted
preimages directly. The fixed 32-byte Product input lives outside the bounded
row/object LRU so a wide page walk cannot evict the input needed to execute the
closure. Root publication leases the input object and completed proof cache;
restart reopens the version-owned workspace manifest and relation proof through
the typed persisted-root boundary before the worker advertises warmth. Partial
proofs, stale root journals, mismatched manifests, and unleased objects are
reclaimed under one bounded GC budget.

Canonical cut policy version 2 selects boundaries from both row pressure and
encoded-byte pressure. Large values no longer reach the hard 64 KiB cap before
content anchors become eligible. The policy version is committed into every
node header and anchor digest, so the layout change cannot be mistaken for an
old canonical root. The black-box warm-root journey chooses an append frontier
and verifies that it transfers fewer node proofs than the cold root while
still comparing the remote projection against an independent local oracle.

All worker transports use one incremental framed-stream kernel. It retains the
partially read length and payload across `TimedOut`/`WouldBlock`, reuses the
payload allocation across frames, distinguishes clean EOF from truncation,
and resets the cursor only when a connection generation changes. The worker's
active cancellation poll and locald's split background reader use that same
kernel instead of maintaining similar outer-frame decoders.

The current remote trace is therefore the authenticated node cache plus the
recipe input and result memo. It gives exact subtree reuse and delta-sized
transfer without introducing an independently mutable worker relation. Stateful
operator traces may be added behind the same `(recipe, input roots, parameters,
coverage)` identity once they can prove a checked frontier transition; they
must not weaken the node and result admission path.

Acceptance is staged:

1. Authenticate the peer and verify it is authorized for the requested recipe/data scope.
2. Validate the receipt's WorkKey, exact inputs, authority/capability, output contract, coverage and attempt fence.
3. Verify transferred pack/object integrity, canonical decoding, references and structural/domain invariants.
4. Pin all required admitted outputs, then compare-and-swap the result slot for this WorkKey.
5. Advance a selected view or authoritative relation transaction only if its source basis and publication fence are still valid.

**A content hash proves content identity, not correct execution.** A signature authenticates a worker's claim; it does not make an incorrect compiler or query correct. Merkle proofs prove membership/absence under a known root, not completeness of a claimed derivation. The default trust model uses authorized compatible workers plus structural/domain validation. Higher-assurance recipes may require deterministic redundant execution, sampled verification, replayable evidence, or a separately designed verifiable computation scheme. Do not market ordinary signed results as proofs of computation.

Authorization policy is itself versioned and checked at result selection/reuse, not only when the job starts. Ordinary key rotation can preserve already accepted receipts under an explicit historical-validity rule. Compromise revocation or removal of a recipe authority marks affected provenance invalid according to the revocation scope/time rule, prevents new selection, and quarantines selected dependent views until trusted revalidation/recomputation. Keep a provenance-to-result dependency index so this is a targeted transition with explicit coverage, not an unnoticed global trust change. A signed historical result is not automatically trusted forever, and revocation does not physically erase already delivered client data.

Deterministic equivalent attempts should agree. If a late second result disagrees after the first was accepted, quarantine that recipe/worker combination, mark affected provenance suspect, and recompute from a trusted path; it is impossible to retroactively promise neither was ever visible. Strict dual verification waits for both and belongs to an explicit assurance policy, not the fast path.

## 7. Reservations, cancellation, and effects

Use nonblocking/rollback-capable admission for memory, CPU and output credits. A job waiting for network input does not hold a scarce execution slot. Dependency objects are fetched/pinned as ready dependencies; compute is admitted once it can proceed. Mutable scratch belongs to the worker/attempt. Immutable input/output segments may be shared, with retained bytes charged once plus each actual pin's lifetime.

Inspect closure manifests, object availability and size summaries before choosing placement; do not hydrate the whole local input just to decide to run remotely. A held fallback reservation has a bounded latest-start deadline, reclaim policy and measured opportunity cost to other local work. If reclaimed, the scheduler must revoke the promised fallback bound or start/re-admit local work; it cannot continue advertising a reservation it no longer owns. A hedge is charged for both attempts' scratch, possible outputs, overlap and loser cleanup. Soft predicted credits and hard admitted capacity are distinct types/states.

The local scheduler owns the attempt fence. Leases expire by the owner's observation and authenticated protocol; do not compare unrelated machines' monotonic timestamps. Remote lease renewal carries duration/epoch semantics. A stale worker may submit immutable cache candidates but cannot advance the selected head. Cancellation is best effort physically and exact logically: stale publication rights are fenced immediately, while resources are released when actual work and pins end.

Hedge only repeatable pure work. [The Tail at Scale](https://barroso.org/publications/TheTailAtScale.pdf) motivates selective duplicate requests to reduce tails, but duplication consumes resources and can amplify congestion. Enforce a global hedge fraction, bytes/CPU budget and per-job attempt cap; stop hedging during overload. Deduplicate input fetches and arrange remote cancellation, but measure work actually completed by losers.

Package acquisition, installs, file writes and publication are **effects**, driven by durable intents and idempotency keys. They have an explicit state machine and fencing; they are not run twice because the pure graph was hedged. A negative relation update retracts a desired fact; it does not undo an external side effect. Reconciliation or compensation is a separate operation with its own receipt.

## 8. Local commits, replication, and conflict semantics

Prepare canonical objects and the `WorkspaceDelta`; validate exact base/before-values, all relation/schema constraints, provenance and coverage. Durably stage the referenced object closure and transaction record before atomically advancing the selected workspace head. The durability boundary must be specified for the actual filesystem/store implementation: flush ordering, atomic rename or transaction semantics, directory durability where needed, and recovery checks. Return the local commit receipt only after that boundary. Emit notifications from the durable sequence; a crash between commit and notification is recovered by cursor replay.

Replication sends commit ancestry and workspace-root summaries, then missing canonical nodes/objects or exact base-bound deltas. A peer with the right base can apply the transition once. A peer without it fetches the base/target closure or uses a Merkle diff; it does not invent the missing expected state. Verify the final canonical WorkspaceRoot before selecting it.

Offline devices create branch histories. Same final visible relation contents can have the same StateRoot despite different commits. Reconciliation is schema-specific:

| State | Merge rule |
|---|---|
| User pins, tags, selected preferences | Explicit CRDT or deterministic domain merge with tombstone/causal retention rules |
| Source documents | Existing file/VCS authority; a future text CRDT is a separate editing feature, not implied by CAS |
| Compiler facts | Recompute/select under exact source and authority; never CRDT-merge arbitrary semantic bytes |
| Derived indexes/views | Reuse by proven input/recipe identity or recompute; no independent conflict authority |
| Permissions, capability activation, external effects | Designated authority and fenced transitions |

Delta-state replication is useful for appropriately defined convergent state, as discussed in [efficient CRDT synchronization research](https://arxiv.org/abs/1803.02750). CRDTs are not a universal merge function for this engine. Concurrent scalar edits, package version choices and deletion/resurrection need visible product semantics, not an unexplained last-writer-wins default.

## 9. Storage locality, GC, and privacy are one ownership problem

Keep hot demanded pages in RAM, likely reusable objects and offline-promised closures locally durable, and cold history/large optional materializations in remote tiers when policy permits. Eviction operates on physical residency, not logical existence. A published local offline promise pins the transitive required object closure. Remote presence never satisfies that promise without verified local availability.

GC roots include selected workspace/branch history retention, durable user pins, live view/session pins, prepared transactions, effect receipts and active transfer/attempt leases. Trace frontiers decide what hot temporal support can compact; durable history policy decides which immutable snapshots survive. Neither substitutes for the other. A two-phase retire/grace protocol prevents cancellation races from deleting an object still in flight. Reclaim deduplicated physical extents only when all logical owners and pins release them.

Deduplication and cache discovery stay within authorized sharing domains. A tenant must not test arbitrary private content existence via a global hash lookup. Remote job admission includes the allowed data namespace and egress policy. Cross-project or public-package sharing is explicit; do not hash a private workspace and upload it automatically merely because a remote estimate is attractive. Encryption and physical key rotation change PackId/locations, not canonical plaintext logical identity inside the authorized domain.

## 10. Product-visible freshness and failure behavior

Every result names its source basis, recipe, coverage and view root. The UI can show current local documents while semantic search is still catching up. A multi-lane query either selects a common coherent input snapshot or explicitly reports lane-specific coverage under its query contract. Fast stale output must not masquerade as current complete output.

| Event | Required behavior |
|---|---|
| Offline at startup | Open locally promised roots; rebuild local demand; queue replication |
| Edit while remote job runs | Commit locally; late output remains tied to old inputs; reuse only where manifest validation permits |
| Slow/disconnected subscriber | Bound queue, release obsolete demand, send reset to a pinned complete ViewRoot |
| Worker dies or lease expires | Fence attempt; local fallback/retry policy; retain inputs until lease/transfer cleanup |
| Crash after durable head update | Replay receipt/notification sequence without double-applying intent |
| Corrupt pack or incompatible schema | Reject admission; recover a verified replica or report unavailable scope |
| Local memory pressure | Drop optional demand/materializations, compact under credits, preserve committed roots and interactive reservation |
| Broad edit/huge SCC | Choose budgeted scope rebuild; retain valid prior view with explicit freshness |

Measure local-only and connected end-to-end p50/p95/p99 with identical work, background contention and offline transitions. Report remote bytes and CPU avoided alongside wasted speculation, retained memory, battery impact, and SLO misses. The goal is lower total work with a dependable local product, not a higher percentage of jobs labeled remote.
