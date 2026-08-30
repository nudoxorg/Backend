# Capability roadmap

This is the active plan. A checked box means the named vertical proof exists, not that the distributed
product is finished. [`ORCHESTRATION.md`](ORCHESTRATION.md) defines the Sol/Terra/Luna execution and
evidence-custody system; [`WAVE1.md`](WAVE1.md) is historical ownership context.

The active completion waves, controller ownership, exact product terminals, and integration order are
in [`OVERNIGHT_COMPLETION.md`](OVERNIGHT_COMPLETION.md). Dispositions of the recent P1/P2/P5/P6,
performance, and GUI tasks are frozen in
[`RECENT_TASK_INTEGRATION.md`](RECENT_TASK_INTEGRATION.md). Those ledgers supersede the old
prototype-only dispatch strategy without deleting its evidence.

## Current checkpoint

The portable foundation is coherent enough to build on, but the product is still between a local
reference implementation and its first remote data plane. The strongest completed properties are
domain-separated hash construction and wire records, borrowed validation, canonical roots/locality, demand-bounded
hydration, first-write-wins memory storage, a bounded lock-free runtime, a deterministic workflow
reducer, typed probes, one measured SIMD validation kernel, and the beginning of a packed object
artifact.

The remaining gaps are architectural, not polish:

- typed identity authority now survives raw wire/index round trips and locality validates one
  artifact-wide authority before lending typed payloads; the next foundation gap is canonical-byte
  ownership for roots rather than another identity repair.
- `GenerationRoot` still owns a boxed native row arena instead of making canonical bytes the primary
  owner with borrowed/mmap/lease adapters.
- complete local closure yields one non-forgeable verified capability, but no durable publication
  head consumes it and releases a distinct published capability after a stable receipt.
- `PinnedObjectRequest` is rebound after the provider was already selected; operation binding should
  happen once and eliminate the duplicate generation/object comparisons.
- `nudox-workflow` now has a crash-safe, single-owner file journal with sync receipts, reopen,
  torn-tail repair, exclusive ownership, directory durability, and fault injection; bounded MPSC
  group commit and atomic publication remain open.
- the object pack validates its header and ordered directory, but has no public zero-copy lookup/body
  view, selected content verification, authenticated partial range, or async leased range adapter.
- transport, horizontally partitioned remote index, vertically elastic compiler workers, adaptive
  NVMe/object-store placement, and the lean GUI are contracts only.
- Wave C.7 froze a useful shared application-service contract but stopped before implementation after
  two reviewer-custody failures. Its evidence is integrated; any restart must use the corrected
  context-free Sol-sidecar -> Terra-reviewer custody and cannot inherit approval from the blocked run.
- performance evidence is Apple M3 Pro focused; x86, sustained contention, cache-miss/branch, power,
  and binary-size baselines remain incomplete.

## Ordered capability graph

### 0. Review system and integration spine — active

- [x] Common craft, domain, hostile-review, layout, dispatch, and rubric-writer skills; the product
  rubric is explicitly unadopted until blind reviewer/writer calibration closes its readiness ledger.
- [x] Manager skill with scoped worker evidence, independent breaking, churn accounting, and skill
  feedback.
- [x] Forward-test the manager cycle on the server-only tracing/OTEL adapter and record where worker
  prompts, budgets, or review packets caused avoidable turns.
- [x] Put the from-scratch workspace under a path-scoped Git baseline; workers now commit passing
  checkpoints on isolated branches and Terra managers cherry-pick only reviewed increments.
- [x] Remove the dedicated scenario crate; cross-crate journeys live inside ordinary crate
  integration tests and never enter shipping dependency graphs.
- [ ] Add one deterministic system driver whose commands can later run against memory, file, and
  simulated-network adapters without shadow state.

### 1. Portable typed foundation — identity/locality boundary closed

- [x] Make serialized content/artifact identities carry and validate their closed domain/encoding
  authority while remaining fixed-width, allocation-free, borrowed, and single-pass hashed. Remove
  unchecked raw reconstruction and prove cross-authority rebranding fails.
- [x] Domain-separated hash preimages, closed marker registry, canonical fixed records, borrowed frame
  views, exact validation errors, root/locality planning, immutable memory store, runtime/workflow core.
- [x] Complete local closure produces one two-fact `VerifiedGeneration` that external code cannot
  forge; the effect-free `Verified -> Ready` marker transition was deleted rather than called
  typestate.
- [ ] Replace the retained native root row owner with canonical-byte-first borrowed views and concrete
  optional owners; measure HRTB callback, `self_cell`/Yoke-style owner, mmap, and leased-buffer shapes.
- [ ] Bind operation requests once at the `GenerationView` boundary; delete provider/request equality
  rechecks and make the verified capability reach the publication/operation consumer that requires it.

### 2. Server observability adapter — bounded seam proven; health plane remains

- [x] Portable lazy typed `Probe`, bounded allocation-free flight recorder, server-only nested adapter.
- [x] Close exact trace/log correlation, metric snapshots, bounded overload, flush/shutdown ownership,
  disabled-interest laziness, and production-dependency negative space under the manager trial.
- [x] Preserve exact SDK queue/drop evidence without recursive telemetry or data-plane ownership.
- [ ] Expose background span/log exporter failure through one bounded caller-owned health/lifecycle
  capability. Test-private exporter counters and SDK error text are not operational visibility.
- [ ] Exercise a real collector/OTLP outage and recovery in a nested server harness without adding its
  runtime, transport, TLS, or protobuf graph to portable crates.

### 3. Packed object plane — active managed component

- [x] Exact caller-output writer, compact count header, borrowed ordered directory validation, and
  representative non-empty zero-allocation evidence.
- [x] Binary-search a descriptor into a typed body range without allocation, reparsing, or constructing
  temporary identities inside comparisons.
- [x] Lend body bytes from the original owner and perform optional selected BLAKE3 verification with
  exact mismatch/source evidence.
- [ ] Add sparse authenticated range binding: header/directory first, requested bodies second, missing
  ranges explicit in typestate. Evaluate `bao-tree`/iroh-blobs range proofs without making transport
  technology part of logical identity.
- [ ] Integrate pack writer → range fetch → validation → immutable store → hydration replay under
  exact byte/copy/allocation/work counters.

### 4. Durable publication and workflow plane

- [x] Blocking single-owner file journal with stable receipts, typed poison/reopen, streaming replay,
  exclusive physical ownership, and no retained log allocation. See
  [`DURABLE_JOURNAL_D0_CLOSURE.md`](DURABLE_JOURNAL_D0_CLOSURE.md).
- [x] Crash/reopen at every prefix, short write, sync failure, torn tail, duplicate record, checksum
  enforcement, and directory durability evidence against an independent reducer.
- [ ] Put bounded MPSC submissions and reusable group-commit buffers around the single file owner;
  prove exact receipt fan-out, cancellation, poison, shutdown, and no producer-side file/probe sharing.
- [ ] Immutable publication log plus compact CAS head/index; only a stable receipt can release the
  publication effect and convert verified authority to published authority.

### 5. Leased async range transport

- [ ] Runtime-independent semantic stream with owned byte/item leases, explicit partial/degraded/
  cancelled/failed terminal, bounded reorder, and deterministic wake/cancel tests.
- [ ] Compare a bounded blocking file worker with completion-owned adapters such as Compio; keep
  runtime and platform types in nested adapters.
- [ ] Compare HTTP range transport with iroh/Bao verified streams. Iroh's current mainline blob crate
  is not assumed production-ready; pin only a measured, audited release if it wins.

### 6. Horizontally scalable remote index

The full greenfield contract and manager slices live in
[`INDEX_GREENFIELD_PLAN.md`](INDEX_GREENFIELD_PLAN.md). Legacy `workspace/index` is inspiration and
anti-pattern evidence only; none of its APIs, schemas, stores, or search behavior is a compatibility
constraint.

- [x] Establish the minimal nested no-std identity vocabulary: snapshot, exact-segment, and
  lexical-segment aliases use the checked central domains directly. Dormant future-family variants,
  a one-implementation projection trait, and raw rebranding surface are absent.
- [ ] Partition immutable generation/object metadata by canonical key with rendezvous placement only
  as an efficiency hint; durable object/change bytes plus atomic publication remain truth.
- [ ] Build indexes from sealed deltas, publish compact immutable segments once, and share compaction
  output across replicas. Popular segments reside on NVMe/RAM by measured demand; cold segments age
  to object storage without changing IDs or query semantics.
- [ ] Keep exact, lexical, relation, usage, and vector projections as separate typed segment families
  over one snapshot/publication/range substrate; no universal registry DTO or mutable index authority.
- [ ] Exercise node loss, stale routing, rebalance, hot-key demand, and remote outage while local
  proven facts remain usable.

### 7. Vertically elastic compiler plane

The full greenfield compiler and compact semantic-IR contract lives in
[`COMPILER_IR_GREENFIELD_PLAN.md`](COMPILER_IR_GREENFIELD_PLAN.md). Legacy compiler, IR model, and IR
VCS code has no compatibility standing.

- [ ] Content-address compiler inputs/toolchain/environment; schedule idempotent jobs under typed
  rate and byte credits; stream artifacts/logs rather than staging object graphs.
- [ ] Emit compact borrowed semantic fragments through compile-time-selected concrete frontends;
  package IR is an immutable manifest, and history/diff is ordinary fragment publication rather than
  a second VCS subsystem.
- [ ] Scale worker size vertically and fleet width independently. Demand and cost choose compilation
  placement; index correctness never depends on compiler availability.
- [ ] Cache only reusable immutable compiler products with measured recomputation cost; do not cache
  merely because a value was expensive once.

### 8. Lean local client and GUI

- [ ] Keep the installed client below the declared 50 MB target with feature/dependency/binary-text
  budgets and no server SDK/exporter/database graph.
- [ ] Ship roots, descriptors, schemas, operation vocabulary, and minimal local store/runtime; fetch
  optional codecs, analyzers, models, or compiler components by typed artifact identity on demand.
- [ ] Adapt capability placement to latency, local demand, battery/memory, and remote health without
  changing operation semantics. Local work expands during remote inconsistency and contracts again
  when remote service is healthy.

## Parallel prototype portfolio

[`SOL_PROTOTYPE_ORCHESTRATOR_HANDOFFS.md`](SOL_PROTOTYPE_ORCHESTRATOR_HANDOFFS.md) contains nine
self-contained Sol-session prompts for canonical root/hydration, durable publication, authenticated
partial storage, real index, real compiler/IR, declarative registries, the local-first heart, lean
GUI/component delivery, and the unified proof harness. These sessions commit isolated controls and
alternatives but never merge or close roadmap boxes. Root extracts evidence and re-derives a smaller
integration card on current shared state; prototype APIs have no compatibility standing.

This portfolio is now historical. New work uses the stage controllers in
[`OVERNIGHT_COMPLETION.md`](OVERNIGHT_COMPLETION.md); P5 remains live until its active controller
returns, P1 is a re-derivation candidate, and rejected P2/P6 implementations remain negative evidence.

## Next-batch readiness

| Capability | State and prerequisite | Root integration concern |
|---|---|---|
| Typed identity integrity | Integrated with closed registry, public all-pairs/split-point falsifiers, and safe typed locality payloads | Keep raw authority checks at ingress and resist reintroducing unchecked constructors or post-validation decoding. |
| Index I0 vocabulary | Integrated after identity repair; nested tests, strict linting, formatting, and docs pass | Start the immutable snapshot/segment grammar without rebuilding a future-family registry or generic family projection. |
| Compiler C0.1 | Accepted after root rejection and compaction; C0.2/C0.3 remain unstarted | The public value-dispatch path lends its exact input through two concrete generic rows; compile-fail subset/owner proofs and locked nested gates are closed. |
| Leased range T0 | Autonomous Terra calibration active; the first pre-edit Terra rejected an incomplete ABI/credit card | First terminal is a two-lease runtime-independent reorder/conservation proof with complete/cancelled terminals. Partial/degraded/failed and physical adapters remain later children. |
| Durable journal D0 | Synchronous single-owner durability substrate integrated; asynchronous heart remains open | Preserve the accepted file authority and its fault proofs while adding bounded MPSC/group commit as a separate owner, then bind stable receipts to publication CAS. |
| Observability health | Adapter seam is green after removing the dedicated scenario crate | A future lifecycle capability must expose real exporter failure without reintroducing a test-support package or portable SDK dependency. |

## Manager/worker execution cycle

One primary Terra manager owns one capability and its progressively loaded skill. It owns architecture,
proof, measurements, concise red tests, and acceptance; Luna workers implement bounded cards; a
separate read-only Terra reviewer attacks the card and artifact at proof checkpoints. Every writing
worker starts from a frozen contract digest and path ledger in an isolated branch and commits each passing checkpoint;
rejected work remains auditable without contaminating the manager branch. Both requested child models
are explicitly selected and never inferred from role names. The manager returns one evidence packet
only after it reproduces reviewer findings, exact falsifiers, and full owned gates. The root then
performs the cross-crate architectural review, updates this graph, and generalizes only lessons that
actually prevented a repeatable failure.

The next object-plane trial is authenticated partial binding followed by leased range transport.
Index I0 vocabulary and typed raw identity integrity are integrated, compiler C0.1 is accepted, and
leased range T0 is being recalibrated after its first builder proved the original card could not fit
its coupled ABI and proof boundary. Primary Terras own
architecture and proof, reviewer Terras remain read-only, and narrowly scoped Luna workers implement
one checkpoint at a time.
Parallelism never permits agents to invent incompatible artifact, lease, or terminal representations.

## Research decisions carried forward

- Rust typestate should use one unchanged runtime representation plus zero-cost state markers. The
  verified generation deliberately has no second marker state today; durable publication may
  introduce `Generation<Verified> -> Generation<Published>` only when the transition consumes a
  stable receipt and both states have real consumers.
- `self_cell`, Yoke, and similar self-referential ownership are adapter candidates only when a view
  must escape an HRTB callback and the extra owner demonstrably removes a copy/revalidation.
- `GhostCell`/QCell-style branding is relevant to owner-scoped mutable graphs, not immutable artifact
  views or lock-free queues by default.
- uv's useful pattern is disposable, versioned cache buckets with link/reflink-aware materialization;
  cache validity and expensive-build retention differ. Its large orchestration objects and dynamic
  reporter paths are not copied into the lean core.
- iroh-blobs/Bao demonstrates hash-and-range-described verified streams; the protocol shape is useful,
  while connection/runtime/store choices remain replaceable adapters.
- Tantivy and Quickwit demonstrate immutable segments, mmap/range-readable search, pruning metadata,
  and shared compaction output; Qdrant demonstrates useful segment and vector experiments. Their
  document APIs, metadata databases, consistency tradeoffs, and cluster types are not adopted.
- OXC, rust-analyzer/Salsa, and Cranelift demonstrate arena ASTs, change-aware stable summaries, typed
  dense entity maps, and pooled lists. These are measured techniques or frontend-local choices, not a
  universal compiler database or IR ownership model.
