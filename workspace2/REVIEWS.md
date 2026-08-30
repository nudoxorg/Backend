# Review ledger

No swath is complete until its score reaches eight with no cap. A high weighted mean does not bypass
a cap. Every finding records observable evidence, the violated invariant, the smallest corrective
action, and whether the job skill needs a reusable rule.

## Review order

1. Reproduce the agent's commands from a clean invocation.
2. Run `bash tools/quality.sh`.
3. Read every public type and every state transition before implementation details.
4. Search for allocations, copies, panics, hidden scans, erased dispatch, unbounded retention,
   duplicated vocabulary, and configuration masquerading as types.
5. Mutate/truncate untrusted inputs and generate invalid transition sequences.
6. Verify complexity and memory claims with counters or benchmarks.
7. Attempt to delete types, traits, generic parameters, states, conversions, and owned intermediates.
8. Score every weighted dimension and apply caps.
9. Update the relevant skill only when a finding generalizes beyond the specific line.
10. Resume the same compacted Terra agent with the exact failing evidence and required invariant,
    without prescribing a needlessly narrow patch.
11. Reject accessor ceremony over valid public fields, `.get()`-heavy transparent scalars, erased
    error causes, one-letter public generics, implementation-heavy crate roots, and large branchy
    functions whose unnamed transitions cannot be tested independently.
12. Reject unit planner/manager/factory namespace objects while retaining empty semantic marker types
    that prevent generic-domain mixing. Attempt to replace every namespace object with a free function.
13. Trace every layout number to one typed authority. Reject anonymous width sums, copied offset
    arithmetic, bespoke `.wire()` methods, and handwritten ordinary error formatting/source plumbing.
14. Audit every `Box`, `Vec`, and `Arc` by backing bytes, allocation point, maximum capacity, ownership
    lifetime, and rejection path. Attempt borrow, caller scratch, or small proven inline storage first.
    A self-referential helper must eliminate a measured copy in an exercised path to survive review.
15. Count conditions as well as lines. A helper that only relocates four repeated `if` branches does
    not simplify the model; a typed descriptor/table/constructor must centralize the invariant while
    retaining exact diagnostic values.
16. Stress every abstraction with the next two plausible cases. Reject it if extension requires a
    semantic boolean, nullable catch-all, backend leakage, unrelated generic responsibility, or a
    caller promise. Redraw the owning crate/type boundary instead of weakening the original law.
17. Search public errors and iterators for `Internal`, `Unexpected`, defaults, `.ok()`, `filter_map`,
    and release-only `debug_assert`. A validated boundary must absorb impossible states rather than
    asking ordinary callers to handle implementation corruption or silently shortening output.
18. For fixed binary records, attempt to delete every raw offset/read/write helper in favor of one
    endian-aware typed wire declaration borrowed on read and instantiated on write. Reject a parser
    framework that adds I/O/materialization or solves decoding while leaving a mirrored encoder.
19. Reject custom `from_bytes`/`from_slice` and fixed-length error types when standard `From`/`TryFrom`
    express the complete invariant. Domain tags come from one typed declarative registry, not repeated
    hand implementations.
20. Treat `is_err`, variant `any`, pointer-size comments, and one happy path as weak smoke tests. A
    strict test names exact ordered facts, exact causal error data, rollback/conservation, and negative
    space.
21. After a hard bound or linear token proves a conversion, capacity, or state transition, reject a
    second fallible check and its impossible public error. Consume the proof directly. If safe Rust's
    container API cannot express the proof, redesign the representation before adding `Internal`,
    silently dropping a value, or fabricating a user error.
22. Treat hash preimages as binary protocols. A branded hasher prevents cross-domain reuse but does
    not make an arbitrary sequence of `update(&[u8])` calls unambiguous. Structured identities consume
    typed canonical records/transcripts; arbitrary chunk streaming is reserved for domains whose
    semantics really are an undelimited byte stream.
23. Reject runtime policy branches in high-frequency generic infrastructure when a descriptive type
    parameter can monomorphize the choice. In particular, a bounded flight recorder has O(1) append,
    overwrite, and iteration setup; moving all retained events for each new observation is a failure.
24. An incremental builder has an explicit capacity and returns the rejected input on exhaustion.
    `try_reserve(1)` on every push is allocation-aware but still unbounded and is not acceptable in a
    portable core.
25. Do not trade hot-loop branch prediction for a smaller setup allocation without measurement. A
    resident row must not perform two sparse binary searches. Prefer one compact routing index consumed
    by one random lookup or a monotone scan cursor; report index bytes, comparisons, and route bias.
26. Reject positional tuples for heterogeneous counts or transition facts. Fields have semantic names,
    and increments happen through the constructor/transition that proves the accepted fact. A third
    case must extend an exhaustive enum/record rather than reinterpret `.0` or `.1`.
27. Reject stringly scenario and reporting fields. Steps, expected outcomes, observed outcomes, fault
    sites, and recovery phases are closed enums or structured domain values. Use Strum selectively to
    derive iteration/count/string mechanics; do not use it to bypass canonical protocol ownership.
28. Review admission by drawing the ownership graph. If the code acquires several capabilities and
    then contains a family of compensating rollback helpers, replace it with named linear bundles and
    narrow consuming transitions before discussing source shape.
29. Configuration arithmetic has one typed authority. Index load factor, minimum bucket count,
    rounding, mask, and supported target bounds become one validated geometry value; allocation and
    probing consume it without repeating arithmetic or manufacturing impossible overflow states.
30. Async readiness is a concurrency protocol. Draw the observation/register/arm/recheck/notify/drop
    interleavings, identify waiter generation and reclamation, and test multiple waiters with different
    byte demands. A `Pending` enum plus one waker cell is a lost-wake and starvation bug, not a seam.
31. When Clippy reports a large result error, draw the ownership overlap before boxing. Remove fields
    already carried by rejected input, compress snapshots to the failed semantic axis, and size-test
    the result. Heap allocation is not an error-model abstraction and is especially hostile on an
    exhaustion path.
32. Track memory by simultaneously live owners. A borrowed selection plus copied plan descriptors, or
    canonical input facts plus packed rows plus hierarchy scratch, is charged at peak overlap. Drop
    construction inputs at the earliest proof boundary and let returned views retain source borrows
    when that deletes proportional copying.
33. Self-reported allocation counts are telemetry, not proof. Validate zero/one/inline/spill/large
    cliffs with an independent counting allocator or profiler, and do not retain derivable allocation
    class/count fields in every hot owner merely to make a test pass.
34. Never turn a proof-impossible queue-full result into destructive `force_push`. A spare cell for the
    largest work type wastes bounded memory; displacement silently destroys linear work if the proof
    is ever wrong. Prefer permit-addressed payload slots and a compact modeled readiness publication.
35. The event type already names a single operation. A one-variant `StoreOperation`,
    `HydrationOperation`, or `RootOperation` plus an event field is a namespace object and removable
    state, not future-proof observability.
36. Layout review measures total live state and access, not only `size_of`. Inventory stack/heap
    backing, padding, indirections, capacity cliffs, cache-line scans, branches, atomics, code size,
    client/server profiles, and drop/cancellation. Unsafe lab candidates need a safe reference,
    written proof, differential corpus, and applicable Miri/sanitizer/Loom evidence.
37. Test code must be compressed to the semantic contract. Use `rstest` dimensions, typed fixtures,
    reusable drivers, and small evidence records; setup/poll/join/error-report plumbing belongs behind
    named helpers. A monolithic cross-crate `Result` scenario is unreadable even when it avoids panic,
    and helper abstraction never excuses weak or inexact assertions.
38. Test Ragel/Colm against the actual machine before adoption. Ragel may generate a regular private
    control coordinate only when deterministic Rust output beats the typed enum baseline and host
    actions retain payload/error invariants. Colm's C/GCC runtime is compiler-tool territory, never a
    portable-client dependency; atomic state machines remain Rust plus Loom memory-order proofs.
39. Audit every primitive by semantic unit. Use transparent newtypes plus standard traits when they
    prevent mixing offsets/counts/ordinals/credits/epochs/capacities and delete conversions or branches.
    Reject both primitive soup and decorative wrappers whose only result is `.get()` ceremony; valid
    aggregate facts remain direct fields and typed projections have semantic names.

## Round 0 — scaffold

| Swath | Score | Cap | Evidence |
|---|---:|---:|---|
| Foundation fabric | 1.0 | 2 | Green crate scaffold; no implementation or tests. |
| Object hydration | 1.0 | 2 | Green crate scaffold; no implementation or tests. |
| Operation/runtime | 1.0 | 2 | Green crate scaffold; no implementation or tests. |

## Round 1 — vertical slice

| Swath | Score | Cap | Evidence |
|---|---:|---:|---|
| Foundation fabric | 5.7 | 6 | Typed identities and zerocopy frame records landed, but known-section storage leaked proof, layout/errors were broad, modules/tests were weak, and downstream raw hash domains remained possible. |
| Object hydration | 5.9 | 6 | Semantic root/locality split and bound demand landed, but allocation proofs, packed-coordinate handling, store invariant leakage, and adversarial/model evidence were incomplete. |
| Operation/runtime | 5.5 | 4 | Bounded CAS credits and a real operation existed, but manual panics, invalid workflow combinations, erased return errors, incomplete delayed replay, and no async/observability slice triggered the error-handling cap. |

## Round 2 — open blockers

These are acceptance findings, not suggestions. Scores remain provisional until a clean global run.

### Foundation fabric — provisional 5.8/10, cap 6

- `nudox-frame/src/encode/layout.rs` and `nudox-view/src/validate/{header,directory/record}.rs`
  retain conversions and arithmetic failures made impossible by the already-checked 64-section,
  one-megabyte, 32/64-bit target bounds. This adds branches and false public states after proof.
- `nudox-view/src/validate/directory/span.rs` recomputes `end - offset` on the outside-frame path and
  labels the impossible subtraction failure as addition with misleading operands.
- `nudox-id::Domain` and `Encoding` remain downstream-implementable while the architecture requires a
  central collision-reviewed registry. `ContentHasher::update(&[u8])` remains an untyped structured
  transcript escape hatch.
- Schema offsets are still a hand-maintained cumulative authority beside the actual zerocopy record;
  they must derive from `offset_of!`/`size_of::<Record>()`.
- Twenty focused tests are useful smoke evidence, not the required exhaustive truncation/field
  mutation/version/compatibility matrix. The Bolero target mutates only four selected fields.

### Object hydration — provisional 4.9/10, cap 6

- `nudox-object/src/residence.rs` retains the exact trivial `RemoteBase` getters already rejected by
  the user, and an unused generic `Residence` algebra duplicates the root locality vocabulary.
- `GenerationRootBuilder::new + try_push` grows without a declared maximum. Allocation failure is
  preserved, but demand is not bounded and rejected entries are not returned.
- `nudox-root/src/encode.rs` implements canonical row serialization twice and maintains manual width
  sums instead of streaming one typed canonical record into both destinations.
- Overlay propagation peaks at marks plus a full facts vector plus two final sidecars, then sorts and
  validates facts it just produced. There is no caller scratch or peak-allocation evidence.
- `MemoryStore` exposes `IndexExhausted` and defensive found-index/insert failures that should be
  impossible under its single-owner, preallocated half-load invariant. Capacity errors conflate byte
  and slot axes; oversized host length is rewritten as `u64::MAX`.
- `LocalityMap::locality_at` independently binary-searches promises and overlays for every access; the
  common resident scan therefore pays both searches. Tuple counter fields obscure construction. Use a
  named unified routing index plus forward scan cursor and measure the extra metadata against branches.

### Operation/runtime — provisional 3.8/10, cap 4

- Manual `expect`/panic sites remain across operation, workflow, Loom, runtime, and E2E scenarios;
  `Owner::poll_terminal` erases `CreditReleaseError` with `map_err(|_| ...)`.
- A unique `WorkPermit` still feeds fallible `SlotClaimError::{Occupied, Contended}` and dequeue emits
  `UnexpectedState`, exposing invariant bugs as ordinary user errors instead of using a linear slot
  ownership transition.
- Terminal publication precedes detection of byte/work-permit return failure, leaving an ambiguous
  visible partial-success outcome with inadequate conservation evidence.
- Durable replay did not initially accept all earlier delayed duplicates and could issue `Publish`
  without representing a durable post-start publication failure/reconciliation state.
- The first flight recorder shifts every retained event on overwrite and branches on runtime policy;
  this violates the zero-overhead/O(1) observation contract.
- Static async leased streaming, deterministic wake/cancel scheduling, the shared typed scenario
  driver, and the isolated OTEL adapter are not yet integrated, so the vertical slice is incomplete.

## Round 3 — representation and async review

### Foundation fabric — provisional 6.2/10, cap 4

- `Domain: Debug` hides the irrelevant marker bound the generic APIs were told to remove. Formatting
  is not an identity-domain capability; derive/manual generic formatting must avoid that bound.
- `ValidateError::{HeaderCast, DirectoryCast}` publicly expose zerocopy failures made impossible by
  exact slice width plus `FromBytes + Unaligned`. This violates the validated-boundary law even though
  comments call them unreachable.
- `ContentHasher::write` remains a public arbitrary field-sequencing surface for structured root,
  dependency-set, and stage-key identities. Private downstream writers reduce accidental misuse but
  do not make one canonical transcript constructible at the type boundary.
- `SchemaId`/`OperationId` still mirror repr discriminants through handwritten `From`, `TryFrom`, and
  display matches. The closed registry has enough derive support to delete this duplication.
- The layout audit measures each BLAKE3 hasher at 1,920 stack bytes. That is not automatically wrong,
  but nested construction-stack/high-water and streaming-chunk API effects are unmeasured.

### Object hydration — provisional 6.1/10, cap 6

- `CoverageSidecar` is 16 bytes for every selected row and repeats providers already borrowed from
  locality. A sparse ordered `u32` absent-ordinal plan makes present rows cost zero retained bytes and
  fetch traversal proportional to actual misses; dense-bit alternatives require density-cliff tests.
- Overlay still retains marks plus a full `Vec<LocalityFact>` before sorting/re-searching the same root
  and allocating final route/payload arrays. Canonical direct count/fill construction is still open.
- `RootDiff::next` takes both optional heads and restores one through a four-way ownership ladder; it
  also lacks the changed-only stream needed for million-unchanged roots.
- Store allocation tests trust retained `metadata_allocations`, exact store size is only `<512`, and
  `InlineOrHeap::push` can panic through `ArrayVec::push` if its commented proof drifts.
- Store index duplicates the foundation routing-word projection. Test reporters still box root/store
  rejections. Probe events retain removable one-variant operation namespaces.

### Operation/runtime — provisional 5.0/10, cap 4

- Epoch-exhausted work slots were initially returned to the free bitmap and could fail forever. The
  fix now retires the coordinate and lets other slots progress, but retirement/conservation still
  needs bitmap-level modeling under races.
- The replacement `ReadyQueue` allocates `capacity + 1` full payload cells and calls `force_push`.
  Its debug-only displacement check means an invariant bug silently drops owned queued work in release;
  it also charges an unused cell sized for arbitrary `Work`.
- Layout evidence: `Runtime<u64,u64>` is 768 bytes/aligned 128; its ready queue header is 384 bytes.
  A waiter is 40 bytes and 64 waiters span 2,560 bytes/40 assumed cache lines. Wake-all scans every
  record even with zero/one active waiter; active indexing/SoA/bucket alternatives are unmeasured.
- `AdmissionFuture` represents phase with correlated pending/ticket/ready Options. Raw waiter status
  constants and ignored `ArmResult` leak the state matrix despite new Loom coverage.
- Operation setup and source poll still share one error type, local poll advertises impossible stale
  request errors, batch uses `items()`, and promised absence drops its exact ProviderSet.
- Workflow events are 68 bytes and a per-key log repeats the 32-byte key plus repeated 32-byte output.
  The accepted durable representation has not been separated from conflict-bearing input events.
- Flight recorder pays an `Option` tag/padding per event and a duplicate length field. The E2E scenario
  remains monolithic and broad `Invariant { step }` errors omit exact expected/observed conservation.

### Cross-workspace layout audit — provisional 4.0/10

- The generated baseline and exact private runtime offsets are reproducible and useful, but they are
  discovery rather than optimization evidence.
- The audit is not complete until competing hydration, waiter, ready-slot, recorder, workflow-log, and
  store-allocation representations are implemented and tested across the declared distributions.
- No unsafe candidate has yet supplied the required reference deficit, proof, Miri/sanitizer/Loom
  evidence, or end-to-end win. Hypotheses alone cannot score above four.

## Round 4 — ownership and proof-boundary blockers

### Foundation fabric — provisional 6.4/10, cap 4

- `nudox-view::Sections::next` still contains a production `assert_eq!` when its retained known count
  disagrees with a repeated directory scan. The immutable input cannot change, but a validator/view
  contract regression would panic rather than remain structurally impossible. Retain the exact known
  descriptor coordinates selected during validation; iteration must not rediscover them or expose an
  internal error.
- `validated_section` repeats four unchecked proofs for each yielded section: descriptor cast,
  compatibility, bounded row count, and body range. A compact validated witness can pack the at-most
  three known descriptor coordinates and resolved kinds into the otherwise padded 24-byte view,
  leaving at most one audited immutable body reborrow. Compare that shape with cached compact spans;
  do not optimize the witness header while adding hot revalidation and unsafe surface.
- Miri has not run because the selected nightly/sysroot combination is incompatible. That is an
  evidence gap, not a code failure, but the handwritten-unsafe score cap remains until an applicable
  Miri or sanitizer job exercises the proof boundary.

### Object hydration — provisional 5.8/10, cap 5

- `LocalityMap` still owns independent boxed route, promise, and overlay slices, constructed by three
  `Vec`s and a fourth input-fact `Vec`. This is an eagerly materialized native projection of an
  immutable artifact. The primary representation must instead be a validated borrowed view over
  caller, pooled-lease, mmap, or range-fetched bytes; an optional owner is one generic byte-storage
  adapter, not domain state.
- `LocalityRoute` stores a tagged payload coordinate even though exceptional-row membership yields an
  exception rank, the class bitmap yields promise/overlay rank, and overlay-presence rank yields the
  present-object coordinate. The redundant coordinate costs bytes and permits cross-lane mismatch.
  Replace it with a monotone exceptional-row set plus semantic bit/value lanes whose cardinalities are
  jointly validated.
- `OrderedLocalityBuilder` stages all three final inventories before converting them to boxed slices.
  Preparation must preflight exact region geometry and output-too-small before mutation, then stream
  canonical lanes into caller output and the typed identity sink. A convenience allocation may make
  one exact artifact allocation outside the borrowed core.
- The initial artifact sketch grew past one thousand lines in one file while the existing locality
  implementation remained live. It is not reviewable or acceptable as a parallel fallback. Split
  header, layout, row-set policy, preparation, writer, validator, semantic view, cursor, and
  adversarial tests; then migrate consumers and delete the old owner.

### Operation/runtime — provisional 6.0/10, cap 6

- Static accounting is now measurable and honest: default `NoopAccounting` adds zero bytes or history
  atomics, while opt-in `AtomicAccounting` adds 64 bytes and explicit historical queries. This closes
  the prior fake-zero-metrics finding but not the runtime swath.
- Runtime tables and queues still need monomorphic const-inline, caller-arena, and dynamic-remote
  storage policies with 1/4/64-slot and large-`Work` evidence. Accounting policy success does not
  justify `Vec`/`VecDeque`, spare full-work cells, or hot storage enum dispatch elsewhere.
- `WorkflowLog` remains an in-memory `Vec` with a durability name. Durable execution requires a fixed
  canonical record, pure preparation, async append returning a commit witness, streaming replay, and
  exact persisted failure identity. The vector is only a test adapter.
- Flight recording must prove O(1) wrap/drop and exact iteration under zero capacity, wraparound, and
  destruction. A silent failed index becoming `None` or `DroppedNewest` falsifies both diagnostics and
  `ExactSizeIterator` and is a blocker, not graceful degradation.

## Round 5 — canonical seam and active-cell review

### Foundation fabric — provisional 6.8/10, cap 6

- `ValidatedFrame` now retains the three known descriptor coordinates in a 24-byte view and iterates
  them with a closed one-byte cursor. This deletes the production assertion and repeated directory
  search. Exact remaining-length tests pass, and matching nightly Miri passes all 12 frame plus 31
  view tests over the unchecked descriptor/body/row-count proof seam. The unsafe-evidence cap is
  lifted; missing comparative layout/compute evidence still caps the performance claim at six.
- `FixedCanonicalRecord<N>` and `ContentHasher::write_record` remove the object descriptor's copied
  stack encoding. The foundation job is not closed while public `ContentHasher::write(&[u8; N])`
  lets application crates construct untyped transcripts; it becomes private after root, hydration,
  and workflow migrate to declared records.
- `LocalitySortedEncoding` is registered centrally. The locality layout laboratory must now measure
  the actual rank-directory artifact, including directory bytes and scalar/SIMD rank work, rather
  than only the rejected three-owner baselines.

### Object hydration — provisional 6.0/10, cap 5

- The 46-byte object descriptor is now one zerocopy record used for direct output, borrowed decode,
  and typed hashing without a second array. This is a verified seam, not completion of the swath.
- The old `LocalityMap` with three boxed slices and its three-Vec builder remain live. The agent ending
  a checkpoint while naming this as a future/global target was rejected and compactly resumed on the
  atomic replacement. The only accepted core is prepared immutable facts, one exact caller output,
  one checked layout, and a borrowing validated view; no compatibility fallback survives.
- `DependencySetWriter` still sequences root, projection tag/range, and count through raw fixed byte
  writes. `Projection::tag()` is a bespoke wire accessor. Complete and range forms need declared
  canonical records; the public arbitrary-write seam cannot remain an application protocol.
- `nudox-root::encode` still uses a one-line sink trait plus `HashSink`/`VecSink`, making a growable
  vector the convenience grammar. Root header/row records must themselves be the shared fixed records,
  and explicit output must be measured caller storage.

### Operation/runtime — provisional 6.2/10, cap 4

- The untagged union tied the safe tagged enum at 72 bytes and was correctly deleted. Payload and
  terminal reuse one permit-addressed safe-enum cell; the packed handle remains two words. Ordinary,
  Loom, and matching-nightly Miri suites cover the current transition, but the strict all-feature
  Clippy gate still fails and therefore no runtime checkpoint is accepted.
- Owner dequeue is now one AcqRel swap racing cancellation. However, a first no-panic correction
  silently mapped failed `NonZero` construction to `MIN`, potentially aliasing a broken handle to
  slot/epoch one. Proof-bearing arithmetic must construct the niche without a default. A readiness
  bitmap/payload-class mismatch is currently restored to another bitmap and returned as Idle/None
  without an exact fault fact; recovery cannot erase the diagnostic.
- `FlightRecorder` still retains `[Option<Event>; CAP]`, duplicate length state, conditional indexing
  that can silently discard, and an iterator whose `ExactSizeIterator` promise depends on every slot
  being `Some`. The requested dense initialized prefix/ring representation and adversarial drop/wrap
  evidence have not landed.
- `StageKey::derive` manually maps an operation wire code, calls bespoke stage conversion, writes a raw
  domain tag and primitive arrays, and wraps the resulting typed content ID in another 32-byte ID.
  Replace this with one declared fixed record and the existing branded identity.

## Round 6 — const authority, control flow, and reconstruction cost

### Foundation fabric — provisional 7.3/10, cap 7

- Raw frame mutation is now split by invariant boundary and driven by compile-time header/descriptor
  field tables. One const checker proves contiguous coverage; the runner consumes those field ranges
  directly rather than searching them through fallible delegation. Ordinary tests exhaust all 256
  one-byte values across 41 structural bytes (10,496 validations), while ordering, every truncation,
  and correlated multi-byte boundaries run once. The isolated foundation lint/test gate is green.
- The next performance cap is measured selection, not correctness: the layout lab must distinguish
  sparse exception-row membership from promise-class rank and overlay-presence rank, then report full
  artifact bytes and work across actual densities. Public raw structured-hasher writes remain until
  downstream root/dependency/workflow records migrate.

### Object hydration — provisional 6.5/10, cap 5

- The rejected three-owner locality map has been replaced in the active root crate by one exact caller
  byte artifact, a 16-byte borrowed validation witness, a 24-byte semantic layout, sorted u32 exception
  rows, and omitted-zero /256 rank prefixes. Validation and prefix construction are linear rather than
  quadratic; random rank touches at most four words and sequential cursors carry payload ordinals.
- The 16-byte witness currently buys size by repeatedly decoding/copying the 48-byte header and
  rebuilding an 80-byte lane table: multiple times per random lookup and nearly every cursor action.
  Header/descriptor fields also have parallel manual ser/des beside their wire records. Acceptable
  completion needs one zerocopy header authority, one lane derivation per lookup/scan, and measured
  16/24/32-byte witness alternatives.
- Direct writing still clears rank lanes and makes a second popcount pass over membership. Because
  sorted emission already knows cumulative promise/present counts at every /256 boundary, prefixes can
  be written during the first pass. Layout setup also duplicates formulas and fabricates a Header/
  u32::MAX overflow when a host conversion fails; one u64 geometry plan must retain the exact source.

### Operation/runtime — provisional 6.3/10, cap 4

- The safe enum and single-swap cancel/dequeue transition are directionally sound, and the runtime
  crate root is now a 65-line module map. The actual all-target/all-feature strict gate reports 36
  failures: duplicate initialized-prefix unsafe machinery in storage/waiters, `mem::forget`, multiple
  unsafe operations per block, raw casts/defaulted niches, type-complexity leaks, and repeated Loom
  panic allowances. The checkpoint remains rejected regardless of passing unit/Loom/Miri counts.
- Waiter, async admission, and runtime test files remain 818/515/662 lines. They require splits by
  state/invariant, one reusable initialized-arena boundary, and declarative test scenarios before work
  moves to the dense recorder, durable workflow log, or executable OTEL adapter.

## Review finding format

```text
Severity: blocker | major | minor | polish
Evidence: path:line, test, trace, allocation count, or benchmark
Invariant: exact contract that fails
Why it matters: user-visible/correctness/resource consequence
Smallest sound resolution: capability required, not a dictated diff
Skill update: none | exact reusable rule added
Status: open | verified
```

## Round 7 — final cross-crate compression

### Verified resolutions

- The native three-owner locality projection is gone. This historical round measured a 136-B
  geometry-caching witness. The current replacement is a 200-B typed-lane witness with infallible
  traversal: one artifact-global content authority replaces repeated per-descriptor authority bytes,
  and trusted reads carry no drift-error cascade. The retained historical `fearless_simd` result was
  10.18× scalar at 16,384 rows with +328 B text and no release-file growth.
- Runtime admission is `no_std + alloc`, monomorphized over heap, const-inline, or caller-arena
  storage. `Arc`, mutex queues, duplicate terminal storage, rollback bundles, and no-op policy structs
  are absent from production. Work and terminal occupy the same permit-addressed cell. Nine Loom
  models and three matching-nightly Miri lifecycle tests cover publication, ABA retirement,
  wait/cancel/reuse, partial initialization, terminal reuse, and drop.
- `Probe<Event> for ()` and `Accounting for ()` are the disabled policies. The dense `ArrayVec` flight
  recorder has O(1) overwrite and exact destruction/order tests. The nested OTEL adapter proves three
  correlated spans, 15 globally ordered typed logs, seven aggregate gauges, disabled-builder
  laziness, bounded overload, exporter-failure attempts, and shutdown flush without entering the core
  dependency graph.
- Workflow durability has one 68-B typed canonical record, an associated concrete append future,
  commit-before-effect ordering, and fallible streaming replay. `MemoryWorkflowLog` is named and
  documented as the bounded in-memory adapter, not durable infrastructure by assertion.
- E2E broad `Invariant { step }` failures are gone. Each transition reports its exact expected and
  observed typed fact or retains the original source and rejected owner. Operation batches are
  lending borrows and every source terminates explicitly before fused exhaustion.
- The final quality tier checks formatting, ordinary and all-feature strict Clippy, workspace tests,
  dedicated Loom, all-feature rustdoc, dependency exclusions, exact unsafe-module allowlisting, and
  nested adapter gates. The layout inventory compiles against the final public API.
- The root closure pass rejected one false compile-fail lifetime proof after Miri rustdoc showed it
  compiling successfully. The replacement keeps the arena runtime live beyond the shorter work
  borrow and now fails under both ordinary and matching-nightly rustdoc. The same pass split the last
  oversized workflow test into small transition-law tables; no complexity suppression remains.

### Honest remaining caps

- `GenerationRoot` still owns one `Box<[RootRow]>` and its convenience builder stages a `Vec`; a
  generic canonical-byte owner/view has not yet displaced that final native materialization.
- Store metadata has measured static heap and inline policies, but no caller-arena backing. The
  default payload owner remains `Box<[u8]>`, though every API is generic and returns the exact owner.
- Runtime correctness evidence is strong, but the dense waiter scan has not earned replacement by an
  active bitmap/bucket under sustained hardware contention and cache/false-sharing measurement.
- Index/catalog, compiler fleet, object-pack/range transport, and GUI are intentionally later swaths.
  Overall rubric score is therefore capped at 7.0 rather than claimed as plan-complete eight.
