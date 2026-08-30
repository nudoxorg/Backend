# Overnight completion program

This is the execution ledger for taking the greenfield workspace from its current foundation to one
end-to-end product proof. It supersedes the prototype-only portfolio as the active dispatch plan; the
prototype prompts remain historical design input.

“Complete” means the public terminal and evidence below exist on a clean committed candidate. It does
not mean a task reports success, a crate compiles, or a happy-path test passes. Each stage has one Sol
controller. Sol controllers may commission capability-scoped Terra managers, who use explicit Luna
implementers and independent Terra reviewers according to `ORCHESTRATION.md`.

## Shared laws

- Start from the recorded `orchestra-shared` baseline in an isolated worktree. Never edit or clean the
  dirty canonical checkout under `/Users/mileswirht/Downloads/backend`.
- Read `ORCHESTRATION.md`, `ROADMAP.md`, `TESTING.md`,
  `.codex/skills/steward-greenfield-rust-program/SKILL.md`,
  `.codex/skills/deliver-reviewed-rust-slice/SKILL.md`,
  `.codex/skills/manage-rust-swarm/SKILL.md`, and the named domain skills completely.
- Explicitly select the registered roles. Sol is `gpt-5.6-sol`/`xhigh`; Terra manager and reviewer are
  `gpt-5.6-terra`/`xhigh`; implementation workers are `gpt-5.6-luna`/`max`. Record runtime task/model/
  effort/config/checkout receipts. A role label is not evidence.
- Unit, property, fault, Loom, Miri, and compile-fail tests live with their owning crate. Cross-crate
  journeys live in ordinary top-level `tests/` directories. No test-only crate, support API in a
  shipping graph, or giant scenario file is allowed.
- No compatibility with `workspace/`, the deleted docs tree, legacy index/compiler/IR/GUI APIs, or
  prototype APIs is required. Old material supplies attacks and possible mechanisms only.
- Prefer construction-time invariants, borrowed views, caller scratch, scoped ownership, exact
  fallible storage, and bounded streams. Allocation is allowed when its lifetime and allocator fit
  the workload; `Box`/`Vec`/`Arc`/`dyn` are never default escape hatches.
- Every terminal preserves structured source, owner, chronology, and causal error. No panic path,
  erased `map_err`, string state, magic primitive, or forged typestate substitutes for a real fact.
- SIMD, unsafe, lock-free algorithms, generics, macros, and dependencies require a scalar/safe/simple
  control, a current consumer, and evidence for the exact benefit. Branch predictability outranks an
  allocation slogan in measured hot loops.
- Tracing is a typed lazy product seam. Disabled probes neither format nor allocate; exporters are
  server-only, bounded, batched, and report health without owning data-plane work.
- Controllers commit coherent passing increments, keep a current salvage ledger, and return one exact
  candidate commit plus raw gates. They do not merge another controller's branch or weaken a law to
  finish overnight.

## Dependency waves

### Wave A — shared substrate

1. **Integration, foundation, and data-layout controller**
   - Reproduce and integrate the P1 borrowed canonical-root mechanisms on current identity authority.
   - Bind a request once at a borrowed generation boundary; remove later provider/request equality
     checks; carry one non-forgeable verified capability to its first real consumer.
   - Finish canonical-byte-first root ownership and packed object partial authenticated views.
   - Reproduce the preserved performance controls, compare safe layouts and allocators, and retain
     only measured winners. Own Nix/toolchain/Dylint fresh-cache closure.
   - Terminal: canonical bytes -> borrowed root/range views -> verified selected bodies -> immutable
     local store, with 100,000-row peak-live-byte/copy/allocation evidence and clean current gates.

2. **Durability, transport, and placement controller**
   - Extend the accepted single-owner journal with bounded MPSC submission, reusable group-commit
     buffers, exact receipt fan-out, cancellation/poison/shutdown, immutable publication records, and
     CAS head release.
   - Implement runtime-independent leased range streams with explicit complete/partial/degraded/
     cancelled/failed terminals and deterministic wake/reorder/credit proofs.
   - Provide the same immutable object/range semantics through memory, file/NVMe, and object-store
     adapters; authenticated sparse fetches name absent ranges exactly.
   - Terminal: verified generation -> append/group commit -> stable receipt -> published authority ->
     crash-safe reopen -> leased sparse fetch, under every-prefix fault schedules.

### Wave B — compute and retrieval planes

3. **Compiler and compact semantic IR controller**
   - The existing P5 continuation is authoritative until closure; no duplicate writer starts.
   - Complete typed recipes, compact borrowed IR fragments/manifests, static frontend dispatch,
     vertically credited execution, publication, diagnostics, and changed-fragment reuse.
   - Supply real adapters for at least four language families selected by reproducible local tooling;
     record exactly which semantics each adapter proves rather than pretending parity.
   - Terminal: a deterministic offline corpus of at least 200 packages, spanning all selected
     languages, compiles through real frontends into validated immutable fragments and manifests;
     unchanged fragments are reused and changed-only publication is proven.

4. **Immutable index and Tantivy controller**
   - Build sealed deltas into typed exact and lexical segments; publish immutable snapshots and
     compaction-equivalent replacements; execute borrowed bounded queries with exact partial/degraded
     terminals under stale routing and node loss.
   - Tantivy is a nested lexical build/query adapter, not the logical schema or truth. Prove snapshot
     pinning, deterministic merge/ranking, deletion/update semantics, segment reuse, range warming,
     and local/remote result equivalence.
   - Terminal: published IR deltas from the package corpus become searchable immutable snapshots;
     exact and lexical results survive compaction, rebalance, stale routes, and one unavailable node.

5. **Async graph and vector controller**
   - Project published IR/index facts into a typed async Trustfall query seam with bounded leased
     batches, cancellation, backpressure, provenance, and exact absent-partition reporting. Runtime
     types stay in adapters.
   - Implement Qdrant-backed vector segment build/query as a replaceable projection over immutable
     snapshot identity. Batch ingestion, quantization/filter choices, payload shape, transport, and
     local fallback are measured; Qdrant never becomes canonical truth.
   - Terminal: the same pinned package snapshot answers graph and vector queries locally or remotely,
     with deterministic typed terminals and fault/pressure evidence.

### Wave C — local-first product

6. **Adaptive local-first capability controller**
   - Combine typed local facts, current demand, remote health, latency, battery/memory/storage credits,
     and capability-bundle availability into deterministic placement decisions.
   - Demand-selected headers/ranges move RAM <-> NVMe <-> object storage without semantic change;
     remote inconsistency expands proven local capability and recovery contracts it safely.
   - Base-client artifacts remain below 50 MB in measured release builds. Optional analyzers,
     compilers, codecs, models, and server adapters are hash-pinned bundles acquired conditionally.
   - Terminal: deterministic demand/recovery traces prove expansion, bounded overload, eviction, and
     replay without shadow mutable truth or server dependency leakage.

7. **Unified CLI, MCP, and GPUI controller**
   - Define one closed typed application command/query service over the accepted compiler/index/
     graph/vector/locality capabilities. CLI, MCP, and GPUI are thin adapters; none reimplements
     business logic, validation, streaming, terminals, or error translation.
   - Build a real GPUI interface for package generation, snapshot status, search, graph/vector
     results, local/remote health, and bounded progress streams. This stage must not invoke
     accessibility automation or perform unbounded/global filesystem searches.
   - Terminal: the same golden command/query corpus yields semantically identical typed results and
     diagnostics through in-process API, CLI process, MCP protocol, and visible GUI state.

### Wave D — system closure

8. **End-to-end proof and performance controller**
   - Own no duplicate product logic. Compose the public seams in ordinary top-level integration
     tests and benchmark-capable fixtures.
   - Exercise 200+ multi-language packages through acquire/compile/IR/publish/index/Tantivy/Trustfall/
     Qdrant/query and all three interfaces. Include restart at durable prefixes, remote outage and
     recovery, stale routing, cancellation, bounded overload, mutation, corrupted ranges, and
     deterministic replay.
   - Capture peak live bytes, allocations by lifetime, copies, scans, branches, queue occupancy,
     atomic traffic, first/warm latency, throughput, release binary size, and typed OTEL correlation.
     A benchmark is never an assertion unless it has a calibrated environment and explicit bound.
   - Terminal: one reproducible Nix entry point runs the complete correctness suite offline; optional
     network/service suites are explicitly gated and provision their own local dependencies.

## Integration order and stop rules

Controllers may prototype in parallel, but the shared branch integrates in dependency order: Wave A,
then compiler, index, graph/vector, adaptive client, interfaces, and system closure. Before each merge,
the chief records candidate tree, public terminal, novel chief attack, affected gates, retained and
rejected mechanisms, and unresolved platform evidence. Any overlap is resolved by selecting the
strongest single invariant owner and refactoring consumers; parallel branches do not create API
compatibility obligations.

The overnight run is successful only if every terminal is either integrated or represented by an
honest `EVIDENCE_BLOCKED` receipt after two distinct bounded attempts. Time elapsed, agent completion,
test count, and line count cannot convert missing evidence into closure.
