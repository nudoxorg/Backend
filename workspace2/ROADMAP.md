# Capability roadmap

This is the active plan. A checked box means the named vertical proof exists, not that the distributed
product is finished. [`WAVE1.md`](WAVE1.md) is historical ownership context.

## Current checkpoint

The portable foundation is coherent enough to build on, but the product is still between a local
reference implementation and its first remote data plane. The strongest completed properties are
typed identities and wire records, borrowed validation, canonical roots/locality, demand-bounded
hydration, first-write-wins memory storage, a bounded lock-free runtime, a deterministic workflow
reducer, typed probes, one measured SIMD validation kernel, and the beginning of a packed object
artifact.

The remaining gaps are architectural, not polish:

- `GenerationRoot` still owns a boxed native row arena instead of making canonical bytes the primary
  owner with borrowed/mmap/lease adapters.
- complete local closure yields one non-forgeable verified capability, but no durable publication
  head consumes it and releases a distinct published capability after a stable receipt.
- `PinnedObjectRequest` is rebound after the provider was already selected; operation binding should
  happen once and eliminate the duplicate generation/object comparisons.
- `nudox-workflow` proves canonical records and shared asynchronous append, but not a crash-safe file
  journal with group commit, sync receipts, reopen, torn-tail handling, and fault injection.
- the object pack validates its header and ordered directory, but has no public zero-copy lookup/body
  view, selected content verification, authenticated partial range, or async leased range adapter.
- transport, horizontally partitioned remote index, vertically elastic compiler workers, adaptive
  NVMe/object-store placement, and the lean GUI are contracts only.
- performance evidence is Apple M3 Pro focused; x86, sustained contention, cache-miss/branch, power,
  and binary-size baselines remain incomplete.

## Ordered capability graph

### 0. Review system and integration spine — active

- [x] Common craft, domain, hostile-review, layout, dispatch, and provisional rubric skills.
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

### 1. Portable typed foundation — implemented, debt remains

- [x] Domain-separated identities, closed registries, canonical fixed records, borrowed frame views,
  exact validation errors, root/locality planning, immutable memory store, runtime/workflow core.
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

- [ ] Single-owner file journal with bounded MPSC submissions, reusable group-commit buffers, stable
  receipts, typed poison/shutdown, and no producer-side file or probe sharing.
- [ ] Crash/reopen at every prefix, short write, sync failure, torn tail, duplicate record, and
  directory durability evidence against an independent reducer.
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

## Next-batch readiness

| Capability | State and prerequisite | Root integration concern |
|---|---|---|
| Index I0 | Terra calibration active; implementation follows on the same transcript | I0 and C0 may each propose minimal sealed identity rows; root serializes the shared registry diff and re-proves global uniqueness. |
| Compiler C0 | Terra calibration active; implementation follows on the same transcript | Static dispatch must show two genuinely distinct concrete consumers before any macro or generic surface is accepted. |
| Leased range T0 | Queued behind one free Terra-plus-Luna pair; complete object-pack view is available | First terminal is runtime-independent lease conservation plus explicit partial/cancel/fail behavior; no transport/runtime SDK enters core. |
| Durable journal D0 | Rich rejected prototype retained off the shared branch | Re-scope before dispatch: its green candidate exceeded both production and test ceilings, so correctness alone cannot earn integration. |
| Observability health | Adapter seam is green after removing the dedicated scenario crate | A future lifecycle capability must expose real exporter failure without reintroducing a test-support package or portable SDK dependency. |

## Manager/worker execution cycle

One fresh Terra manager owns one capability and its progressively loaded skill. It commissions narrow
read-only scouting, a smallest-proof builder, an independent breaker, and targeted repair. Every
writing worker starts from a frozen digest/LOC ledger in an isolated branch and commits each passing
checkpoint; rejected work remains auditable without contaminating the manager branch. The requested
worker model is verified live at cycle start and is never silently substituted. The manager returns a
single evidence packet only after exact falsifiers and full owned gates pass. The root then performs
the cross-crate architectural review, updates this graph, and generalizes only lessons that actually
prevented a repeatable failure.

The next object-plane trial is authenticated partial binding followed by leased range transport.
Index I0 and compiler C0 are being independently calibrated as greenfield nested-workspace trials;
their managers own architecture and verification while narrowly scoped Luna workers implement or
falsify one checkpoint at a time. Parallelism never permits agents to invent incompatible artifact,
lease, or terminal representations.

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
