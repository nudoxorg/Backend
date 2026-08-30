# Sol prototype-orchestrator handoffs

These are copy/paste prompts for separate Codex sessions running `gpt-5.6-sol`. They produce isolated,
committed, hostilely reviewed prototypes. They do **not** merge into `orchestra-shared`, preserve a
prototype API, issue a product score, or claim roadmap closure.

The current typed-identity-integrity and leased-range-T0 Terra cycles are already active. Do not start
duplicates of those two objectives. Prototypes below may declare those capabilities as future
integration prerequisites while using a private experimental seam on their own branch.

## Objective map

| ID | Objective | Prototype terminal | Final integration prerequisites | Principal overlap |
|---|---|---|---|---|
| P1 | Canonical root, locality, hydration, pre-publication authority | canonical bytes → borrowed view → bounded selection/hydration → non-forgeable verified fact | typed identity integrity; P2 owns any later published typestate | `nudox-root`, `nudox-hydration`, `nudox-object` |
| P2 | Durable publication heart | reduce → append/group commit → sync receipt → CAS head → crash-safe reopen | typed identity decode; later P1 published capability | `nudox-workflow`, runtime/observe, durable adapter |
| P3 | Authenticated partial object/range storage | header/directory → selected authenticated ranges → borrowed body verification → store transfer; local/NVMe/object-store parity | leased range T0; typed artifact identity; P2 publication | object pack, storage/range adapters |
| P4 | Real immutable index | delta → exact segment → snapshot → borrowed query → equivalent compaction → local/remote outage result | typed identity, range, durable publication for integration | `planes/index/**` |
| P5 | Real compiler and compact IR | source → real bounded frontend → packed IR fragment/manifest → recipe/stage driver → vertically admitted execution | typed identity, range, durable publication for integration | `domains/ir/**`, `planes/compiler/**` |
| P6 | Declarative protocol registry and static dispatch | one auditable declaration drives two real consumers, compile-time closure, compile-fail diagnostics, and erased codegen | typed identity grammar decision before shared adoption | foundation/index/compiler registries; layout lab |
| P7 | Local-first adaptive heart | typed demand + local facts + remote health + resource budget → deterministic placement/expansion/contraction with exact terminals | stable operation/range/index/compiler/publication vocabularies | operation/runtime/workflow plus private prototype crate |
| P8 | Lean client, GUI, and conditional capability delivery | measured <50 MB base shell opens local facts and acquires one verified optional component without server dependency leakage | compiler bundle, local-first coordinator, stable foundation | new client/GUI nested workspace |
| P9 | Unified system proof harness | one public journey runs as exact test, benchmark, traced simulation, and replayable fault schedule without a test-only crate | stable public seams for final integration; prototype may target current accepted subset | ordinary crate `tests/`, adapter tests, tools |

Safe parallelism is by isolated worktree, not by pretending dependencies are settled. P1, P2, P5,
P6, P8, and P9 can explore concurrently. P3, P4, and P7 must explicitly model their active/future
prerequisites and cannot declare integration readiness until those prerequisites close.

## P1 — canonical root, locality, hydration, and pre-publication authority

```text
You are the gpt-5.6-sol master orchestrator for prototype P1: canonical root/locality ownership,
borrowed hydration, and the complete-closure authority boundary in the workspace2 greenfield Rust
program. This is an isolated prototype portfolio. Do not merge, rebase, cherry-pick, or edit the
shared orchestra-shared branch. Do not call the prototype accepted, complete, 8/10, or production
ready.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create an isolated worktree and branch named codex/prototype-canonical-root-hydration and commit every
proof-bearing checkpoint there.

Read these files completely before dispatching work:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-object-hydration/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/PACKED_COLLECTIONS.md
- /private/tmp/nudox-orchestra/workspace2/LAYOUT_AUDIT.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md
- /private/tmp/nudox-orchestra/workspace2/ROADMAP.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. With four slots, run
Sol + manager Terra + Luna + reviewer Terra; do not create idle managers that block either child.

Objective: find the smallest and strongest canonical-byte-first representation that can replace the
retained native boxed root-row authority without harming arbitrary-order construction, hierarchy
validation, range/ancestor selection, locality independence, or ergonomic local ownership. The
observable vertical is canonical root bytes → checked borrowed root/locality views → caller-reused
selection/hydration scratch → complete-closure verification. Stop there. P1 must not define, construct,
or test `Published`, a stable-receipt surrogate, or any verified→published transition. Return durable
publication as an explicit future dependency on P2; a test receipt is not authority.

Keep the current safe boxed-row implementation as a measured control. Compare at most three serious
owner shapes one axis at a time: caller-retained exact canonical bytes with borrowed view; mapped or
leased immutable bytes; and a self-referential owner only if an escaping dependent view demonstrably
removes revalidation/copy. Measure owner+backing bytes, peak construction owners, allocations, copies,
pointer depth, selection work, validation repetition, construction/drop, release text, and local vs
remote ownership. Do not assume Box, Vec, Arc, Ouroboros, self_cell, Yoke, arena, or mmap wins.

Hard laws: semantic identity is independent of locality/tier; validation yields infallible trusted
projection; no field mixing forges a view; public independent facts use fields/standard traits rather
than getters; errors retain sources and rejected owners; allocation is fallible and measured; tests
live in ordinary owning-crate top-level tests; no test-only crate; no server/runtime/I/O type leaks
into borrowed core. Any typed identity migration is a declared prerequisite—do not modify the active
identity manager's paths.

Require concise public tests for every truncation/structural mutation, permutation canonicality,
pointer containment, zero/one/100k and density cliffs, exact range/ancestor work, allocation failure,
owner drop, compile-fail lifetime/forged-view impossibility, locality-only identity stability, and a
constant/input-removal mutant. Run Miri for any self-reference/unsafe candidate and keep a complete
safe control.

Return committed candidate and losing branches, raw evidence, exact Sol/Terra/Luna/reviewer task/model
proof, a deletion/API ledger, strongest counterexamples, every UNVERIFIED platform, and one verdict:
RETAIN BASELINE, REJECT PROTOTYPE, or PROMOTE FOR FUTURE INTEGRATION REVIEW. End with the smallest
future shared integration card and explicit prerequisites. Do not merge anything.
```

## P2 — durable publication heart

```text
You are the gpt-5.6-sol master orchestrator for prototype P2: the crash-safe durable publication heart
for workspace2. This is a non-merge prototype portfolio. Never edit or merge orchestra-shared, never
preserve the prototype API by default, never issue a numeric product score, and never call branch-local
gates production closure.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-durable-publication-heart in a separate worktree and commit every accepted or
rejected proof-bearing checkpoint on isolated branches.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-operation-runtime/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-foundation-fabric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/PROTOCOL_TOOLING.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md
- /private/tmp/nudox-orchestra/workspace2/ROADMAP.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. Luna commits small
cards; Terra owns crash oracles, proof, and rejection; Sol owns cross-capability prototype review.

Build the smallest end-to-end durable vertical: typed workflow command is reduced before append;
canonical fixed records enter a bounded MPSC submission path; one single-owner writer batches into a
reusable group-commit buffer; write/flush/file-sync/directory-sync semantics are explicit; stable
receipts map back to individual commands; only a stable receipt can release publication authority and
advance a compact compare-and-swap head; reopen streams records without retaining the log and repairs
or rejects a torn tail by contract.

Keep a simplest single-thread blocking file journal as control. Compare at most two more mechanisms
only after the control works: a bounded async adapter and an alternate segment/head layout. Do not
hide blocking calls behind async, introduce a database/ORM, use serde, share a file handle/probe by Arc
per operation, detach tasks, or make tracing/network export part of durability. A lock-free submission
claim requires receiver-level concurrency, bounded slots/bytes/waiters, linearization, ordering,
reclamation, Loom over production transitions, and exact cancellation/drop conservation.

Fault every write length, write error, flush, sync, directory sync, rename/head CAS, duplicate record,
corrupt checksum, torn tail, crash prefix, restart, shutdown, poison, and group failure fan-out. Use an
independent reducer/recovery oracle. Preserve primary plus cleanup/join errors. Tests and deterministic
drivers belong in ordinary owning-crate top-level tests; no scenario/testkit crate. Typed probes must
answer batch, high-water, receipt, retry, corruption, and exporter-health questions while unit probe ()
constructs nothing. OTEL remains a bounded server adapter.

Measure allocations, reusable buffer high-water, copies, syscalls, sync grouping, producer progress,
latency distribution, retained bytes, release text, dependency graph, and recovery work. Prove one
published head never names absent bytes. Treat typed-identity raw decoding and the future shared
Published typestate as integration seams, not authority to modify their active paths. P2 alone owns
prototype receipt construction and may demonstrate publication only inside this private durable
vertical. It must not
export that receipt or a publication capability as shared authority, and P1 cannot supply either.

Return commits/branches, raw fault schedules and measurements, exact model/task proof, rejected
alternatives, strongest surviving counterexample, UNVERIFIED platforms/filesystems, deletion ledger,
and RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW. Supply the smallest
future integration card. Do not merge anything.
```

## P3 — authenticated partial object/range storage and tiering

```text
You are the gpt-5.6-sol master orchestrator for prototype P3: authenticated partial object ranges,
partial clone, and portable local/NVMe/object-store tiering for workspace2. This is prototype-only.
Do not merge or edit orchestra-shared; do not compete with the active leased-range-T0 or typed-identity
managers; do not claim transport, storage, or index production closure.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-authenticated-range-storage in an isolated worktree and commit controls, winning
candidates, and losing variants separately.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-foundation-fabric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-object-hydration/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-leased-range-transport/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-operation-runtime/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/PACKED_COLLECTIONS.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. Managers produce cards
and oracles; Luna implements bounded checkpoints; reviewers never edit; Sol never integrates.

Prototype this public vertical: open only the object-pack header/directory/hot metadata; derive exact
selected byte ranges; bind each externally supplied byte region to the expected artifact and
authenticated range proof; expose a typestate that distinguishes incomplete sparse view from complete
verified view; borrow selected bodies from that original region; verify requested content; transfer
the same owner to immutable storage. The mandatory first manager card stops at checked header/directory
→ exact range derivation over an immutable in-memory owner. Require its committed calibrated return
before opening proof verification or storage adapters.

P3 must not modify or duplicate the active T0 paths, public lease/poll/cancel/credit/terminal types, or
their state machine. Until T0 closes, its byte-region input is a private, explicitly incompatible
experimental seam—not a reusable transport abstraction. Later cards may run identical storage
semantics over an in-memory owner, file/NVMe range adapter, and simulated object-store range adapter;
the future integration card must replace the seam with T0's public terminal. Remote outage yields exact
missing ranges, never empty success.

Keep whole-artifact BLAKE3 fetch/verify as safe control. Compare Bao/iroh-style outboard proofs and at
most one alternative authenticated-range structure using upstream code and measured proof bytes,
range amplification, CPU, copies, owners, code size, dependency graph, target support, and failure
semantics. IPFS/CID, iroh, lakeFS, object-store SDKs, mmap, Compio/Monoio, self-reference, and caches
are candidates only; none may become logical identity or leak into portable core by fashion.

Prototype only supplied immutable placement observations outside identity: RAM/NVMe/object-store
residency, fetch/rebuild cost, exact retained bytes, and eviction source. P3 owns no demand-counter
updates, decision loop, hysteresis, or eviction policy; those belong to P7. Caching is a first-class
projection product and may be rejected. Do not build a universal backend enum, node assignment truth,
mutable metadata database, or whole-segment staging path.

Falsify every header/directory/range/proof mutation, short/overlap/wrong artifact, neighbor bleed,
reorder, duplicate, owner drop, local corruption, object-store outage, stale supplied placement, and
partial completion. Prove pointer identity, one owner, no per-range allocation in the borrowed core,
bounded range amplification, and identical canonical results for equal available bytes. T0 retains
exclusive responsibility for cancel/drop/credit/poll proofs. Tests live in ordinary crates/adapters;
no test-only crate.

Return commits, controls/candidates, raw evidence, task/model proof, exact active prerequisites, rejected
couplings, strongest counterexample, every UNVERIFIED platform, and RETAIN BASELINE / REJECT PROTOTYPE /
PROMOTE FOR FUTURE INTEGRATION REVIEW. Supply a later integration card that explicitly waits for typed
identity integrity and leased-range T0. Do not merge anything.
```

## P4 — real immutable index plane

```text
You are the gpt-5.6-sol master orchestrator for prototype P4: a real exact-family immutable index that
demonstrates the future horizontally scalable index plane end to end. This is greenfield prototype
research, not a port of workspace/index and not a merge candidate. Do not edit/merge orchestra-shared,
do not preserve legacy APIs, and do not declare I0-I6 complete.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-real-index-plane in an isolated worktree and commit every manager checkpoint and
losing representation on isolated branches.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-greenfield-index-plane/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/write-evidence-rubric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/INDEX_GREENFIELD_PLAN.md
- /private/tmp/nudox-orchestra/workspace2/PACKED_COLLECTIONS.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md
- /private/tmp/nudox-orchestra/workspace2/evidence/TYPED_IDENTITY_ROOT_REVIEW.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. Keep one manager active
enough to leave slots for its Luna and reviewer. No agent may merge or score the product.

The mandatory first manager card is I0 only: the nested workspace, currently consumed typed identities
and family codes behind a private checked seam, a packed snapshot-manifest builder/borrowed view,
binary-searchable segment directory, exact mutation corpus, zero-allocation validation, and one local
manifest scan. It has no segment/query/publication/range/async/backend type. Sol must reproduce and
hostilely review a committed, calibrated I0 return before authorizing I1. Each later phase receives a
new committed/calibrated card and may expose only its current terminal; future phase names stay prose.

Build a prototype portfolio around one exact-family public journey, not a vocabulary museum:
canonical sealed delta → sorted exact segment built from caller-owned input → compact snapshot manifest
→ borrowed validated view → pure exact lookup/prefix cursor → typed complete/partial terminal →
equivalence-checked compaction → same query over all-local, mixed, and remote-down range owners. Add a
simulated horizontal coordinator only after leaf semantics close: snapshot pinned in every request,
rendezvous routing advisory, stale route costs one attempt, node loss returns exact missing ranges,
and a healthy worker reconstructs from immutable bytes.

Use a private prototype identity/manifest seam if necessary; never modify the active foundation
identity work. The future integration prerequisite is checked fixed-width raw authority. Do not ship
future relation/usage/vector variants, projection traits, raw codes, generic families, or errors in the
exact slice. A generic needs two current consumers and a deleted branch/copy. SIMD is forbidden until
a scalar profile finds a long contiguous posting/bitset/vector kernel; ordinary construction, binary
lookup, hashing, and sparse traversal remain scalar.

Compare fixed sorted descriptors with at most two measured alternatives for the actual density/key
shape. Validate once and borrow original bytes; lookup builds no IDs/documents and allocates nothing.
Manifest pruning happens before payload I/O. Query plans reserve fan-out, range, item/byte, and TopK
credits before execution. Compaction output is built once, equivalence checked, and published as a new
immutable set; replicas would download it rather than rebuild.

Falsify every truncation/field mutation, wrong family/snapshot, raw identity rebrand, owner mixing,
duplicate/out-of-order keys, density cliff, range absence, stale route, lost/delayed/duplicate leaf,
node loss, rebalance, hot key, remote outage, compaction replacement, tie ordering, and constant-body
query mutant. Measure retained/transferred bytes, allocations/copies, touched ranges, comparisons,
branches, decoded blocks, fan-out/high-water credits, build/query/compaction CPU, release text, and
portable dependency graph. Tests belong to owning crates; no testkit crate.

Return exact commits/branches, model/task proof, snapshot/segment diagrams, safe controls, raw
measurements, rejected legacy/framework designs, strongest counterexamples, UNVERIFIED platforms,
deletion/API ledger, and RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW.
End with ordered future integration cards and prerequisites. Do not merge anything.
```

## P5 — real compiler and compact semantic IR

```text
You are the gpt-5.6-sol master orchestrator for prototype P5: a real bounded frontend feeding compact
semantic IR and a vertically admitted compiler stage path. This is entirely greenfield. Legacy
compiler, ir/model, and ir/vcs are only counterexample/idea corpora. Do not preserve their traits,
serde graphs, JSON protocols, repositories, tests, or behavior. Do not merge orchestra-shared or call
the prototype plan-complete.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-real-compiler-ir in an isolated worktree and commit each accepted and rejected
proof checkpoint.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-greenfield-compiler-ir/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/write-evidence-rubric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/design-zero-cost-dispatch/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/COMPILER_IR_GREENFIELD_PLAN.md
- /private/tmp/nudox-orchestra/workspace2/PACKED_COLLECTIONS.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. Terra owns cards and
oracles; Luna implements frozen verticals; reviewer attacks and never edits; Sol simplifies. No merge.

The mandatory first manager card is C0 only: create the two nested workspaces; currently needed typed
IDs and language/stage/entity/type vocabularies behind a private checked seam; a manual closed static
dispatch control with two tiny honest consumers; a const capability matrix; compile-fail illegal
dispatch/stage/kind cases; exact layout and release-text evidence. It has no fragment, builder, frontend,
recipe, scheduler, sandbox, or bundle public type. Sol must reproduce and hostilely review a committed,
calibrated C0 return before C1. Each C1+ phase gets a separate card and cannot pre-create later surface.

Build this ambitious but coherent prototype chain in ordered manager slices: closed recipe/language/
stage/entity vocabulary with two honest concrete consumers; compact caller-output IR fragment format
with typed dense IDs, kind lanes, type DAG, atom bytes, pooled lists, external refs, borrowed validator
and cursors; one real bounded frontend slice chosen after measurement (Rust public interface or OXC
TypeScript subset are candidates, not mandates) borrowing its native arena and lowering directly into
the builder; canonical recipe/stage driver that reuses exact matching artifacts; owner-threaded
vertical admission reserving CPU, memory, scratch, input/output bytes, and toolchain slot; complete
validated fragment publication or nothing.

Keep a manual closed enum/static driver as control. A registry macro requires two genuinely different
frontends/capability rows, input pointer/length forwarding, manual expansion, compile-fail subset/stage
cases, and optimized consumer codegen. Do not use tagless/GADT or a proc macro by default. Do not
create testkit crates, universal AST/oracle documents, per-entity Vec/Box/String owners, public dyn,
boxed errors, async-trait, serde, Salsa as truth, runtime string stages, Arc for scoped work, process
JSON, or a second IR VCS.

Test recursive types, cross-kind/fragment misuse, every wire mutation/truncation, forward refs/fixups,
one-pass prepared output, pointer containment, allocation failure sources, input/scheduling permutation
determinism, recipe sensitivity/insensitivity, body-only invalidation, cancellation/crash at every stage,
duplicate jobs, resource-class load, diagnostic citations, and constant-body/input-removal mutants.
Tests live in ordinary owning crates' top-level tests. Measure parser arena + IR scratch + canonical
output peak, allocations/copies, per-entity retained bytes, stage/hash work, invalidation set, admission
high-water, latency/CPU, release text by monomorph, and base-client dependency exclusion.

Typed identity, durable publication, range transport, sandbox, bundle acquisition, and index delta
consumption are future integration seams unless this prototype owns an explicitly isolated adapter;
do not mutate their active/shared APIs. Return commits/branches, model/task proof, layouts, raw evidence,
rejected alternatives, strongest counterexamples, UNVERIFIED platforms, deletion ledger, and
RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW. End with ordered integration cards.
Do not merge anything.
```

## P6 — declarative protocol registry and static dispatch

```text
You are the gpt-5.6-sol master orchestrator for prototype P6: a minimal declarative registry/static-
dispatch abstraction that can serve real foundation, index, and compiler consumers without runtime
reflection, dyn, boilerplate forests, or opaque macro magic. This is a lab/prototype objective only.
Do not change or merge orchestra-shared and do not force other prototype sessions to adopt the result.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-protocol-registry-dispatch in an isolated worktree, retain manual controls, and
commit every candidate and losing result.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/design-zero-cost-dispatch/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-foundation-fabric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/PROTOCOL_TOOLING.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md
- /private/tmp/nudox-orchestra/workspace2/INDEX_GREENFIELD_PLAN.md
- /private/tmp/nudox-orchestra/workspace2/COMPILER_IR_GREENFIELD_PLAN.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. No role self-reviews
or merges.

The first scout must freeze the real call graph, naming two pre-existing non-test callers and the exact
downstream functions that consume each supplied input. Apply input-removal mutants before opening a
macro card. Prototype consumers/facades cannot satisfy admission; if two real consumers do not exist,
retain manual control and return REJECT PROTOTYPE. Only then may one narrow declaration own closed
codes/tags, compile-time uniqueness, exhaustive conversion, capability subsets, and one static dispatch
match without future-phase public types. Compare normally formatted manual code, private macro_rules,
one maintained delegation/static-dispatch crate, and a proc macro only if arbitrary Rust parsing or
source-spanned semantic diagnostics is genuinely required. A macro is not tagless merely because it
generated a match. A tagless/GADT candidate needs two real interpreters, inhabited-vs-uninhabited case
differences, simpler call sites, and branch/state erasure.

The invocation must be hyper-readable and the expansion auditable. Preserve docs, visibility,
attributes, lifetimes, descriptive generics, consts, where clauses, exact spans, no hidden helpers,
no undocumented conversions, no allocation/dyn/panic arms, and stable no_std/MSRV behavior. Raw
identity bytes use checked TryFrom; no generated From may overwrite authority cells. Registry codes
are permanent only after the active identity grammar closes—use private experimental codes here.

Require compile-pass complex generic fixtures, compile-fail duplicate/missing/illegal subset cases,
one reviewed expansion, exact error spans, two production-shaped callers that visibly consume their
input, input-removal mutants, cross-crate release IR/assembly against retained manual controls, compile
time, monomorphized text, dependency graph, LOC/deletion, and developer-facing invocation comparison.
Do not create a test-only crate; fixtures live under the owning prototype crate tests.

SIMD dispatch is a separate question. Keep explicit fearless_simd dispatch unless a second measured
production kernel has the identical scalar/crossover/tail/error law. Do not generate SIMD construction,
hashing, sparse lookup, or short-record code.

Return branches/commits, exact task/model proof, invocation+expansion, candidate decision table, raw
compile/codegen/text/diagnostic evidence, rejected alternatives, strongest counterexample, UNVERIFIED
tooling, and RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW. Propose a
minimal future integration card; do not merge anything.
```

## P7 — local-first adaptive heart

```text
You are the gpt-5.6-sol master orchestrator for prototype P7: the local-first adaptive heart that lets
a lean device expand capability under local demand or remote inconsistency and contract again when
remote service recovers. This is a policy/proof prototype, not a universal runtime/backend framework.
Do not merge or edit orchestra-shared and do not claim the index/compiler/range/publication planes are
implemented.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-local-first-heart in an isolated worktree and commit each model/control/candidate
and losing schedule.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-operation-runtime/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-leased-range-transport/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-object-hydration/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/ROADMAP.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. One manager at a time
leaves capacity for both children. Prototype branches never merge or self-score.

Define a closed, typed capability-demand algebra for current local facts, missing immutable artifacts,
remote health/consistency evidence, latency, battery/memory/CPU/byte budgets, and requested operation
priority. Produce a deterministic plan that says which pure work/data stays local, which immutable
ranges/artifacts are requested remotely, which optional capability bundle may be acquired, and the
exact complete/partial/degraded/cancelled/failed terminal. The same semantic request/result types apply
regardless of placement. Placement, host, URL, retry, tier, and cache state never salt logical identity.

Keep an explicit rule table/pure reducer as control. Compare at most two stronger static policy shapes;
do not invent a universal backend enum, dyn provider graph, string capability registry, optional-field
mega-context, ambient global health, cache correctness, or per-request Arc. Stateful coordination must
have a method×phase matrix, bounded physical credits, cancellation/drop conservation, and durable
replay inputs. Remote instability can waste work but never invalidate proven local facts or turn
missing data into empty success.

Use deterministic schedules for demand bursts, hot local queries, remote slow/down/stale/recovering,
partial local snapshot, battery/memory pressure, capability acquisition failure, duplicate requests,
cancellation, restart, and oscillation. Prove hysteresis/rate decisions with named typed facts rather
than magic thresholds. After each action compare an independent reducer, exact resource ownership,
terminal, and typed event sequence. Tests belong in an ordinary owning crate; no scenario/testkit
crate. Unit probe () constructs nothing; server OTEL adapter is irrelevant to policy correctness.

Measure decision work, branches, retained state, allocations, concurrent in-flight items/bytes,
unnecessary remote bytes/compute, local completion latency, recovery convergence, release text, and
dependency graph. Interfaces to real index/compiler/range/publication remain private prototype
adapters and explicit future prerequisites.

Return commits/branches, model/task proof, state/capability diagrams, raw schedule evidence, controls
and rejected abstractions, strongest counterexample, UNVERIFIED integration/platforms, deletion ledger,
and RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW. Supply ordered future
integration cards and do not merge anything.
```

## P8 — lean client, GUI, and conditional capability delivery

```text
You are the gpt-5.6-sol master orchestrator for prototype P8: a genuinely lean local client/GUI shell
with conditional verified capability acquisition. The base target is below 50 MB and must not inherit
server, object-store, OTEL exporter, compiler, database, embedding, or broad web-runtime dependency
graphs. This is a measured prototype, not a product merge or a license to choose a GUI framework by
fashion.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-lean-client-gui in an isolated worktree and commit each shell/framework/bundle
candidate and raw size artifact separately.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-foundation-fabric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-operation-runtime/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-greenfield-compiler-ir/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/ROADMAP.md
- /private/tmp/nudox-orchestra/workspace2/COMPILER_IR_GREENFIELD_PLAN.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. Do not run multiple
idle managers, merge, or issue a product score.

Prototype the smallest real shell that opens a local immutable snapshot/root, renders a demand-driven
view, submits one typed operation, shows exact local/partial/remote-unavailable provenance, and can
acquire one optional signed/content-addressed capability bundle by typed manifest. Bundle validation
covers target, protocol, feature subset, transitive native objects, digest/signature, partial download,
installation receipt, eviction, and uninstall without invalidating published artifacts. The base shell
does not link the optional implementation.

Before framework or bundle work, freeze one target triple, toolchain/profile/build flags, shipped-file
manifest, required runtime/assets accounting rule, and prototype trust-anchor provenance. Distinguish a
fixture key from future production signing authority; substituting another self-signed key must fail the
frozen trust test. Then establish a headless client/core binary control and complete dependency/text/
startup/memory budget. Compare at most three GUI delivery shapes using current upstream source and real
stripped release artifacts; framework popularity is irrelevant. Measure application executable plus
every required shipped runtime/asset, cold/warm startup, idle/active RSS, allocations, input-to-frame
latency, binary sections, transitive dependencies, licenses, platform support, update/component
granularity, and accessibility. The <50 MB claim names exactly what ships and on which frozen target.

Keep semantic state in typed core values; GUI message/view state is not a second domain model. No JSON
DTO mirror, dyn plugin registry, embedded server SDK, broad web server, ambient runtime, string stages,
or per-row logs. Optional components are immutable verified artifacts loaded through a narrow process/
binary frame boundary unless static linking and installation evidence wins. A plugin ABI is not assumed.
Local-first placement policy is a private adapter seam to P7, not reimplemented in the GUI.

Test offline startup, corrupt/missing/stale snapshot, remote down/recovering, demand burst, optional
bundle absent/wrong target/wrong protocol/missing object/substituted object/invalid signature/partial
download/eviction/uninstall, cancellation, and disabled diagnostics. Use ordinary crate/GUI integration
tests, not a test-only crate. Capture exact typed failure and flight tail; no panic/expect.

Return branches/commits, framework/control comparison, reproducible release size and dependency raw
artifacts, exact model/task proof, strongest counterexample, rejected coupling, UNVERIFIED platforms,
deletion/API ledger, and RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW.
End with a minimal integration card and prerequisites. Do not merge anything.
```

## P9 — unified system proof, benchmark, simulation, and tracing harness

```text
You are the gpt-5.6-sol master orchestrator for prototype P9: the unified public proof harness that
makes integration tests concise while reusing the same real journey for benchmarks, deterministic
fault simulation, fuzz/model replay, and tracing/OTEL verification. No dedicated scenario/testkit crate
and no shipping scenario module are allowed. This is an isolated prototype; never merge orchestra-shared
or claim whole-system completion.

Root must first supply a clean, committed source-candidate SHA/worktree containing this prompt and all
linked skill revisions. It is currently expected at /private/tmp/nudox-orchestra. Verify that exact SHA
and a clean tree before dispatch. If either is unavailable, stop with `UNVERIFIED: no clean prototype
baseline`; do not copy uncommitted files, manufacture a baseline, or use an older shared commit. Then
create codex/prototype-system-proof-harness in an isolated worktree and commit each harness shape,
mutant, and losing result.

Read completely:
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/orchestrate-greenfield-rust-prototype/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/deliver-reviewed-rust-slice/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/manage-rust-swarm/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/review-rust-gem/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/calibrate-rust-agent-contract/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-foundation-fabric/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-operation-runtime/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/build-object-hydration/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/.codex/skills/audit-data-layout/SKILL.md
- /private/tmp/nudox-orchestra/workspace2/TESTING.md
- /private/tmp/nudox-orchestra/workspace2/TEST_INFRA.md
- /private/tmp/nudox-orchestra/workspace2/ASYNC_STREAMING.md
- /private/tmp/nudox-orchestra/workspace2/OBSERVABILITY.md
- /private/tmp/nudox-orchestra/workspace2/ROADMAP.md

Use this exact topology, proven by successful spawn calls rather than role names: remain the Sol master;
spawn one primary manager at a time with explicit model=gpt-5.6-terra and fork_turns="none". That Terra
must spawn every implementation/mechanical worker with explicit model=gpt-5.6-luna and
fork_turns="none", plus a separate read-only reviewer with explicit model=gpt-5.6-terra and
fork_turns="none". Retain successful spawn/transport output and task identities. No agent self-scores,
merges, or creates a test-support crate.

Before any implementation, commit and calibrate a readiness card naming the exact accepted public API
path across at least two ordinary crates, its first observable terminal, the top-level owning-crate test
location, and one real cross-crate production mutant it must kill. If no such coherent journey exists,
stop with the named readiness gap; do not fabricate private state or shrink to a one-crate happy path.
For an admitted journey, a concise rstest case supplies fixture/schedule and compares one typed Evidence
record; focused helpers under ordinary top-level tests own setup, polling, join/release, conservation,
exact causal error, action index, and flight tail. The driver must not recreate production state or use
panic/expect/unwrap/let-underscore cleanup.

Run the same public commands in four modes without forking semantics: exact assertion; benchmark with
unit probe; deterministic fault schedule with virtual time/network/filesystem only at adapter seams;
and fuzz/model replay with persisted seed/action/cause/conservation/flight tail. A benchmark is an
ordinary benchmark target in an owning crate or nested adapter, not a test-only crate. OTEL tests use
the actual adapter's in-memory exporters and exact trace/span/log/metric correlation; portable core
keeps no SDK dependency.

The proof must kill plausible mutants: ignore supplied input; replace body with a constant; drop one
error source; turn partial into empty success; duplicate/drop a lease; skip a committed effect on
replay; unblock a producer only after the test releases it; construct disabled event fields; alter one
wire cell; and make a validated iterator silently shorten. If the harness remains green, reject its
abstraction. Keep tests shorter than production logic and use tables for homogeneous schedules.

Measure harness/test LOC, fixture/support ratio, compile/runtime cost, allocations/work counters,
determinism/replay stability, mutation kill set, benchmark noise, trace export bounds, and dependency
leakage. Evaluate Turmoil/Divan/iai-callgrind/Bolero/trybuild only for the exact layer they solve and
retain simpler controls. Nix may provide missing tooling; record unavailable platform evidence honestly.

Return commits/branches, model/task proof, exact mode/journey diagram, raw mutant and replay evidence,
rejected harness shapes, strongest surviving counterexample, UNVERIFIED tooling, deletion ledger, and
RETAIN BASELINE / REJECT PROTOTYPE / PROMOTE FOR FUTURE INTEGRATION REVIEW. End with the smallest
future integration card and explicit no-merge statement. Do not merge anything.
```
