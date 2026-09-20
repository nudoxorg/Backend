# Sol final adversarial implementation review

Date: 2026-09-09  
Reviewed tree: `688319abc` (`feat(extensions): modularize typed provider leaves`) plus the working-tree implementation presented for review.

## Implementation disposition — 2026-09-09

This document preserves the adversarial findings as they were issued. The
implementation subsequently used them as blocking acceptance criteria:

| Finding | Current disposition |
|---|---|
| P1.1 custody/CAS | Closed for the repository control plane. Candidate, evaluator, reviewer, and Sol are four distinct schema-typed receipt stages loaded from the durable relation; only an accepted Sol decision is reusable. The promotion adapter loads the stored chain, journals intent, reads the real protected ref, and performs `git update-ref <ref> <new> <expected>` before recording completion. |
| P1.2 frozen capacity | Closed. `Frozen` releases execution capacity, retains a separately expiring review fence, and recovery quarantines an abandoned review. |
| P1.3 retained heap | Closed. Segments share an explicit `RunOwner`; retained memory deduplicates physical owner identity and charges the complete backing allocation plus simultaneously live merge storage. |
| P1.4 fallback custody | Closed. Fallible Product fallback keeps both the daemon ticket and adapter entry until engine completion succeeds; transport failure always enters the retryable local path. |
| P1.5 reconnect | Closed. Remote health observations expire, reconnect attempts continue after bounded backoff, and a recovered compatible worker can re-enter placement without restarting locald. |
| P1.6 producer capability | Closed. Complete Product coverage crosses the admitted producer-verifier boundary and is bound to authority, session, schema, basis, and evidence identities. |
| P1.7 Product compute | Closed for the Product slice. Local and worker paths traverse the authenticated Product relation and use the same fixed-state projection algebra; the process journey compares independently computed canonical output bytes. Node preimages are retained by digest, so a subsequent root transfers only its changed Merkle frontier and fixed 32-byte input. |
| P2.1 waiter custody | Closed. Coalesced followers are explicit bounded work records rather than a saturating anonymous count. |
| P2.2 ordered encoding | Closed. Arrangement keys use a canonical order-preserving encoding with collision and variable-width ordering regressions. |
| P2.3 relation marker | Closed. The duplicate relation identity was removed in favor of the shared version schema marker. |
| P2.4 Product process oracle | Closed for the declared slice. The black-box client journey indexes the polyglot fixture through locald and observes one proof-bearing root through CLI, MCP, and desktop adapters. |
| P2.5 process harness/TCP breadth | Partially closed. The controlled runner explicitly builds every binary before its bounded process phase and the full workspace gate is green. The composed Product journey remains Unix-socket based; TCP authentication has focused tests but cross-host TCP is still a rollout gate. |
| P2.6 physical layout | Open by design. Owner accounting and path-copy reuse are fixed, while hot/cold columns, dictionary layouts, and SIMD selection remain K11 work gated by root equality and measurements. |

Post-review process falsification found and closed three additional composition
defects. The production attempt lease had inherited a one-tick unit-test
default; it now covers both I/O legs and the recipe's declared wall envelope.
Stream timeouts had been collapsed into disconnects while partial framing state
was discarded; locald and worker now share one resumable decoder. Finally, the
route model keyed a local baseline by remote locality, so an exact Warm sample
could shadow the Available row that justified delegation. One bounded
recipe/size row now holds independent fixed local and remote evidence slots,
including per-remote-state tail and circuit data. The expanded process oracle
passes only after exercising cold dispatch, exact reuse, a smaller warm Merkle
frontier, forged-result fallback, exact cancellation, late rejection, and
reuse of the local publication.

The current evidence is indexed in `docs/operations/cutover-evidence.md`.
K12 remains open because external production writer fencing, rollback evidence,
and final compatibility deletion cannot be established by repository tests.

## Verdict

**REJECT for control-plane or K8 promotion. No P0 was found. Seven P1 findings remain.**

The implementation has substantial real work behind it. The canonical tree, delta algebra, workspace ownership, durable journals, process framing, result fencing, bounded queues, and most typed admission paths are not paper facades. The declared 25-package product graph in `docs/architecture/package-dag.json:1-46` matches Cargo metadata and its dependency constraints. The status table also correctly leaves K4, K5, K9, K10, K11 production promotion, and K12 contraction open (`docs/operations/cutover-evidence.md:57-65`). I found no basis for a P0 claim because the old writer has not been declared removed and K12 is explicitly incomplete.

The remaining P1s are nevertheless release blockers for the claims attached to the agent control plane, retained-memory enforcement, and Product remote execution. Several tests prove internal consistency while never crossing the authority or allocation boundary named by the plans. The strongest example is executable and does not require malformed input: `backend-control decision` accepts a complete candidate, evaluator receipt, Sol authority, and “current” head invented entirely by its JSON caller, then emits a verified decision.

Severity in this review means:

- **P0:** an already claimed production/cutover state permits catastrophic corruption, loss, or unfenced authoritative publication.
- **P1:** a proven defect or false gate that blocks the affected milestone or control-plane promotion.
- **P2:** a real but narrower defect, test weakness, abstraction debt, or explicitly uncompleted optimization that should not be presented as closed.

## P1 findings

### P1.1 — Typed control receipts authenticate syntax, not authority; the “CAS” compares two caller fields

**Status: proven defect and direct reproducer. Blocks agent-control-plane custody claims.**

`EvaluationReceipt::new` accepts an `Authority` enum supplied by its caller and checks only that the enum is `Evaluator` or `Reviewer` (`crates/control/src/evidence.rs:280-317`). `DecisionReceipt::new` likewise accepts a caller-supplied enum and checks only equality with `Authority::Sol`; a verified decision needs merely one nonduplicate caller-built evaluation with `Verified` outcome and the same caller-built candidate ID (`crates/control/src/evidence.rs:419-479`). There is no evaluator capability, registrar-issued holdout witness, policy-root admission, reviewer-owned store handle, Sol lease, or signature in either constructor.

The executable adapter removes any remaining custody: every identity and outcome is deserialized from strings (`crates/control/src/cli/evidence.rs:19-90`), the decision command reconstructs the candidate and evaluations from the same request (`crates/control/src/cli/evidence.rs:119-143`), and `parse_authority` maps the strings `evaluator`, `reviewer`, and `sol` directly to privileged enum variants (`crates/control/src/cli/evidence.rs:219-227`). The current CLI test positively blesses this shape: it creates evaluator and decision JSON without a ledger, credential, role environment, or pre-existing receipt, and expects `verified` (`crates/control/tests/cli.rs:115-171`).

I reproduced the failure with arbitrary labels:

```text
target/debug/backend-control decision --json <one caller-created candidate,
  one caller-created verified evaluator receipt, current_head == expected_head,
  authority == "sol">
```

It exited successfully and returned decision receipt `1fcaf828f44e0654b8dc5d20160e252b2d6e82af81af83d4035557d347b8cb0b` with `decision:"verified"` and the caller-selected accepted head.

`DecisionReceipt::compare_and_swap` does not observe or mutate a protected ref. It compares the request's `current` argument with the request's `expected_head` and returns `accepted_head` (`crates/control/src/evidence.rs:523-544`). A pure receipt precondition is reasonable if a later trusted Git operation performs the actual CAS, and the plan says that final ref update remains a separate Sol operation (`docs/operations/agent-cutover.md:13-18`). No typed command shown here obtains the real Git head or performs that operation, though, so this output is not authoritative evidence that a CAS precondition held.

The Nu wrapper is a policy hint rather than an authority boundary. `require-authority` and `require-tool` trust `BACKEND_AGENT_ROLE`, `BACKEND_AGENT_CONTRACT_DIGEST`, `BACKEND_POLICY_ROOT_DIGEST`, and `BACKEND_AGENT_TOOL` from the environment (`.config/nu/cutover/main.nu:365-418`), then forward the caller's unchanged JSON to the binary (`.config/nu/cutover/main.nu:650-673`). The mutation fixture demonstrates that roles are installed by setting those variables (`.config/nu/cutover/tests.nu:208-218`). The Nix role tables correctly separate candidate, evaluation, and decision tools (`.config/nix/control.nix:522-574`, `:624-700`, `:702-760`), but PATH separation does not make a same-user environment claim unforgeable.

This is exactly the unimplemented boundary already acknowledged by the plan: role/PATH checks cannot prevent absolute executable invocation or out-of-scope access, so an external sandbox and independently held hashes/ref custody are required (`docs/operations/agent-cutover.md:94-96`). It also violates the plan's rule that admitted receipts are written outside candidate-writable storage and a decision must not trust candidate JSON (`docs/operations/agent-cutover.md:116-125`) and the preimplementation requirement for authority-enforced evidence (`docs/reviews/preimplementation-sol.md:343-346`). Therefore `docs/operations/cutover-evidence.md:103-105` overstates what the first Nu commands establish when it says they validate control-plane custody.

**Required repair:** make privileged constructors crate-private behind opaque, non-cloneable admissions such as `EvaluatorWriteLease`, `RegistrarHoldout`, and `SolDecisionLease`; load cited candidate/evaluation objects from an authority-owned append-only store rather than reconstructing them from decision JSON; bind a decision to the policy root and required acceptance/evaluation set; and have the trusted Git custody operation read the actual ref and perform one atomic expected-old-to-new update. A receipt can record that operation only after it succeeds.

### P1.2 — Frozen candidates consume execution slots forever and have no review lease recovery

**Status: proven liveness defect. Blocks bounded agent scheduling.**

`WorkStatus::is_active` counts both `Running` and `Frozen` (`crates/control/src/record.rs:152-156`). New admissions compare that active count with `max_parallel` and queue when it is full (`crates/control/src/ledger_lifecycle.rs:117-123`; the durable lazy implementation repeats the rule at `crates/control/src/durable_lazy.rs:327-332`). Freezing preserves the implementation attempt and changes the row to `Frozen` (`crates/control/src/ledger_lifecycle.rs:194-234`).

Recovery indexes expired active rows but explicitly skips every status other than `Running`, including frozen candidates (`crates/control/src/ledger_lifecycle.rs:319-362`). The test asserts this is permanent intended behavior (`crates/control/src/tests.rs:703-734`). At the same time, `Frozen` deliberately does not implement `ActiveLeaseMarker`, so the generic fail/cancel paths cannot consume it (`crates/control/src/ledger_lease.rs:54-62`). The only terminal path is `review`, which requires the still-current frozen lease and accepts an unleased reviewer identity with no reviewer deadline (`crates/control/src/ledger_lifecycle.rs:237-275`).

Concrete failure: freeze `max_parallel` candidates, then lose the reviewer process or never deliver its verdict. Owner TTL expiry cannot reclaim them; fail/cancel cannot type-check for `Lease<Frozen>`; every unrelated ready cell queues forever because all scheduler slots remain active. Same-key retries merely add bounded waiter counts. Restart reconstructs the same frozen state, so durability preserves the deadlock.

**Required repair:** freezing should release the execution resource slot and create a separate fenced, expiring review assignment. Model `Candidate<Frozen> -> ReviewLease<Held> -> Reviewed` with reviewer takeover/expiry/quarantine. If frozen artifacts must count against a different storage or review-capacity budget, give that budget its own explicit counter rather than charging `max_parallel` execution capacity.

### P1.3 — segmented `Arc` runs make `retained_bytes` undercount actual live heap

**Status: proven accounting defect. Invalidates the stated retained-memory gate.**

A `Run<V>` owns an `Arc<[Delta<V>]>` plus `start/end` indices (`crates/flow/src/batch/run.rs:7-16`). `Run::segment` makes each logical segment retain the entire owner allocation (`crates/flow/src/batch/run.rs:46-64`). Append splits a batch into `SEGMENT_ROWS` chunks by cloning that same full-batch `Arc` into every segment (`crates/flow/src/arrangement/lifecycle.rs:369-397`).

`Arrangement::retained_bytes`, despite being documented as the “actual heap payload envelope,” walks only `run.rows()`, which yields `start..end`, and sums those logical rows (`crates/flow/src/arrangement/read.rs:31-51`). During a bounded suffix compaction, selected segments are cloned into a new `Vec`/`Arc` (`crates/flow/src/arrangement/lifecycle.rs:455-487`, `:523-542`) and then drained (`crates/flow/src/arrangement/lifecycle.rs:594-608`). If any unselected sibling segment remains, it pins the complete old batch allocation, including every selected row that was cloned into the new run. `retained_bytes` counts the remaining logical slice plus the new run and omits the now-hidden selected rows still alive in the old allocation. Actual steady-state physical payload can approach twice the reported value; transient compaction also holds old and new allocations before publication without reserving that combined peak.

The current tests compare `arrangement.retained_bytes()` with a constant (`crates/flow/src/tests.rs:213-239`, `:1157-1187`), so they verify the flawed estimator against itself. They do not observe allocator bytes or shared-owner allocation identity. This contradicts the layout requirement to account simultaneously live old/new segments (`docs/architecture/layout.md:213-220`) and Astra's closure claim that batches/runs have real byte accounting (`docs/reviews/astra-midimplementation.md:80-88`).

**Required repair:** introduce one allocation owner object carrying charged bytes and segment ownership; charge a shared owner once while any segment pins it, and reserve compaction output/scratch while old owners remain live. Alternatively, copy segments into independent owners at the split boundary and charge that copy. Add a large variable-payload test that compacts a strict subset of sibling segments and measures allocation-owner bytes, not `rows()` bytes.

### P1.4 — Product fallback removes both adapter and engine custody before fallible completion

**Status: proven liveness/recovery defect. Blocks the local-first fallback guarantee.**

The Product adapter destructively removes recovery state before it knows fallback publication succeeded:

- A closure polling error clears `pending_closure`, takes `pending_dispatch`, calls `complete_pending_locally`, and discards its `Result` (`apps/locald/src/builtin/replication/admission.rs:38-49`). `complete_pending_locally` can fail during canonical output admission or local publication (`apps/locald/src/builtin/replication/dispatch.rs:477-504`). The affine plan/snapshot is then gone from the adapter.
- On a transport failure it calls `PendingIndex::take_all` first, clearing every correlation, work, and deadline index (`apps/locald/src/builtin/replication/admission.rs:95-112`; `apps/locald/src/builtin/replication/pending.rs:131-139`). Each subsequent `fallback_remote_pending` is fallible and its error is ignored.
- Deadline expiry calls `pop_expired`, which removes the live entry, before the same fallible operation (`apps/locald/src/builtin/replication/admission.rs:115-126`; `apps/locald/src/builtin/replication/pending.rs:141-155`).

The engine compounds the window. `fallback_remote_pending` journals a fence, removes the daemon's pending envelope, then performs fallible scheduler fallback, publication, and journal finishing (`crates/engine/src/daemon/remote/result.rs:279-296`). An error after removal leaves no live envelope to retry in the process. A later process restart may reconstruct an action from the dispatch journal, but the current process has lost both the adapter snapshot/index entry and, after the inner removal point, the engine ticket. Callers receive no terminal failure because `poll` returns only a Boolean and the adapter discards the error.

This violates exact logical cancellation/fallback and resource ownership in `docs/architecture/local-remote.md:117-123`. It can strand a queued request or reservation until daemon restart precisely when disk/journal/output admission is failing.

**Required repair:** use a two-phase fallback transition. Peek/borrow the pending metadata, prepare and durably admit the fallback result, then remove adapter and engine entries only after terminal publication is committed. On error, return the affine envelope or retain a durable retry action and surface terminal/backpressure state to the requester. Add injected failures at journal fence, envelope fallback, output publication, and journal finish for disconnect and deadline paths.

### P1.5 — Three failed connects permanently disable remote recovery until locald restarts

**Status: proven liveness defect. Blocks reconnect claims for K8.**

`ensure_transport` returns without doing anything once `reconnect_attempts >= 3` (`apps/locald/src/builtin/replication/reconnect.rs:11-29`). Attempts increment on thread spawn or spawn failure (`:31-61`) and reset only after a successful connection (`:64-90`). A repository-wide search finds no other reset. Dispatch comments claim that “a new dispatch starts a fresh, bounded generation” (`apps/locald/src/builtin/replication.rs:90-97`), but dispatch intentionally refuses to reset the counter while offline and merely calls `ensure_transport` (`apps/locald/src/builtin/replication/dispatch.rs:154-167`).

Concrete failure: start locald while the worker is unavailable, let three bounded attempts fail, then start a healthy compatible worker. Every poll and later Product request returns at the threshold; the worker is never contacted. Local fallback preserves local availability, but remote execution cannot recover without restarting locald.

**Required repair:** use a bounded in-flight connector plus time-based backoff/circuit state, not a lifetime attempt cap. An offline generation needs an explicit cooldown/next-probe time and a generation transition that can occur without prior success. Test worker appearance after the cap, repeated outage/recovery cycles, and cancellation while a reconnect is in flight.

### P1.6 — Builtin complete coverage remains deterministically caller-mintable

**Status: proven authority defect. Astra's required correction is not closed for the builtin profile.**

The public `builtin_coverage` function accepts any caller-supplied `ObjectVersion<T>` and returns complete coverage (`crates/engine/src/builtin/authority.rs:171-179`). Internally, `authorized_coverage` constructs both the declaration and `BuiltinCoverageVerifier` from that same value, asks the verifier to create its expected observation, verifies it against itself, and admits complete scope (`:98-163`). All producer identity, context, scope, and evidence bytes are deterministic functions of the public authority version. There is no producer-held secret, snapshot handle, registered capability, store lease, process observation, or authority-owned write involved.

This is the precise false-positive pattern Astra rejected: equal caller-created scope material proves a matching claim, not an authority observation; evidence must originate from a sealed admitted producer capability (`docs/reviews/astra-midimplementation.md:58-64`). The disposition says that producer-originated coverage is now admitted through an opaque capability (`docs/reviews/astra-midimplementation.md:90-94`), which is true for stronger boundaries such as `WorkspaceViewProducerAdmission::from_snapshot` (`crates/engine/src/builtin/authority.rs:25-96`) but false for the compiled builtin path. Worker composition calls this public mint after checking only that another admitted claim contains the same public authority ID (`apps/worker/src/builtin.rs:58-65`).

**Required repair:** remove `builtin_coverage(authority)` as a public mint. Require an opaque capability created by the actual compiled producer/session admission and bound to its source snapshot, executable/profile identity, policy/revocation epoch, and evidence sink. The complete witness should be emitted once by consuming or borrowing that capability, not reconstructed from an ID.

### P1.7 — The K8 “Product remote warm compute” evidence is root-token formatting, not Product computation

**Status: proven evidence/claim defect, not a claim that the transport is fake. Blocks K8 disposition.**

The transport, closure transfer, negotiation, attempt fencing, signed result path, cancellation, fallback, and reuse are real. The function being accelerated is not the Product source computation described by K8.

The Product output contract is a fixed prefix (`crates/engine/src/builtin/profile.rs:14-23`) plus exactly 64 bytes (`:137-157`). `output_bytes` concatenates that prefix, a workspace root, and the input object bytes (`crates/engine/src/builtin/output.rs:50-63`). `ProductInput` contains only the relation root (`crates/engine/src/builtin/relation.rs:261-280`). The worker executor charges one CPU unit, does not open or scan the transferred source relation/closure, and emits `prefix + input_basis + first input identity bytes` (`apps/worker/src/builtin/profile.rs:19-54`). Local fallback performs the same root-token concatenation (`apps/locald/src/builtin/replication.rs:48-66`). The validator accepts any complete-coverage output with the right prefix, total length, and self-consistent output hash (`apps/locald/src/builtin/profile.rs:529-543`).

The real process journey asserts that one request/result crossed the proxy and the result starts with the prefix (`tests/journeys/tests/cutover_e2e.rs:1599-1617`). It never asserts a remote docs/name/outline relation, output delta, retained trace advancement, or computation over transferred rows. The remote test changes a package coordinate and constructs the next request (`tests/journeys/tests/cutover_e2e.rs:1315-1371`), but the worker still returns only the two roots.

Migration K8 is “placement over retained remote traces, background precompute, bounded hedge and fallback,” with SLO/interference evidence (`docs/operations/migration.md:176`). The remote rollout order separately distinguishes immutable memo/object transfer, pure scope execution, and warm-trace delta advancement (`docs/operations/migration.md:196`). The current journey is valuable evidence for remote plumbing and a tiny deterministic identity recipe. It does not support the statement that the Product source slice uses remote work as an admitted acceleration path under the `K8 remote warm compute` row (`docs/operations/cutover-evidence.md:61`).

**Required repair:** either rename this profile/result and K8 evidence to an identity-envelope/transport canary, or execute one actual Product projection over admitted source rows remotely and compare its typed result relation/root with independent local recomputation. To claim warm compute, demonstrate a second source delta advancing retained remote state with work/bytes proportional to the delta, then measure queue, transfer, validation, fallback reservation, wasted loser work, and end-to-end latency against the local baseline.

## P2 findings and residual design debt

### P2.1 — Agent-control coalescing stores a saturating counter, not waiter custody

`WorkRecord` retains only `waiters: u16`; admission increments it for any repeated active request (`crates/control/src/record.rs:282-328`; `crates/control/src/ledger_lifecycle.rs:95-105`). There is no waiter identity, generation, cancellation, decrement, notification cursor, or demand lease. A caller can consume the waiter budget by repeated admits, and abandoned waiters remain counted until the work becomes terminal. The product execution interner has real follower capabilities and cancellation, but the agent control plane does not. If the agent scheduler promises individually cancellable/wakeable waiters, replace the count with bounded waiter records or explicitly document polling-only, noncancellable coalescing.

### P2.2 — `CanonicalValue` publicly promises order preservation that its generic implementations violate

`CanonicalValue::encode_ordered` says its bytes follow `Ord`, with a default to `encode_canonical` (`crates/flow/src/types/row.rs:18-43`). `Option<T>` and `(A, B)` implement only `encode_canonical` and therefore use the default (`crates/flow/src/batch/schema.rs:158-175`). For `Option<String>`, Rust orders `Some("aa") < Some("b")`, while the inherited length-prefixed canonical bytes place `Some("b")` before `Some("aa")`. Runs sort by typed `V::Ord` (`crates/flow/src/batch/schema.rs:228-233`), while `ArrangementKey` equality/order use only frozen encoded bytes (`crates/flow/src/types/row.rs:346-415`). Current checkpoint bounds exclude `Option`/tuple, so I did not prove a persisted corruption. The public in-memory generic contract is still false and leaves two orderings available to future merge/seek code. Make ordered encoding a separate sealed/unsafe-to-implement contract or implement and property-test it for every admitted composite.

### P2.3 — Two Rust relation marker types claim the same wire schema

`ArrangementRelation<V>` and `ArrangementCanonicalRelation<V>` both declare domain `0x66`, type `0x100`, the same key/value types, and the same encoding (`crates/flow/src/types/row.rs:430-480`). The second exists only to add `CanonicalRelation` decoding (`:483-529`). That creates compile-time-incompatible `StateRoot` marker types for one schema identity and requires adapter comparisons in lazy checkpoint code/tests. Implement `CanonicalRelation` conditionally on `ArrangementRelation<V>` where `V: CheckpointValue`; one schema should have one Rust marker. This is an abstraction cleanup unless callers currently persist the two markers under separately interpreted APIs.

### P2.4 — The Product process oracle does not exercise the K3 product journey

The CLI/MCP/desktop process test performs `health` through CLI and MCP and seeds a desktop model from that health view (`tests/journeys/tests/cutover_e2e.rs:1632-1735`). It does not perform add/edit/delete, docs/name/outline query, cursor delta, restart after an edit, or a second client's content read. Those behaviors have useful in-process/unit coverage elsewhere, but process framing and product behavior are not composed in one oracle. Keep the conservative wording that broader domains remain outside scope (`docs/operations/cutover-evidence.md:56`) and add one real K3 process journey before calling the migration milestone complete.

### P2.5 — Process tests are build-order-dependent and Unix-only at the composed boundary

From a state without top-level application binaries, `cargo test --workspace --all-targets` failed four `cutover_e2e` tests at `ChildGuard::spawn` (`tests/journeys/tests/cutover_e2e.rs:100-116`) because the fallback binary locator assumes `target/debug/<name>` (`:143-165`). The dedicated runner knows Cargo does not expose dependency-package binaries and explicitly builds them first (`tests/journeys/run-cutover-e2e.sh:16-32`); after that build, the same full workspace command passed. The runner itself is mode `100644`, so it must be invoked through `sh`, not directly. This can produce a false red or, in a dirty target directory, hide that CI never established the prerequisite. Make the controlled workspace gate own the build step hermetically.

The composed locald/worker Product journey uses Unix sockets (`tests/journeys/tests/cutover_e2e.rs:1438-1465`). TCP authentication has focused listener/stream tests, but there is no TCP locald-to-worker Product process journey for negotiation, closure transfer, reconnect, cancellation, fallback, and restart. Add one before making cross-host remote claims.

### P2.6 — K11 data layout remains row-oriented and duplicates payloads; the status document is honest about it

`Run<V>` is an `Arc<[Delta<V>]>`, not the planned hot/cold column lanes (`crates/flow/src/batch/run.rs:7-16`; compare `docs/architecture/layout.md:80-105`). The visible authenticated `RelationState<ArrangementRelation<V>>` also retains a cloned `V` inside every `ArrangementKey`, plus an `Arc<[u8]>` containing the encoded key (`crates/flow/src/types/row.rs:346-373`), while trace runs retain their own values. This is poor cache locality and duplicates payload/encoding storage. It is a future optimization rather than a false completed claim because K11 explicitly says “evidence only” and requires baselines/root equality before accepting physical optimization (`docs/operations/cutover-evidence.md:64`). Treat column COW, dictionary reuse, owner-level accounting, SIMD selection, and branch measurements as uncompleted K11 work.

The Nu append-only JSON reducer is similarly an acknowledged compatibility oracle, not a hidden second production authority: its header explicitly says Rust never reads it (`.config/nu/cutover/main.nu:1-8`), and K12 records that compatibility oracles remain (`docs/operations/cutover-evidence.md:65`). It should be deleted at K12 rather than generalized.

## Milestone and plan adherence

| Milestone | Review disposition |
|---|---|
| K0 | Schema and mutation evidence exists, but authority/custody is not established by the current executable receipts (P1.1). Baseline thresholds remain openly missing. |
| K1 | Strong implementation evidence is present for typed roots, path-copy updates, durable publication, recovery, and bounded admission. I found no P0/P1 in the reviewed slice. |
| K2 | Algebra, retractions, joins, recursion, paging, and compaction have meaningful tests. The retained-memory gate is false for segmented owners (P1.3). |
| K3 | A real locald and real client processes exist. The process oracle is health-oriented and the source slice is much narrower than migration K3 (P2.4); the status document does acknowledge broader domains are out of scope. |
| K4–K5 | Open, accurately. Provider/facet completeness and shared production recipe promotion are not claimed. |
| K6 | The generic execution scheduler has strong fencing, reservation, coalescing, and cancellation tests. Product adapter error custody remains unsafe (P1.4). Production planner cutover is correctly open. |
| K7 | Protocol/closure/transfer machinery is substantive. Product fallback error handling and lack of a composed TCP process journey remain gaps (P1.4, P2.5). Full replication promotion is correctly open. |
| K8 | Not demonstrated as remote warm Product compute. The worker formats roots, reconnect permanently stops after three failures, and no real retained remote trace is advanced (P1.5, P1.7). |
| K9–K10 | Open, accurately. |
| K11 | Evidence only, accurately; row-oriented duplicated representations remain (P2.6). |
| K12 | Open, accurately. The legacy oracles, writer deletion, external custody, rollback, and full product/provider gates are not closed. |

## Validation performed

- `cargo metadata --no-deps --format-version 1 | cargo run --quiet -p backend-workspace -- --metadata-only` passed. The Cargo/package DAG is internally consistent.
- A first `cargo test --workspace --all-targets` reached the process journey and failed 4 of 7 `cutover_e2e` tests because application binaries were absent from `target/debug`. This is the order-dependent harness issue in P2.5, not evidence of those product processes crashing.
- `sh tests/journeys/run-cutover-e2e.sh all` passed 22 journey library tests, all 7 real process tests, and all 7 cutover regression tests after explicitly building application binaries.
- Re-running `cargo test --workspace --all-targets` after that explicit build passed the complete workspace suite.
- `nu --no-config-file .config/nu/cutover/tests.nu` reached and passed the contract mutation section, then stopped at `missing-b3sum` outside the pinned environment. `nu --no-config-file .config/nu/cutover/e2e.nu` refused to run without `BACKEND_CONTROL_PLANE`, as designed for a pinned entrypoint. I do not count either ambient-shell failure as product evidence or a product defect.
- The direct arbitrary-label `backend-control decision` reproducer succeeded as described in P1.1.

## Required release posture

Keep K12 open. Do not promote the typed agent evidence commands as a custody boundary, K2 retained-memory enforcement as closed, or K8 as Product remote warm compute. The smallest coherent repair tranche is:

1. authority-owned stored receipts plus real protected-ref CAS;
2. an expiring review lease that releases execution slots on freeze;
3. allocation-owner and simultaneous old/new compaction accounting;
4. non-destructive, retryable Product fallback and a recoverable reconnect circuit;
5. producer-capability coverage for the builtin profile; and
6. one semantically meaningful remote Product delta with an independent local oracle and measured resources.

After those repairs, add falsifiers that inject each fallible fallback stage, retain only one sibling of a segmented large-payload owner, bring a worker online after three failed connects, attempt direct self-minting of evaluator/Sol receipts, and compare a remote Product relation/delta rather than a prefix and root bytes.
