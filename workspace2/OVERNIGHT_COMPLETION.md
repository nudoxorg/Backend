# Overnight completion program

This is the execution ledger for taking the greenfield workspace from its current foundation to one
end-to-end product proof. It supersedes the prototype-only portfolio and the failed evidence-first
overnight run. Prototype branches and task transcripts remain design and attack input, but progress
is measured only by integrated code, a newly red falsifier, or a reproduced product gate.

## August 31 migration finish

The remaining work runs as three dependency-aligned product verticals, not one controller per crate
or adapter. At most three top-level Sol tasks may run. Each Sol task may use at most two child lanes;
a Terra reviewer consumes one of those lanes. Adjacent discoveries enter the chief backlog rather
than spawning another manager. Tasks begin from the current `canonical` commit in isolated
worktrees, return coherent commits, and never merge a stale branch wholesale.

1. **Compiler, semantic IR, and publication.** Port the best mechanisms from
   `codex/multilingual-compiler-ir-a7ae`, the IR-VCS continuation worktrees, and the accepted durable
   publisher into the unified `workspace2/crates/` graph. The public terminal is real source from
   every supported language -> typed recipe -> borrowed compact IR -> stable durable publication,
   with changed-fragment reuse and structured diagnostics.
2. **Local immutable retrieval.** Port the useful Tantivy, Trustfall/S3, Qdrant, range-authority,
   mmap, and foreign-link mechanisms from `codex/waves-b4-b5-retrieval` and continuation commits.
   The public terminal is the published IR snapshot queried through exact, lexical, graph, and
   vector paths against locally provisioned services, with pinned authority and typed partial,
   degraded, cancellation, and corruption outcomes.
3. **One application and Hummingbird-style interface.** Replace lifecycle stand-ins with the real
   compiler and retrieval services. CLI, MCP, and GPUI project one typed command/query/stream model.
   The GUI must have a stable first frame, command-palette navigation, virtualized results, no
   polling animation, and Home/Libraries/Search/Connections/Settings information architecture.

Each vertical must produce executable progress before a second research tranche. Luna may perform a
bounded research experiment when the question, comparison, sources, and code/test artifact are
named; Terra synthesizes it into the next rubric row. Proof is tiered: workers run focused row gates,
Terra runs the capability gate once, and Sol/chief run the complete closure only after integration.
Repeated full-workspace, fresh-cache, Nix, Dylint-UI, or external-service gates are forbidden unless
the relevant environment or mechanism changed.

Final closure is one 200+ package multilingual journey through compile, IR, publication, Tantivy,
Trustfall, Qdrant, and the in-process/CLI/MCP/GPUI projections. It includes restart, stale routing,
outage/recovery, cancellation, overload, corruption, deterministic replay, release size, peak-live
memory, allocation/copy, latency, and throughput comparisons against the old workspace. Claims of an
order-of-magnitude improvement require a calibrated measured dimension; every other old behavior
must be matched, deliberately retired with a stronger contract, or replaced by a documented new
capability. Only after this closure is green may the chief remove the old `workspace/` and promote
`workspace2/` to `workspace/`. Protected scratch under `workspace/_patches/` and
`workspace/_worktrees/` is never part of that removal and must be preserved outside the promoted
tree first.

“Complete” means the public terminal and evidence below exist on a clean committed candidate. It does
not mean a task reports success, a crate compiles, or a happy-path test passes. Each stage has one Sol
controller. Sol controllers may commission capability-scoped Terra managers, Luna implementers, and
independent Terra reviewers according to `ORCHESTRATION.md`; role availability strengthens review but
never gates reversible implementation or promotion of an independently green candidate.

## Recovery corrections from the failed overnight run

The task transcripts exposed six repeatable sources of token waste. These are planning defects, not
product blockers.

| Observed failure | Enforceable correction | Immediate salvage |
| --- | --- | --- |
| Thousands of lines of plans and skills were reread before the first red test. | Use progressive context: Sol holds the capability graph, Terra receives one manager packet plus common/domain craft, and Luna receives assigned executable rubric rows. Load supplemental material only for a live decision. | Preserve existing research notes as searchable references; do not replay their reading ceremony. |
| A committed Phase-0 packet and source-isolated pre-edit review were treated as production gates. | Start from a compact contract and one red falsifier. Review begins after the first coherent checkpoint, when concrete code exists to attack. | Foundation, index, application, adaptive, and system packets become test inputs, not authorization tokens. |
| Model routing, reviewer custody, and exact receipt formatting caused `EVIDENCE_BLOCKED`. | Missing roles fall back to the available reviewer or controller review. Only a product dependency, unavailable external system, missing authority, or reproducible toolchain failure may block product work. | Reopen every custody-only terminal without regenerating its packet. |
| Research and packet revisions continued without intervening code or tests. | Each research tranche names the changed decision and is bracketed by a commit, red test, reproduced failure, or integration verdict. A second tranche without one stops research. | Graph/vector freezes its V17 packet; later learning lands as tests or code. |
| Circular calibration and evidence-size rules rejected working compiler code. | The worker result creates its calibration receipt. Generated LLVM, assembly, corpora, and traces are measured by bytes/digests, separate from authored-code review. | Re-evaluate C1 commits `e61f34e9`, `6be0bd2c`, and checkpoint `38662cf5` on current APIs before redesigning them. |
| Agent-thread exhaustion caused green candidates to be abandoned. | Thread exhaustion collapses review into the controller and triggers local integration; it never discards a passing candidate. | Close existing worktrees and commits before dispatching duplicate implementations. |

The terminal branch histories quantify the failure without using source-volume targets. By commit
subject, foundation produced 10 commits with no production-shaped commit; durable 16 with none;
index 6 with none; adaptive 10 with none; application 7 with none; system 3 with none; and graph/vector
reached 21 evidence commits before its first live implementation checkpoint. The compiler branch made
206 commits: 9 production-shaped, 6 test-shaped, and 191 evidence/documentation-shaped. These counts
are not quality scores. They show that the workflow repeatedly allowed multiple bookkeeping cycles
between executable changes, which the progress clock below now forbids.

### Progress clock

- A work cycle is one bounded sequence ending in a commit, a new red falsifier, a reproduced product
  failure, or an integration verdict over concrete code.
- Research may run concurrently with a cycle. It must change the next test, representation, or
  implementation decision before another research tranche begins.
- Evidence describes work already performed. It cannot authorize the start of work, substitute for
  code, or turn missing reviewer infrastructure into product closure.
- Terra keeps Luna implementing continuously while its own research stays several decisions ahead.
  After two research tranches that change no rubric row, experiment, or dispatch, that question is
  saturated and research stops.
- Review returns one ranked patch-or-reject report. A new case strengthens the owning abstraction
  and its test; it does not restart planning.

## Shared laws

- `canonical` is the merge center. Isolated implementation worktrees start from its recorded commit;
  only the designated integration steward writes or merges in the canonical checkout. Never clean,
  reset, or discard unclassified canonical changes.
- Read only the documents owned by the active role: Sol reads the capability graph and shared craft;
  Terra reads its manager packet, shared craft, and one domain skill; Luna reads its assigned rubric
  rows and linked craft sections; reviewers read the rubric, diff, consumer, and applicable review
  mode. Load broader plans or references only for a named live decision.
- Explicitly select the requested registered roles when available. Sol is `gpt-5.6-sol`/`xhigh`;
  Terra manager and reviewer are `gpt-5.6-terra`/`xhigh`; implementation workers are
  `gpt-5.6-luna`/`max`. Record task/model/effort/checkout once in the final integration receipt. A
  missing router receipt is never a reason to stop production work.
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

## Recovery checkpoint before new waves

1. The canonical steward classifies and commits the saved checkout without deleting any useful
   mechanism, then semantically merges `orchestra-shared` through its latest verified commit.
2. The recovery Sol audits existing branches by commit and symbol, not by dumping complete
   transcripts into context. It ports green mechanisms in dependency order and replays at least one
   old falsifier for every accepted mechanism.
3. Candidate priority is: compiler C1 borrowed writer/reader; foundation and object-plane closure;
   durable-journal controls; any concrete graph/vector implementation; then adaptive/index/interface
   scaffolds only where they own a real invariant. Documentation-only branches supply attacks and do
   not receive compatibility treatment.
4. Only after this salvage pass records the exact integrated baseline may new stage controllers
   start. Their first checkpoint is a thin working vertical, not a full public API proposal.

### Integrated recovery baseline — August 30, 2026

Canonical now contains the reviewed shared substrate through `70f1cbb6`, the action-first policy
through `c89ba8b8`, and the recovered C1 wire/borrow foundation through `5f99ad4d`. The C1 range is a
real allocation-free `no_std` implementation with caller-owned output, private prepared authority,
borrowed validated views, typed wire layout, recursive references, mutation attacks, and an
actual-exported-rlib surface probe. The repeatable Nix/Dylint repairs through `5555107b` pass both a
warm replay and the complete canonical quality gate. C1 is not structural semantic-IR closure:
products, atoms, pooled lists, external references, hash-consing, and permutation-stable canonical
bytes remain red.

| Wave | Current canonical state | Next executable terminal |
| --- | --- | --- |
| A1 foundation/object | Partially integrated; repeatable quality tooling is green through `5555107b` | Add the smallest authenticated owning-range witness and wrong-artifact header/directory/body attacks while preserving rejected owner/store state. |
| A2 durability/transport | Partially integrated | Put bounded nonblocking admission and reusable group commit around `FileJournal`; prove exact `StableReceipt` fan-out before publication or leased transport. |
| B3 compiler/IR | C0 plus C1 wire/borrow foundation integrated | Add the first hash-consed product/atom representation and a permutation-canonicality falsifier, then extend to pooled lists and external references. |
| B4 immutable index | Borrowed exact/lexical manifests and real Tantivy projection integrated through `ff55b02a` | Connect compiler-published deltas to immutable snapshot construction and replay compaction/stale-route journeys across the real publication seam. |
| B5 graph/vector | Borrowed graph/vector and live Qdrant projection integrated through `18197b90`; lease closure remains red | Qdrant now binds typed vector-segment provenance, rejects immutable-coordinate replacement, avoids inferred partial terminals, and passes the pinned live service. Finish the hostile lease falsifiers (dropped batch, producer wake/disconnect, nonblocking poll, genuine Loom interleavings), require a portable validated segment/receipt before publication, and integrate the real Trustfall adapter. Do not call this row complete while those counterexamples survive. |
| C6 adaptive local-first | Integrated through `691960dd` | Deterministic bounded policy, typed pins, explicit residence, resource budgets, inconsistency recovery, and safe contraction are present. Next connect decisions to live remote/index/transport health and prove an executor-visible wake stream. |
| C7 application/interfaces | Integrated through `691960dd` | One typed in-process service now drives CLI, framed MCP, and GPUI, including canonical identity parsing and typed cancellation. Next replace the local lifecycle stand-ins with real compiler/index operations. |
| D8 system closure | Partial through `7de1fb50` | A deterministic 204-package multilingual corpus and exact fault/resource oracle are present. The real compiler-to-publication-to-index-to-query terminal remains deliberately unclaimed. |

No wave is externally blocked. A missing reviewer/model route is a controller fallback. The former
Dylint temporary-path and read-only generated-header failures are closed and cannot excuse delay of a
product slice.

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
   - Project published IR/index facts through a typed async leased-batch seam with cancellation,
     backpressure, provenance, and exact absent-partition reporting. A Trustfall adapter queries an
     already-pinned resident graph view, or uses an explicitly measured bounded bridge; Trustfall's
     synchronous boxed-iterator API is not described as the async or monomorphized core seam.
   - Implement Qdrant-backed vector segment build/query as a replaceable projection over immutable
     snapshot identity. Full canonical identities live in indexed keyword payload fields; Qdrant
     point IDs are disposable physical coordinates with collision checks. Batch ingestion,
     consistency policy, quantization/filter choices, payload shape, transport, exact-versus-
     approximate provenance, and local fallback are measured; Qdrant never becomes canonical truth.
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

The program is successful only when every terminal is integrated. `PRODUCT_BLOCKED` is an honest
temporary state, not a successful terminal: it requires two distinct implementation attempts against
the same reproducible product, authority, external-system, or toolchain blocker. Reviewer/model
availability, packet format, elapsed time, agent completion, test count, and line count cannot create
a block or convert missing implementation into closure. Every blocked row retains its best candidate
and exact next executable action.
