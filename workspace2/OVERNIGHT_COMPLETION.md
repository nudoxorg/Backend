# Overnight completion program

This is the execution ledger for taking the greenfield workspace from its current foundation to one
end-to-end product proof. It supersedes the prototype-only portfolio and the failed evidence-first
overnight run. Prototype branches and task transcripts remain design and attack input, but progress
is measured only by integrated code, a newly red falsifier, or a reproduced product gate.

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
| Thousands of lines of plans and skills were reread before the first red test. | Read the common idiom skill plus the narrow owning skill. Read supplemental material only when a named uncertainty appears. The first cycle ends in a red test or production edit. | Preserve existing research notes as searchable references; do not replay their reading ceremony. |
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
- After two cycles without a progress receipt, reduce scope to the smallest end-to-end vertical
  slice and implement it directly. Do not write another packet.
- Review returns one ranked patch-or-reject report. A new case strengthens the owning abstraction
  and its test; it does not restart planning.

## Shared laws

- `canonical` is the merge center. Isolated implementation worktrees start from its recorded commit;
  only the designated integration steward writes or merges in the canonical checkout. Never clean,
  reset, or discard unclassified canonical changes.
- Read `ORCHESTRATION.md`, `ROADMAP.md`, `TESTING.md`,
  `.codex/skills/steward-greenfield-rust-program/SKILL.md`,
  `.codex/skills/deliver-reviewed-rust-slice/SKILL.md`,
  `.codex/skills/manage-rust-swarm/SKILL.md`, and the named domain skills completely.
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
actual-exported-rlib surface probe. It is not structural semantic-IR closure: products, atoms, pooled
lists, external references, hash-consing, and permutation-stable canonical bytes remain red.

| Wave | Current canonical state | Next executable terminal |
| --- | --- | --- |
| A1 foundation/object | Partially integrated | Add the smallest authenticated owning-range witness and wrong-artifact header/directory/body attacks while preserving rejected owner/store state. Repair the disposable-`TMPDIR` Dylint UI path as a separate tooling defect. |
| A2 durability/transport | Partially integrated | Put bounded nonblocking admission and reusable group commit around `FileJournal`; prove exact `StableReceipt` fan-out before publication or leased transport. |
| B3 compiler/IR | C0 plus C1 wire/borrow foundation integrated | Add the first hash-consed product/atom representation and a permutation-canonicality falsifier, then extend to pooled lists and external references. |
| B4 immutable index | I0 vocabulary only | Implement one borrowed immutable manifest plus exact segment/query pinned to `IndexSnapshotId`; add lexical/Tantivy only after that slice is green. |
| B5 graph/vector | Absent; an external candidate is under decomposition | Separate vocabulary, faults, admission, graph, vector, and adapters; do not import the current mixed-responsibility query module wholesale. |
| C6 adaptive local-first | Absent | Implement the smallest deterministic `next_action` over bounded `FactKey` inputs after the graph/vector seam is real. |
| C7 application/interfaces | Absent | Build one in-process typed command/query path; add CLI, MCP, and GPUI only as thin consumers. |
| D8 system closure | Absent | Add ordinary cross-crate integration coverage as each real seam lands; the 200-package terminal remains deliberately unclaimed. |

No wave is externally blocked. A missing reviewer/model route is a controller fallback, and the
reproduced Dylint temporary-path defect is actionable tooling work rather than an excuse to stop a
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

The program is successful only when every terminal is integrated. `EVIDENCE_BLOCKED` is an honest
temporary state, not a successful terminal: it requires two distinct implementation attempts against
the same reproducible product, authority, external-system, or toolchain blocker. Reviewer/model
availability, packet format, elapsed time, agent completion, test count, and line count cannot create
a block or convert missing implementation into closure. Every blocked row retains its best candidate
and exact next executable action.
