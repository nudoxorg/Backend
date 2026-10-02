# Nudox stopping checkpoint — 2 October 2026

This is the handoff at the user's request to reach a stopping point. Implementation, agent work, and builds have stopped. Source changes are committed in their worktrees; unfinished slices and failed runs are preserved. **Nudox is not yet accepted as production ready.** The continuation candidate has substantial reviewed source work, but has not been compiled as a whole. This checkpoint does not merge code into canonical or push anything.

The most immediate work is to review the remaining isolated patches, compile the composed candidate, fix the remaining required-content failure precedence, and execute native journeys against a real owner. The broader index/compiler/IR-history goal remains open and should be paused, not marked complete.

## 1. How to use this brief

Read §§2–6 before changing code. They identify the authoritative repositories, the source already composed, and the saved work that is still outside that composition. Read §§7–11 before making readiness or performance claims. Read §§12–16 when scheduling the next integration and acceptance pass.

Three different statements must stay separate:

| Statement | What it establishes |
|---|---|
| Source reviewed / syntax parsed | A design and a saved implementation; not Rust type correctness or execution. |
| A frozen gate passed | The named tests ran on the named source, lockfile, toolchain and profile. |
| Product accepted | Real input, real data, persistence, failure recovery and visible/native behavior pass the specified flow. |

“Integrated” below means cherry-picked into the **continuation candidate**, unless explicitly described as primary. It does not mean deployed, compiled, live-tested, or merged into canonical. Agent reports are identified as such when the root has not read their entire final diff. Earlier readiness documents are historical ledgers; this brief updates their status without erasing their failures or narrowing the original goals.

The accompanying [workspace inventory](workspaces.json) records 17 relevant workspaces at the source checkpoint, with full heads, trees, branches, dirty state and lockfile digests. [Additional stopped test slices](additional-stopped-test-slices.json) records two older isolated agent patches discovered in the final agent inventory. [Integration commits](integration-commits.tsv) lists all 116 atomic source commits composed after `17a5a872b42e46b7082b0daffff31c9420d06b9e`. Documentation commits made after this snapshot do not change those code identities.

## 2. Repositories and preservation boundaries

| Role | Workspace / branch | Frozen code head | State and intended use |
|---|---|---|---|
| Primary continuation | `/Users/mileswirht/Documents/ChatGPT/backend-index-compiler-tentpole`, `codex/index-compiler-tentpole` | `93d7260403d0ba7af44705dd7ac3a9e03ef2a0c1` | Clean at the source snapshot. Contains the focused geometry repair and the subsequent unrun capture-ledger repair. Stable home for this brief. |
| Composed GUI candidate | `/private/tmp/nudox-gui-flow-integration-20261002`, `codex/gui-flow-integration-20261002` | `3bab7869eb843cbd404ef9fb9a9e152e2ad0d4c1` | Clean code, 116 selected source commits; **no composed Cargo/check/test gate**. Continue integration here after resumption. |
| Canonical | `/Users/mileswirht/Downloads/backend`, `canonical` | `f9c158af0dfc229c4567bf5cd725c5c0828043fa` | An external local edit exists in `.config/nix/tools.nix`. Preserve it. This checkpoint does not merge or push code here. |
| Original supplied cwd | `/Users/mileswirht/Documents/ChatGPT/backend` | **No resolvable HEAD** | Symbolic branch is `canonical`; `crates/`, `docs/`, `extensions/`, `implementation/`, `redesign/` are untracked and Cargo.lock is absent. This anomaly was observed read-only and was not repaired. |

The primary code tree is `4952d51dcd3c9b529a39615836ccfd7ee88e5b72`; the candidate code tree is `183ba2931a73b7db080713150d33e99d4f7bcac1`. Both have Cargo.lock SHA-256 `d747fe5381bb0006934581c41d9985620c142119adff019ac5ba84a9fb5701ad`. The isolated producer worktree has a different lockfile, `15e42752ce7a6cc072058cb7f0df77be60e3220be7419d0e38a25508678adc17`; its passes must not be relabeled as candidate passes.

Do not reset, clean, prune, or reconstruct the original Documents checkout to resume this work. Use the preserved primary and candidate branches. Resolve the original checkout anomaly separately, after checking its Git metadata and untracked content. Likewise, do not sweep old worktrees merely because they look inactive: this repository has many historical lanes, including prunable registrations, that were not exhaustively audited for deletion at this stopping point.

All agents in this turn's live team reported stopped/completed. The final process observation found no Cargo process. No new build was started for this brief.

## 3. Architectural direction and what is actually unified

The intended system remains local first: the GUI embeds or attaches to the same local owner used by CLI and MCP; a horizontally scalable compiler tier produces IR and embeddings; a vertically scalable index owns package discovery, versioned metadata, dependencies/dependents, search projections, durable publication and retrieval. Local disk must be a complete storage backend; S3 is an optional deployment choice. Remote delegation should reduce work without making local editing wait for a remote round trip.

The GUI architecture is documented in [GUI architecture](../../../operations/gui-architecture.md) and `apps/desktop/ARCHITECTURE-V3.md`: an immutable application snapshot, typed intents, a reducer, and effect actors. Framework entities and borrows own UI mutation; immutable snapshots may use shared ownership. This does not justify claiming that all paths are zero-copy, lock-free, allocation-free, or free of Arc/Mutex. The latest source strengthens authority and lifecycle boundaries; the unbounded completed-read queue below remains a concrete memory counterexample.

The useful unifying chain is:

1. **Versioned identity:** exact package/release, requested project, effective source scope, source bytes and owner publication are distinct identities.
2. **Admission:** a root is usable only under a current owner attachment and the exact resource binding. A matching diagnostic revision string is insufficient.
3. **Dependency plan:** a route declares the resources needed for its content separately from optional chrome. The same plan should drive loading, watches, residency, readiness and failure display.
4. **Visit identity:** visible controls and deferred intents carry the current route, root authority, attachment and page stamp. A callback must re-admit them at event time and again at deferred flush where necessary.
5. **Native ownership:** retained or departing pages can remain visually present without owning keyboard, pointer or accessibility actions. Motion settlement and mounted control identity determine focus admission.
6. **Work lifecycle:** capacity must remain owned until a result is landed or dropped, not merely until a worker finishes. Producer reply permits do this in reviewed source; the GUI ReadPool does not yet.
7. **Certified publication:** streaming deltas and reset hydration become authoritative only after verification and acknowledgment. A reconnect, identical cursor or retained root alone must not establish fresh readiness.

This chain is a stronger basis for reducing duplicate state than separate booleans spread across Reader, Library, Find and the owner observer. It is not yet fully closed: required-content failure selection, README actions, producer expiry, fresh observer admission, and GUI result capacity still need composition and execution.

The larger compiler work must continue from the existing compiler and IR, not replace hundreds of hours of semantics with a new lightweight analyzer. Earlier audits confirmed that foundation is present, while distinguishing snapshot comparison from the removed persistent commit/replay repository. Restoring durable lineage and stable producer segmentation remains necessary; a clever planner cannot create edit locality from ordinal byte chunks.

## 4. Source composed into the continuation candidate

Root reviewed the selected source series and resolved cherry-pick conflicts. The complete commit inventory is the TSV, rather than an implied single squashed change. The following groups describe the resulting behavior and the important limits.

### 4.1 Find, Settings and native controls

The 23-commit Find series adds exact result membership and typed query routes, event-time root/resource checks, human names for symbolic buttons, native text-input identity, radio focus retention, actual Settings choices and cache-age actions, and native Tab routing. Departing or pending evidence cannot leave acquisition actions live. The Find query focuses on its first eligible active paint, rather than stealing focus on every update.

An ignored 36-frame Find/Settings native journey is present as source. **It has not run.** Fixture regressions and source inspection do not establish live focus, input editing, accessibility or actual package navigation.

The final cross-flow commits `d9421d44a0f4eca4f6974766b5e56d2712149724` and `3bab7869eb843cbd404ef9fb9a9e152e2ad0d4c1` unify `CurrentBrowseVisit` checks for Find and Compare. Compare painting reads current data without re-entering Reader rendering. Local recovery remains available where appropriate. Folio Tab consumes input until the native input owner, motion and current paint permit navigation. Find records its exact pre-Ask input handle and a one-shot return disposition so late results do not steal focus after intentional navigation.

One return-disposition edge remains corrected only in an isolated follow-up: Tab or pointer movement **inside the open Ask overlay** should not cancel the pending return to Find. That patch is listed in §5.4.

### 4.2 Cargo routes, Library and native navigation

The selected Cargo/Library series introduces one `RouteDependencies` plan for visible keys, gathering, residency and watches. It carries an opaque owner attachment, lazily revokes old reads across same-root owner replacement, treats seed state as unserved, and preserves exact Library reading memory only under the proper source/current-owner identity.

Library release and inventory controls have native handles, exact release ordering, reflow-aware row hints, interruption and generation checks, and explicit return-focus disposition. Local recovery is usable over retained reads without accidentally admitting retained data actions. Wrapped gutter labels remain inert.

The source-binding corrections at `05075099277586d26e3d9e031a588cdd4cd65758` and `96312b6adab1370890a900c68d1b3288a323b317` carry `CargoBrowseContext`: the requested project and full requested/effective binding survive routes, persistence, page keys, workers and current model factories. Optional semantic dossiers do not define content authority. Withdrawn local fallback is cancelled before publishing. Filenames with outer whitespace receive an explicit typed refusal rather than lossy normalization.

`b8e84de4c8ded0b95291926eeae50407dbcea126` carries a `RouteReadLease` through queued actions. It checks the exact route/overlay, root authority, owner attachment, page stamp and graph visit generation at deferred flush. Context-free resolution is refused; immediate and deferred resolver paths re-admit the exact Tree/package/context. Its six-file final diff was read by root; deterministic tests are written but unrun on the candidate.

### 4.3 Header and ecosystem truthfulness

The header series makes titlebar jump controls and manifest URLs real native actions, revalidates source and crumb actions against the current visit, and gives licence disclosure one closed `Owned`/`Controlled`/`HeldSnapshot` mode. Pointer/native click origin and rendered expansion state govern toggles. Native accessibility is forced before the relevant disclosure test reads it.

`6bdfae52ae4c2a3d18e0f5f2c99da49bc5ef27da` binds seven ecosystem choices to typed exact PURLs. “Local” requires a single owner record. NuGet and unknown ecosystems do not inherit Rust/Cargo labels or facts. This is reviewed source, **not an executed seven-ecosystem GUI gate**.

### 4.4 Geometry and native focus

`b9964a2c8d987cf20ad2fb8ae3994634fa973fde` omits Reader visual descendants when Ask leaves no readable preview area, in ordinary and graph paths. Route and motion state remain intact. A zero-opacity covered Reader had still contributed a heading to the paint probe; the repair removes that visual subtree for those frames rather than weakening the clearance assertion.

Native navigation corrections reconcile Shell's zone with the currently focused mounted Reader control before Tab or J/K, and redraw when reconciliation occurs at a walk edge. Root's `d9e14f41e9fdc969605e1a46430d9496b2f3e23d` preserves the private pure zone setter, uses the public focus-taking path for actual Shell focus, shares the settled-motion gate, and excludes Ask's departing state. These corrections are unrun on the composed candidate.

### 4.5 Producer source authority and bounded browse work

The 39-commit producer series preserves Cargo metadata authority, metadata-listed manifest witnesses, config/toolchain byte identity, exact wrapper-chain recognition, independent file/inventory/README observations, and a bounded per-requested-workspace LRU that includes shared inputs and external path dependencies. Cancelled freshness reads do not destroy a previously admitted cache entry.

Local-service browse work is off the owner loop with bounded request-root sharding and reply permits retained through encoding. That is a separate lane from the GUI ReadPool; it does not repair the GUI result queue. Deadline expiry terminalizes a reply before a late worker completion can revive it.

Subprocess capture owns bounded stdout/stderr and descendant cleanup. Unix process groups are retired even after a successful leader exits; capture capacity and unsupported targets fail before spawn. Windows source uses owned Jobs, overlapped dual-stream capture, IOCP retirement, pinned kernel-owned storage behind `UnsafeCell`, ordinal comparison error propagation and documented native syscall deadline limits. **Windows was not compiled or exercised in this pass.** Unsafe storage and teardown require platform review and real interrupted-process tests before acceptance.

The exact saved Nix-generated Cargo wrapper is recognized by measured body and shebang; fixture shadowing errors were fixed. Later isolated executions prove one real two-workspace source-authority fixture and six local-service nonce tests, not the entire candidate.

`4c8ecdb6a36c218ad0f986820416d6e0bfc20196` changes subscription lease identity to a lazy OS-generated 32-byte owner boot nonce and checked ordinal. Lease v2 identities cannot silently wrap or recur across owner boots. Six isolated service tests pass; actual daemon/client cold restart remains unaccepted.

### 4.6 Shared content phase and external observer

Root's `368bc35b4bfb0b021dc088c4ec0918a3d16b26bc` adds a shared `ReadPhase` join with `Ready < Pending < Terminal`. `ResourceAdmission.phase` and `RouteDependencies.content_phase` replace duplicate readiness decisions. A terminal required dependency wins over other pending dependencies; optional chrome is excluded. A completed Tree with the wrong Cargo binding becomes terminal, not indefinitely pending. Model/test constructors and typed dependency gathering were adjusted. This is source parsed and reviewed, not compiled.

`7061cd624116c9ec5403779b387601844ba3ed8e` composes the durable local subscription observer: Open/Resume/Renew/Ack/Cancel outside the UI, coalesced certified roots, stable attachment distinct from publication epoch, and readiness withdrawal plus attachment rotation after protocol rejection before a fresh reset. Root read its full nine-file diff and obtained an adversarial source review.

That observer base still has confirmed lifecycle holes. Producer lease expiry, fresh per-attachment completion, descriptor-based reset limits and best-effort Cancel are saved separately below. Do not call the integrated base a complete bounded reconnect protocol.

## 5. Saved work still outside the candidate

These branches are clean source checkpoints. Root has **not** read every final implementation diff in this section. Agent syntax/diff checks are useful but do not replace root review, typechecking or runtime execution. Keep patches atomic and review their prerequisites before cherry-picking; different producer lockfiles are not interchangeable.

### 5.1 Owner expiry and reset-root reclamation

- Workspace: `/private/tmp/backend-lease-expiry-reclamation`.
- Branch/head: `codex/lease-expiry-reclamation`, `6a54f65ea65f0c1b4a9e839233c7e2b231ff2ac3`.
- Four files: local-service `service.rs`, service tests, `protocol.rs`, `lib.rs`; 611 additions and 60 deletions.

The agent reports an earliest-expiry cache for O(1) idle checks on the existing 5 ms owner poll, a due scan capped at 1,024 leases, release of expired lease/reset roots, and close-time clearing before deferred joins. Defaults are 256 active leases, five-minute lease duration, 2,048 reset pages and a 90-minute absolute reset duration. Hard caps are 1,024 leases, one hour, 4,096 pages and four hours respectively. Only an exact, nonempty, advancing, successfully encoded page can slide the negotiated lease deadline. Replay, stale requests and encode failure cannot keep it alive; independent page/absolute caps bound slow drip.

Written deterministic helper/root-drop regressions are unrun. `rustfmt --check` and diff checks pass. A standalone blocking `serve_stream` still needs its host to poll the owner, and blocking owner work can delay reclamation. The next review must check the actual production listener's polling, encode-failure paths, arithmetic and timeout negotiation, plus simultaneous expiry/cancel/reset/late-worker schedules.

### 5.2 Fresh observer admission, reset limits and cancellation

- Workspace: `/private/tmp/sol-owner-publication`, branch `codex/sol-owner-publication`.
- Head: `579f24992221b8a5ea0fb0459e60d2b9fc5bdec8`, based on the observer source at `c932`.
- Patch: `/private/tmp/sol-owner-publication-review-fixes.patch`.

Three saved commits address concrete observer findings:

| Commit | Proposed repair | Required proof |
|---|---|---|
| `2f9580599b4e5a158a401c9d16132e2ee56d59b7` | A fresh certified marker per attachment is required before completion/readiness. Obsolete/regressed publication cannot establish Ready. Cancellation callbacks move outside the gate mutex on all five paths. | Same-cursor fresh admission, non-one producer epoch, obsolete reply, direct callback re-entry and concurrent withdrawal. |
| `ebb5a0c063480cfc5ae7bf5328ce8fac3372014e` | Fixed descriptor-derived reset budget: 10 seconds plus two seconds per ceiling(rows/64), at most 131,072 rows / 2,048 pages. Authenticated continuation and hydrator progress remain strict; only final certificate and Ack advance admission. | Legitimate multi-page reset lasting over 10 seconds, no timeout extension from replay, mismatched descriptors, partial or cancelled hydration. |
| `579f24992221b8a5ea0fb0459e60d2b9fc5bdec8` | Best-effort terminal Cancel on the exact socket with 50 ms read/write limits, no reconnect or new request preparation; cleanup after failed acquisition. | Prompt interruption, no replacement-session cancellation, exact lease/socket ownership and producer-side expiry when Cancel fails. |

All are syntax/diff checked only. Coordinate this slice with §5.1. A receiver's Cancel attempt does not prove that the producer reclaims retained roots.

### 5.3 Independent Cargo README and link navigation

- Workspace: `/Users/mileswirht/Documents/ChatGPT/backend-sol-cargo-route-dependencies`.
- Head: `b7ec91ed775f1f6a19228109b10fd8aba1e1f56f`, parent `eefd5b38841ea0ed0a8d92fbd529e50423a0816b`.
- Scope: 30 files, 931 additions / 129 deletions.
- Detailed handoff: [preserved agent report](readme-navigation-agent-handoff.md), copied verbatim from `.local/reviews/readme-stopping-brief.md`; exact patch and path inventory remain beside the original in `.local/reviews/`.

The agent reports separate closed `PackageFile` and `ReadmeLink` addresses carrying full origin, inherited scope, href, path and fragment. The README key includes full binding plus origin/content digest, independent of semantic dossier availability. A cold binding failure permits **one** bounded Tree hint with exact requested project/full binding/source package, followed by one identical retry; it is not an unrestricted fallback. Positive, absent and negative results must all bind exact selectors.

Worker-prepared navigation is bounded at 512 headings/links with 32-row disclosure. README links carry a current route/page-stamp lease through dispatch, retained content is inert, and persisted links use typed addresses that convey no authority by themselves. External spelling admission does not open arbitrary local files.

Root read the detailed stopping handoff, not the complete final 30-file patch. The preserved report is the agent's frozen view; some prerequisite commits it calls awaiting integration are already composed as recorded in §4. Three worker regressions are written and unrun. Outstanding tests include forged origin/scope/href/line, persistence, exact cold producer retry, same-root owner replacement, late results/new paint, queued README changes, markdown bubbling and anchors, deferred focus, keyboard and source roles, and actual native pixels.

**Known risk:** ordinal README row IDs can restore focus to a different link after origin/content changes. Rich markdown may still parse on the UI despite prepared navigation. There is no responsiveness or performance acceptance. Compare the slice with the already composed full Cargo context, current-read lease and shared Markdown lifecycle before integration.

### 5.4 Find/Ask return behavior and mounted pending regressions

- Workspace: `/private/tmp/backend-sol-find-pending`, `codex/sol-find-pending`.
- Head: `90b11cd80807bb864df3d97b844eff386dcae18e`, based on candidate `3bab`.

Original repair `d62718e0ce9a3cfacb60d565486307d84a23b2c2` is cherry-picked here as `a948a29187`. It cancels pending Find return for Tab/pointer only when Ask is no longer open, including its leaving state. Modal Tab or a click inside the Ask editor followed by Escape should return to the exact originating Find input; an actual exit/navigation interruption should still cancel that return.

`90b11cd…` adds mounted tests with a real ReadPool-held pending Find result, query/motion, Cmd-K/Escape while still pending, restoration and a late result that must not steal focus. It also tests an old painted Compare closure across same-root owner attachment replacement from Starting to Ready.

These are syntax/diff checked only. Fixture timing, imports, asynchronous preparation and actual native execution remain unverified. Root has not fully reviewed the final follow-up diff.

### 5.5 Joined real ingest, persistent MCP, backup/restore and GUI consumer

- Workspace: `/private/tmp/backend-joined-live-ingest-readiness`, `codex/real-live-ingest-readiness`.
- Head: `56ba612cedc4cdcffc008e5742167ce2907325ae`.
- Source commits: `4ecbec641d21b8bd7eff1c90e568feaec39b22d3`, `56ba612cedc4cdcffc008e5742167ce2907325ae`.
- Exported patches: `/private/tmp/joined-live-ingest-producer-4ecbec6.patch`, `/private/tmp/joined-live-ingest-gui-consumer-56ba612.patch`.

The producer harness uses seven official public pins, the product CLI/locald, and **one persistent stdio MCP connection** across owner cold restart and in-place Turso `VACUUM` backup/restore. Fresh product binaries are receipt-bound; curl/Turso have a minimal environment allowlist; phases are sequential with exact PID cleanup and fresh target state. Conan's identity is corrected to `pkg:generic/conan/zlib@1.3.1`.

The GUI consumer reports a typed receipt with build/joined-receipt hashes, exact canonical workspace/endpoint/PID command, seven exact PURL/EcosystemSession checks, and actual NuGet GPUI pixels, accessibility tree, root and Reader text checks. It depends on the separately composed ecosystem mapping.

Rust parsing, Bash syntax, Python heredoc syntax, diff checks and explicit opt-out exit 64 passed. **No Cargo typecheck, official registry run, Turso run, native GUI capture or cold restart ran on this harness.** Root must review both patches and prove that the GUI consumes the exact joined owner, rather than wiping state, attaching a different daemon, using old binaries or accepting quiet registry failures.

### 5.6 Older isolated test repairs found in the final team inventory

Two older clean test-only slices are saved but are not ancestors of either primary `93d726` or candidate `3bab`:

| Workspace/head | Agent-reported change | Status |
|---|---|---|
| `/private/tmp/backend-sol-fluid-native-repair`, `b05c8767a666f7cc6d5fdfbfdc36b6573136b42d` | `89877a2730360ab28f3e739bb47ccd96e3ac21f3` enables native accessibility in fluid tests; `b05c876…` binds the preview oracle to the current admitted result/route rather than the wrong fixture name, preserving geometry/inertness assertions. | Syntax/diff only. Compare with later primary geometry and full-shell results before cherry-picking; do not blindly apply obsolete test expectations. |
| `/private/tmp/sol-ask-mounted-tab`, `06fc0204aadf09be553b1e9087f4aafa481ad72b` | Waits for the actual settled Ask plate and current native result links with Click actions before a component-root Tab cycle, retaining focus-cycle and overlay assertions. | Syntax/diff only. Review against later mounted ownership changes and execute the exact test. |

These are included to avoid losing agent work, not to imply that every saved patch should be merged.

## 6. Confirmed remaining defects and revised findings

### 6.1 Required Tree failure can be hidden by an optional dossier

This is a confirmed source defect in the candidate's content-phase composition. A Cargo package route bound to context A receives a completed current Tree bound to B. Required `content_phase` is correctly Terminal. Reader, however, gathers both the Tree and optional Package keys; generic terminal exposure scans Symbol/Source/**Package fault**/Orbit without selecting the required Tree failure. An optional dossier fault can therefore hide the actual binding refusal. If the dossier is pending, the package body can show its pending message and a generic observation note instead of the completed Tree mismatch.

Repair terminal display from the same required-content dependency plan that determines phase; retain optional chrome separately. Add tests with completed wrong-binding Tree plus optional dossier Pending, Fault and Ready, including owner replacement and retained data. Do not fix it with another independent boolean or by treating optional dossier failure as content failure.

### 6.2 Observer and producer bounds remain incompletely composed

The integrated observer can admit stale completion without the isolated fresh marker, has a reset limit too tight for larger legitimate hydration, and previously invoked cancellation under the gate lock. The producer can keep expired leases/reset roots pinned without §5.1. These are coordinated correctness and memory issues, not polish. The saved patches are proposals until reviewed and run together.

### 6.3 GUI ReadPool does not bound completed residency or UI work

Read-only audit workspace: `/private/tmp/sol-bounded-read-results-368b`, branch `codex/sol-bounded-read-results`, unchanged head `368bc35b4bfb0b021dc088c4ec0918a3d16b26bc`. **No implementation, parsing, Cargo or tests occurred in this slice.**

The audit identifies:

- `apps/desktop/src/runtime/reads.rs:307`: unrestricted outcome `VecDeque`; the 64-job limit at line 403 counts queued work, not completed payloads waiting for the UI.
- `reads.rs:562` and `:607`: every partial publication is appended; terminal completion retains earlier queued partials.
- `reads.rs:481` and `runtime/store.rs:1030`: all outcomes are drained and landed in one UI update.
- `runtime/store.rs:496` and `wake.rs:145`: a pending wake returns immediately. Limiting a drain to eight alone can still consume many batches in one executor poll.
- `model/pages/source.rs:88`, page-store quiet landing/partial staging: exact source byte equality can execute on the UI; a single source file may be 4 MiB.

The proposed next slice is private RAII admission ownership from accepted request through worker, queued payload, UI landing and final drop; at most 64 admitted reads initially, capacity reserved for normal reads over prefetch, one queued partial per key/generation, terminal replacement of that partial, and small UI batches with explicit cooperative yielding and coalesced wake rearming. Publishing must remain nonblocking and terminal outcomes must never disappear merely because capacity is full.

This proposal must preserve owner/root/generation checks, priority, continuation affinity, rejected-prefetch cancellation and typed Busy faults for rejected normal reads. Prefetch eviction does not release capacity until its outcome is dropped. A drained partial and terminal may briefly coexist, so permit sharing must cover cloning and that interval. A read-count bound is **not** a heap-byte bound; all `PageValue` variants need a defensible payload-cost contract before claiming memory budgeting.

Existing `CoalescingMailbox` is worth reusing only through a deliberate small extension: its key vocabulary, blocking publisher and lack of priority/affinity do not directly fit this pool. Exact equality can later move off-thread using an immutable baseline revalidated at landing; hash equality alone cannot establish semantic equality.

Required tests are still unwritten: slow UI/fast completions, partial flood, cancel during publication, late generation, queued replacement, shutdown, prefetch saturation, release after landing/drop and fairness on the actual executor. This is a real remaining responsiveness/memory gap.

### 6.4 Retracted findings: do not reintroduce false blockers

Two review claims were corrected after root source inspection:

- A pending Find result does **not** itself prevent the return-focus arm in the integrated P1 design: Find has no content dependencies, so its content phase is Ready while results remain pending and its input is painted. The new mounted regression is still valuable but does not prove that the former diagnosis was correct.
- Legacy Cargo routes with missing/invalid binding are **not** proven to observe forever. `current_tree()` still returns a Tree with no context where expected context is absent; `awaiting_tree` already presents an explicit saved-receipt refusal and Library recovery. The separate completed wrong-binding/optional-dossier precedence defect above remains valid.

Preserve these corrections in future reviews. A brutal review should falsify its own diagnoses as well as the implementation's claims.

## 7. Executed verification: exact scopes

The current evidence archive has 143 completed records and 720 copied files whose hashes were checked. It contains failures as well as passes. It does not turn an older isolated pass into a pass on the composed candidate.

| Frozen source / gate | Actual result | What remains unproved |
|---|---|---|
| Primary `17a5a872b42e46b7082b0daffff31c9420d06b9e`, full shell | **357 passed, 2 failed, 3 ignored**, 372 filtered; 268.61 s | Two Ask fluid geometry failures; no candidate acceptance. |
| Primary `7c2e96a7252b81e369d8c089721a51111a5e7792`, focused fluid | **16 passed, 0 failed**, 718 filtered; 29.79 s | No fresh full shell or native pixel run. |
| Same `7c2…`, size-optimized native Ask 32-frame attempt | **Compile failure, zero tests/ingest/frames**; E0609 `CaptureRecord.ledger` | No live capture acceptance; repaired only as unrun primary source. |
| Isolated source producer `d17bce…` | **0 passed, 1 failed**, 740 filtered | Actual Nix wrapper not admitted; helper/cache remained typed Unavailable. |
| Isolated producer `e840682396baf40915897763096a18e0a6d27dc1` | **Compile failure, zero tests**, two fixture shadow/type errors | Wrapper repair not exercised on this head. |
| Isolated producer `45c973c4457f2228ca9630c9d5f19672441bf448` | **1 passed, 0 failed**, 740 filtered; 7.71 s after 6m04 compilation | Only the exact two-real-workspace source/LRU/shared-input fixture. Different lockfile from primary/candidate. |
| Isolated producer `262b2eef8bd0a59a13d935eae1be56de54c122e5` | **6 passed, 0 failed**, 737 filtered; 7m03 compilation | Service nonce/restart-rejection/nonwrapping unit scope; no actual daemon/GUI restart. |
| Primary `35d903e052b1f178ea45b2f39a94cf70c997da89`, earlier shell | **347 passed, 0 failed, 3 ignored** | Historical source only; later candidate changed extensively. |
| Same earlier primary, optimized real native journey | **1 passed**, real 3,492 rows, 15 paired frames | Early Alias/Code captures still show predecessor Reader content; not settled-content, matrix or motion acceptance. |
| Later primary `549…`, shell | **352 passed, 7 failed, 3 ignored** | Preserved intermediate failures, not erased by later focused tests. |
| Isolated component scope equivalent to the older `549` tree | GPUI **322 passed**; Facet motion **108 passed**; component **510 passed, 1 failed** | Component oracle expected None instead of Some(false); later isolated focused repair passes one test, not a fresh complete 511-test suite. |
| Isolated library `4a47…` | **212 passed, 1 ignored** | Separate local-service and present compile attempts fail before tests (8 and 6 errors). |
| Candidate `3bab…` / primary `93d726…` final source | Syntax/diff review only for latest changes | **No all-target check, composed suite, fresh native or live product acceptance.** |

### 7.1 Latest geometry failure and repair

The `17a5` full shell failures record a covered Reader heading at `(17, 91)` for width 360 and `(64.5, 97.5)` for width 663 under Ask. Primary `7c2` applies the visual-subtree omission while keeping route/motion state, then passes all 16 focused fluid tests. This establishes that focused regression scope only.

### 7.2 Latest native attempt and source-only harness repair

The optimized `7c2` Ask journey compiled for 26m20 and stopped at `shell_capture.rs:855`, which accessed a nonexistent `CaptureRecord.ledger`. Fresh OUT and STATE remained empty. **There were no new screenshots to inspect.**

Primary `93d7260403d0ba7af44705dd7ac3a9e03ef2a0c1` repairs this by storing the exact after-paint probe ledger in `JourneyFrame`, enabling/installing the probe, clearing intermediate captures immediately before frame draw, and taking the ledger in the paired pixel/semantic hook. The assertion now reads the frame's latest stack. It is a small test-harness repair, syntax/diff checked only; it is not yet in candidate `3bab` and must be reviewed/cherry-picked when resuming.

The next native gate is the existing ignored `capture_the_shell_over_a_real_index` test with `NUDOX_CAPTURE_ONLY=orbit-ask-keyboard-journey`, a new fresh state/output directory, exact frozen source and the optimized profile. Run no more builds until resumption. Preserve failed compile artifacts and do not reuse their empty directories as though they were a successful capture.

### 7.3 Build-cap evidence has an explicit gap

Machine-wide limit is four actual Cargo processes. For the `17a5` shell run, monitoring failed after 191 initial samples because argv parsing encountered an unmatched quote; recovery added 31 ucomm-based samples. There is an explicit **104.864378-second monitoring gap**. Observed peak was one, but continuous cap compliance for that gap cannot be claimed.

The later fluid run has 86 continuous samples, observed peak two. The failed native run has 1,532 continuous samples, peak four. The final nonce-service run has 1,957 continuous samples, peak four; an earlier reported peak three was superseded by the final summary. No over-cap observation occurred in those continuous records. These are process counts, not RAM/RSS or throughput measurements.

## 8. Evidence locations and reproducibility

The append-only archive is local and Git-ignored:

`/Users/mileswirht/Documents/ChatGPT/backend-index-compiler-tentpole/.local/readiness/evidence/20261001-gui-state-and-live-gates/manifest.json`

Current manifest: **143 records**, **720 verified copied files**, SHA-256 **`318393e91746dc909dad3f334877d976ad8e501f90f43b02afbbe1f3226b0bcb`**. Previous manifest versions are preserved by digest, including the 122-record `620279e3…` and 136-record `4a49b113…` versions. This archive should be retained or exported before deleting local state; a Git documentation commit does not itself back up ignored artifacts.

The seven most recent appended records are:

| Manifest record | Raw run directory |
|---|---|
| `primary-17a5-shell-357-pass-2-fail` | `/private/tmp/backend-desktop-shell-primary-17a5a872-run01/` |
| `primary-7c2-fluid-16-pass` | `/private/tmp/backend-desktop-shell-primary-7c2e96a725-fluid-run01/` |
| `primary-7c2-native-ask32-compile-failure` | `/private/tmp/backend-desktop-shell-primary-7c2e96a725-ask32-run01/` |
| `source-d17-diagnostic-real-fixture-failure` | `/private/tmp/backend-cargo-source-diagnostic-gate-d17bce00.dfjktg/` |
| `source-e840-wrapper-fixture-compile-failure` | `/private/tmp/backend-cargo-source-diagnostic-gate-e8406823/` |
| `source-45c9-real-two-workspace-fixture-pass` | `/private/tmp/backend-cargo-source-diagnostic-gate-45c973c4/` |
| `source-262b-owner-nonce-service-six-pass` | `/private/tmp/backend-cargo-source-service-owner-nonce-gate-262b2eef/` |

Each record preserves completed output, exit status, source/tree/lock/dirty-state provenance and relevant monitor/receipt files. Prefer the copied archive over temporary directories for future evidence links. Failed artifacts must not be rewritten after a repair.

Selected latest raw-log digests: full shell `ad3d9e9237add29fb8f8c9214cb30249bbb3c3a683b6465cd72758a67a80b827`; fluid `1e792f27c2f8c8d57c7efc1259a29d41981ae2df8baa2352a3269ecffebfbb74`; native compile failure `a3c8b7304ca32d4a935c154724154e92d53eabd1d472ee3409b53287daa7c2e5`. The native provenance id is `1790912372951563000-20045`; its receipt digest is `ef63a031e97bcad8637b09b91c1b48c1fa3f9f2ba702009e300de91592e6ea8f`.

No new lib.rs/docs.rs/crates.io comparison screenshots or fresh Zeron/Zed/Hummingbird research were performed during the stopping pass. Earlier research and design inventories remain reference material; this brief does not claim a fresh beat-for-beat parity audit.

## 9. Index, ingest, persistence and all surfaces

This section reconciles the broader read-only readiness audit with the latest GUI work. It reports existing documented evidence, not newly executed runs at this checkpoint. Primary sources are [live index readiness](../../../benchmarks/live-index-readiness-2026-09-30.md), [tentpole measurements](../../index-tentpole-measurements.md), and [production readiness](../production-readiness.md).

### 9.1 Authored live Rust lifecycle is a narrow positive

Frozen bundle `1a6b54a8144fe50a5c0d1d1f6ab079b633929e9f`, built from primary `25a4e…`, runs a dependency-free authored Rust `BeaconEntry` lifecycle in about 26 seconds. Add/progress/publication/document/source work through CLI and one persistent MCP connection after owner stop/reopen. Explicit V2 cancellation preserves V1 and its exact receipt; no V2 appears after SIGTERM.

Its health reports frontend count 9/9 but semantic readiness **0/18** and embeddings **unconfigured**. This is not public-registry closure, all-language semantics, embedding readiness or full backup acceptance. Later public serde `build.rs` admission is refused by SourceScope; another seven-language run has Python QueueFull and Go unconfigured. Do not substitute frontend enumeration for semantic success.

The report is at canonical `.local/live-typed-publish-cancel-1a6b54/20260930T124643Z-59101/artifacts/report.json`, SHA-256 `a81fbda2270a63da1883bf98d2384ec753d5dff8eea2090e36304d2fef0f50f7`.

### 9.2 Historical public-pin Turso restore did happen

The audit found positive historical evidence under canonical `.local/live-turso/20260929T025610Z-62613/artifacts/`: serde 1.0.228 and serde_json 1.0.145 Add return zero; physical `VACUUM` backups of both databases reopen from main files without WAL; 4,355 rows and 17 edges support pinned-package/dependency queries. Health still reports semantic 0/18 and unconfigured embeddings.

This must not be erased by a later narrative saying no registry backup had ever been demonstrated. Conversely, the artifact directory has binary hashes but no identified frozen source commit/manifest, and later current-bundle public closure fails. The defensible conclusion is **a historical narrow public-pin restore positive, no current frozen full-closure acceptance**.

### 9.3 Measured query costs are scoped, not superiority claims

The historical Turso run measures new CLI processes, including startup and IPC, with 20 calls/three warmups while other builds are active:

| Operation | p50 | p95 |
|---|---:|---:|
| Package search | 52.296 ms | 56.592 ms |
| Registry discovery | 355.983 ms | 493.616 ms |
| Dependencies | 30.050 ms | 35.437 ms |
| Dependents | 29.460 ms | 34.575 ms |
| Direct Turso forward query | 12.156 ms | 13.326 ms |
| Direct Turso reverse query | 10.173 ms | 12.586 ms |

These do not measure isolated in-process index latency, production load, cold OS cache or a controlled before/after speedup.

Real small-corpus Maven search on source `259ea09eb237af272211ce7016eed201c69df21b` executes nine independent query chains before and after projection reopen. Build is 284.665 ms, reopen 14.009 ms, post-reopen chains 117.906 ms; 900 warm chains have p50 6.835 ms, p95 51.384 ms, p99 53.636 ms, with 234,941 projection bytes. No OS-cache flush or ranking superiority was established. An earlier Maven CLI measurement uses a different path and corpus and must not be combined with these numbers into one performance claim.

Frozen Maven paging provides 100 PURLs / 96 coordinates exactly once through CLI and the same persistent MCP connection over four owner restarts. Unacquired facts remain `caught_up=false`; that is truthful progress rather than a completed catalog.

The lib.rs comparison has no executed common quality result: the planned Cargo-only seven-row/nine-query corpus and pre-authored relevance judgments do not prove fetched archive metadata, ranking accuracy or cross-language superiority. Faster and more accurate than lib.rs remains an objective.

### 9.4 Ingest and metadata acceptance still required

Execute the new joined harness only after review and compilation, with official public pins and exact binary/source receipts. Then extend beyond a happy path: registry rate limiting, missing/retracted/yanked releases, prereleases, duplicate names across ecosystems, forged/inconsistent metadata, absent downloads, advisory changes, code-forge-only packages, interrupted archive transfer, dependency cycles, optional/platform features, incomplete closure, and partial availability.

Unknown download/advisory facts must remain Unknown rather than zero/safe. Yanked releases must remain addressable by exact historical identity without being offered as ordinary latest installs. Security and registry facts need provenance and freshness. Forge-only identity must not pretend to be a registry release. A package's source origin, requested environment and effective compile context must survive all three surfaces.

The earlier QA failures—persisted workspace re-admission, MCP calls resetting while a daemon lives, oversized generated JS files aborting ingest, missing shelf/onboarding and wrong-language header actions—remain acceptance scenarios until rerun against the current frozen product. Test ignored/build trees from actual JavaScript projects, multi-project selection without environment variables, C#/JS MCP sessions and Windows explicitly. This brief does not infer they are fixed from new types alone.

### 9.5 Publication and crash recovery remain a release gate

Index semantic publication and terminal operation receipts have been recorded as separate writes. A crash can therefore leave ambiguous operation state. The authoritative publication receipt must bind the exact operation atomically; guessing success from package presence or blindly resubmitting a Failed operation is insufficient. Exercise process death before/after data commit, projection update, receipt publication and client Ack. Turso concurrent control-barrier begin had an earlier race; no current successful gate closes it here.

The real backup requested for graph/package browsing must come from a running ingest after a meaningful interval, include provenance/watermarks, and be reopened independently. A synthetically populated SQLite file does not meet that request. No new w-graph task or thread is dispatched at this stopping point.

## 10. Compiler, durable IR history, storage and transport

The current documentation records important incomplete boundaries. These are the last audited/recorded gaps, not a claim that every historical branch was reread and rerun during this stopping pass.

### 10.1 Stable producer segmentation and persistent lineage

See [compiler IR lineage](../../compiler-ir-lineage.md), [durable typed IR history transport](../durable-typed-ir-history-transport.md), [generation materialization policy](../ir-generation-materialization-policy.md), and [compiler semantic parity](../../compiler-semantic-parity.md).

The last recorded production full NXFI image is segmented by ordinal 1 MiB chunks. Typed revision-3 manifest/stable-key verification exists, but the seven-family producer cutover from the older writer is not established. V1 durable history covers ordinal NXFI; V2 exact seven-family closure and local-cache certificates/deltas are not established as the production selection; V3 root/ref/worker selection and rename/resurrection lineage bridge are not established end to end.

Restoring persistent commit/replay must preserve lineage through edits, rename, deletion, resurrection and environment-dependent main-package changes. Local IR-VCS should choose among a pristine materialization, in-memory verified state and bounded delta replay according to actual changed families and cost, without accepting an unverifiable short path. Keyed dependencies must distinguish pure reusable inputs from environment/toolchain/feature-sensitive inputs. Embedding keys must include model/schema/input identity, not just source text.

The desired zero-copy and SIMD work must be measured at the producer, verifier, replay and client boundary. A packed representation or borrowed API alone does not establish no copies across network hydration, owned cache storage and UI preparation. Do not trade explicit ownership/validation for clever unsafe code without an independent oracle.

### 10.2 Work-avoidance experiments are not runtime acceptance

The Python materialization model demonstrates bounded exact-generation work counts, not a Rust implementation or stale/GC proof. A 100k-file metadata tree experiment reports 101,801 pages, 24 changed pages / 3,710 bytes, path-copy p50 0.0264 ms versus full 266.012 ms. This is a useful structural experiment, **not end-to-end compilation speedup, SIMD throughput or production RAM reduction**. Whole-capture scanning still rehashes files in the recorded path.

Compiler semantic parity remains incomplete. Go/Python have actual partial/file-scoped authority; other frontend rows may be semantic Unavailable or transitional typed relations. Test seven producer families, dependency/environment changes, selected history replay, local edit recompilation and remote publication independently before saying all languages work end to end.

### 10.3 Local storage proof gaps

The isolated `7aa7d5f02c` full store gate runs 323 tests: **315 pass, 8 fail**. GC replacement/recovery, parent-child commitments, staged cleanup, leases and atomic publication remain red. A different `9be2` scope later passes 323, but review still finds generic cross-store receipt acceptance without destination closure and oversized cold counts; this is not a composed index proof. Earlier `553335…` testing invalidates pathname redirection after a pin.

Keep store identity, object identity, closure proof, lease ownership and publication receipt distinct. Test corrupt/truncated objects, directory replacement, parent/child commitments, foreign-store receipts, oversized counts, staging crash, GC pin races and main-file-only reopen. Windows and abrupt process crash are open. No durable V3 Ack, multiobject history import journal or history-specific remote grant bridge is established by the recorded passing lower-level suites.

### 10.4 Disk and S3 must share semantics, not deployment assumptions

The recorded S3 experiment is loopback: 1,024 objects around 8 KiB, conditional PUT totaling 8,925,314 object bytes plus a 410,754-byte manifest and two 8,315-byte ranges, 9,352,698 application bytes total. Checked Merkle/Bao and capacity mechanisms are useful positives. It is not an AWS latency/cost/production benchmark; a concurrent point-path experiment fails without an RSS result.

The abstraction should expose typed content-addressed verified chunks, bounded range hydration, explicit publication receipts and resumable closure across both machine-local CAS and optional S3. The index may own only disk. Workers must not receive S3 credentials just to return verified compiler output. Partial sends need explicit staging and final closure, not a “file exists” success condition.

### 10.5 Iroh, Bao and actual installed deployments

Isolated `b565ad819…` runs 27 client tests over an actual controlled local wire; relevant files match primary `46ce6…`. Authenticated peers, deadlines and rejection are demonstrated in that controlled scope. This does not establish installed multi-machine index queries, compiler dispatch, load balancing or history hydration over a real private cluster.

The [deployment runbook](../../../operations/index-compiler-deployment.md) describes direct UDP without relay/public discovery/DNS, same-UID local sockets mode 0600, signed roots/operation grants, protocol 3/schema 2, and 16 KiB exact-head-fenced range hydration into a file semantic-range store with fsynced checkpoints. CAS admission requires complete verified coverage. A headless service UID and GUI login owner are distinct deployment contexts; do not assume their local socket is mutually accessible.

Acceptance still needs two actual machines: installation/configuration from a clean environment, authenticated peer setup, compiler reconnect and load/capacity routing, output streamed to index or optional remote store, forced partial transfer/restart, exact remote root hydration for client delta compilation, and observable refusal with bounded cleanup. The Iroh/Bao history-specific closure Ack and selected V3 bridge remain open. The existing runbook is useful setup material, not certification that this checkpoint is ready to deploy.

### 10.6 Embeddings and GPU

Recorded BEM2 external ABI, cache/duplicate fixtures and persistent-provider tests are bounded mechanism tests; earlier embedding and compile scopes pass 13 and 89 tests respectively. They do not establish an actual native GPU/model provider, durable changed-content reuse, RAM/VRAM budgets, batching throughput or local/remote embedding parity. The live health evidence still reports embeddings unconfigured; the Qdrant fixture has not run in the audited scope.

Measure unchanged/edit/dependency/model-change invalidation separately, including cancellation and provider descendant teardown. Do not claim optimized GPU embedding generation while the live provider is absent.

## 11. GUI design-system and journey acceptance still open

The detailed [remaining GUI brief](../gui-remaining.md) is the large original inventory, not a completed checklist. This checkpoint improves important admission, native ownership and source browsing foundations. It does not declare the comprehensive design rollout finished or visual/capability parity with crates.io, docs.rs and lib.rs achieved.

The main product view should still be package browsing modeled on crates.io, with docs.rs-level documentation/source capabilities and lib.rs-style discovery/relationships. It must be a useful multi-project local-first application, not the previous diagnostic dashboard. Collaboration is the explicit exception; the rest remains in scope. Native GPUI-CE/component reuse, deeply styled native controls and composable motion remain the implementation direction; no canvas shortcut is introduced by this checkpoint.

The following 19 acceptance areas organize the remaining broad inventory. They are a handoff grouping, not a claim that each group has a completed suite:

| Area | Required user-visible acceptance | Current limit |
|---|---|---|
| 1. First run / installation | Finder launch, discovery of toolchains, honest setup guidance, native project picker, no project env vars required | Dev-shell/live fixture success does not establish clean install. |
| 2. Project shelf / lifecycle | Multiple projects, exact pin/selection, add/remove/retry, progress while retaining readable publication | Source ownership work is substantial; real restart and cross-project flows remain open. |
| 3. Frame / header | All buttons native and useful, correct language/ecosystem, menus/links/disclosures, semantic names | New header/ecosystem source unrun in composed native product. |
| 4. Library / package inventory | Exact release/origin, bounded inventory, keyboard/virtualization/reflow and return focus | Typed source integrated; pending README and real owner matrix. |
| 5. Discover / Add | Registry and forge discovery, partial facts, dependencies, failure recovery | Joined seven-pin harness is source only. |
| 6. Package page | Rich truthful metadata, README, versions/features/platform, download/yank/advisory states | No common visual/capability acceptance against reference sites. |
| 7. README / Markdown | Links, anchors, external policy, prepared rendering, responsive large content, selection/accessibility | Independent README patch unreviewed; ordinal focus and UI parse risk. |
| 8. Symbol documentation | Exact modules/source, variants/uses/neighbors, semantic versus structural truth | All-language semantic parity remains incomplete. |
| 9. Code / source | Exact bytes/context, line navigation, gutters, copying, wrapped/large text and native roles | Old source-gutter semantic omission remains an actual native scenario to rerun. |
| 10. Find | Editing, keyboard results, pending/refused states, exact query/owner membership | 36-frame live journey unrun; pending Ask return follow-up isolated. |
| 11. Compare | Exact current visit, retained stale closures inert, releases/environment scope | Source tests written; same-root mounted regression unrun. |
| 12. Graph / world / hand | Real index graph, reach, selection, tour, interruption and native actions | Prototype/fixture history must not be relabeled real graph acceptance. |
| 13. Releases / upgrades | Real history/diffs/change marks, honest unavailable states, return to pinned version | Older fixture release mechanisms require current real-data verification. |
| 14. Inbox / status / recovery | Discoverable actionable progress/refusal/security, no raw transport wall as primary UX | Latest source does not clear all original status/design gaps. |
| 15. Ask | Cmd-K, actual native input, preview/commit, escape, interruption and no focus theft | Latest 32-frame run never rendered. |
| 16. Settings | All actual controls, toolchain/MCP setup, persistent state, hidden flows | Source native actions exist; complete real flow unaccepted. |
| 17. Menus / lenses / peeks | Discoverable keyboard/pointer entry and dismissal, no stale action admission | Cross-flow matrix remains to run. |
| 18. Responsive motion / typography | Width changes, collapse/drawers, zoom/text sizes, smooth interrupted motion, themes/reduced motion | Focused fluid pass is not native matrix/animation acceptance. |
| 19. Accessibility / platform / performance | Native semantic controls/focus/IME, startup/frame budgets, Windows/macOS, localization where promised | Isolated GPUI passes do not establish full app/platform acceptance. |

### 11.1 Screenshot harness requirements

Capture must pair actual GPUI pixels, accessibility tree, route/root/attachment/page identities, focused native node and motion/probe ledger at the same after-paint point. A semantic snapshot without pixels, a seeded route without real input, or a screenshot before content settlement is insufficient. The primary `93d726` ledger repair is intended to preserve this pairing, but is unrun.

For each flow, use actual keyboard/pointer events and current semantic bounds. Wait for a bounded, explicit content-ready and motion-settled condition; then assert exact content in pixels/native tree. Preserve intermediate animation frames as well as endpoints, with timestamps. Capture interrupted/reversed motion and resize during transition, not only a nominal final frame. Use the available test-support/ffmpeg path for deterministic frame sequences where it exercises the real render path; encode success cannot substitute for examining frames.

Root must inspect the full screenshots **and** component/control crops. Review spacing, clipping, line wrapping, text hierarchy, hover/pressed/focus states, duplicate visual nodes, ghost predecessors, scrim/plate clearance and collapsed controls. Evaluate content and action semantics together. Previous 15-frame acceptance passed route/focus while early Alias/Code still painted predecessor Reader content; this is precisely the false positive the new harness must catch.

Minimum matrix includes the demonstrated narrow edge cases (360 and 663), planned 480 and 1,440 widths, wider desktop and rapid threshold crossing; text sizes 100/150/200%; expanded/collapsed navigation; themes and reduced motion. Include empty/pending/partial/fault/current/retained/owner-replaced data; keyboard and pointer navigation; focus return; native input editing/IME; Back/Forward; cold restart and restored routes. Add platform-specific captures where behavior differs. No complete matrix has run at this checkpoint.

### 11.2 Reference comparison and responsiveness

Use consistent viewport/text scale/content when comparing crates.io package view, docs.rs documentation/source, and lib.rs discovery. Capability parity must include hyperlinks, real version/platform/features, search and metadata—not just palette and three columns. Zeron, Zed and Hummingbird research should inform lifetimes, composable motion and responsiveness rather than copying architecture uncritically.

Component-first iteration remains useful: native controls, typography, list rows, disclosures, Markdown, header, shelf and overlays should each pass stress cases before composing the full shell. Use gpui-component facilities instead of parallel reinvention when they fit; vendor a narrow change when needed, with upstream divergence documented and independent tests. Measure actual input-to-visible response and frame cost while ingest and result landing are active. The ReadPool gap means responsiveness is not yet proven under load.

## 12. Build and experiment discipline when resuming

Use the saved shell instead of re-evaluating `nix develop` for each lane:

- Saved environment: `/Users/mileswirht/Downloads/backend/.local/devenv/development.sh`.
- Bash: `/nix/store/5vmd3cqj6skjajg0yj9jl8dsddwp0700-bash-5.3p9/bin/bash`.
- Rust 1.97.1 tools: `/nix/store/ff5chd1i7bm0d7ki0ahkbkgwij973qvx-rust-1.97.1-with-components-2026-07-16/bin/`.
- Current protocol-B wrapper: `/private/tmp/nudox-current-cargo-wrapper-20261001-b/cargo-wrapped` and `cargo-rustc-cache`.
- Shared compiler cache: `/Users/mileswirht/.cache/nudox/cargo-1.97-protocol-b`.

Use locked/offline gates where appropriate, one compiler job per lane, incremental disabled, and the existing exact run recipes/receipts for environment-specific flags. Native capture needed the size-optimized profile because the unoptimized ARM64 test binary failed branch-range linking. A typecheck does not prove the optimized native binary links.

**Four actual Cargo processes globally is the ceiling**, including nested Cargo metadata and unrelated active lanes. Count `ucomm == cargo` from `/bin/ps -ww -Ao pid=,ppid=,ucomm=`; do not parse shell-quoted argv. Reserve headroom for outer plus nested Cargo (begin only with suitable free slots; the previous discipline used at most two already active). Other agents may inspect/edit while awaiting build turns. Do not kill unrelated processes to create a slot.

Share the protocol-compatible compiler cache, not an arbitrarily relabeled target directory. Keep each target owned by its exact worktree/source graph; preserve markers and toolchain/profile receipts. Reuse a warm target only with that provenance. Never label a foreign worktree's cached result as a clean candidate gate. This checkpoint did not introduce a new shared-target locking workaround.

Every run needs fresh unique artifact output, frozen head/tree/lock/dirty-state before and after, command/profile/toolchain, raw output/exit status, workload and state identity, exact process cleanup and an immutable completion receipt. Record gaps and failures. Do not expose secret environment values in logs. No further build is authorized by this document during the requested stop.

## 13. Ordered continuation plan

### Stage A — review and compose source without claiming acceptance

1. Confirm workspace state against the snapshots. Preserve canonical's Nix edit and the unresolved original cwd; continue in the candidate.
2. Root-read complete final diffs for producer expiry, observer fixes, README navigation, Find follow-up, joined harness and the two older test slices. Check their preconditions against the candidate and discard/supersede stale test-only changes with a recorded reason.
3. Compose observer fresh-admission/reset/cancel and producer reclamation together; add tests for both receiver withdrawal and producer release. Keep exact protocol/version/limit semantics coherent across lockfile scopes.
4. Compose README with full Cargo binding, current-read/deferred leases and the shared Markdown lifecycle. Replace ordinal focus identity where necessary; prove persisted addresses cannot grant authority.
5. Compose the Find/Ask return correction and mounted pending/Compare tests, then the primary `93d726` frame-ledger harness repair.
6. Repair required-content failure precedence from `RouteDependencies`, with the optional dossier combinations in §6.1.
7. Implement the bounded ReadPool proposal as its own reviewable slice, or explicitly leave memory/responsiveness acceptance blocked. Do not add a superficial queue size assertion without lifetime and fairness proof.
8. Keep atomic commits with explanations of problem, resulting behavior and validation scope. No source fast-forward to primary or canonical before the next gates.

### Stage B — compile once, then meaningful focused and composed gates

9. Obtain a build slot and run the exact candidate typecheck/required target graph. Repair compile errors in atomic commits; retain each failed run. Test-harness parsing is not a substitute.
10. Execute targeted phase/admission, route persistence, queued intents, owner same-root replacement, source-authority, cancellation/reclamation, README link, Find/Compare and ReadPool interleaving tests.
11. Run the full desktop shell, relevant runtime/model suites and affected vendored component/GPUI suites on the exact final composition. Repeat only after new changes or an unresolved concern; do not broaden testing mechanically to obscure a known failing gate.
12. Compile and execute platform-specific producer capture/cleanup on Windows before platform claims. Keep macOS/Unix results separate.

### Stage C — actual native input and pixels

13. Execute the optimized Ask 32-frame journey from fresh state, then inspect every paired frame/tree/route/ledger and all relevant crops. Require settled Alias and Code content, resize/interruption and exact native focus—not merely correct route ids.
14. Execute the Find/Settings 36-frame journey, native Library/header/README/Compare paths, and the width/text-size/theme/reduced-motion matrix. Add any missing real-input path rather than seeding a route to bypass it.
15. Measure startup, input latency, UI result landing, frame timing and peak memory under live ingest, partial result floods and large Markdown/source. Set budgets from observed workload and design goals, with baseline and environment recorded.

### Stage D — one real owner, all surfaces, restart and backup

16. Review/run the seven-public-pin joined harness. Keep one MCP connection through cold owner restart and physical Turso backup/restore; establish exact same owner/state/binaries for CLI, MCP and GUI consumer.
17. Run the GUI against that ingesting index, including NuGet/C#, JS generated/ignored files, project shelf, actual picker, settings/MCP setup and source/docs hyperlinks. Verify restored state without pre-admitted fixtures or setup-only environment variables.
18. Extend registry/failure metadata and semantic/embedding scenarios. Require truthful incomplete states and per-package results while ingest continues. Preserve an actual ingest-derived SQLite backup for graph iteration.
19. Exercise abrupt process death and authoritative publication receipts across index, store, history import and client restart. Do not accept “reopens” without exact object/publication/operation checks.

### Stage E — broader tentpole completion and release

20. Close stable seven-family producer segmentation and selected persistent IR-VCS lineage/replay/materialization, then local edits/environment changes using remote IR. Prove bounded work and correct unchanged/rename/resurrection lineage with independent output oracles.
21. Test two installed index/compiler machines over Iroh/Bao with partial-send/cancel/reconnect/load scenarios, disk-only and optional S3 destinations, and client hydration. Configure/measure a real embedding provider/GPU before performance claims.
22. Execute common search quality/latency corpora and controlled work/memory benchmarks. Publish limitations and unsuccessful experiments; do not claim best-in-class from small warm fixtures.
23. When the composed product actually passes these gates, commission the requested final adversarial user-flow review, including hidden settings, all surfaces and persistence. The earlier requested Astra high final review belongs here; no new Astra reviewer was started at the stopping point. Incorporate its findings before release.
24. Only then promote code to primary/canonical with the external Nix edit preserved, verify the exact promotion tree, and follow the user's current push authorization. This stopping checkpoint performs documentation commits only and does not push.

## 14. Delegation and review rubric

Resume with scoped worktrees and clear ownership. Sol is authorized for GUI implementation; Luna implementation/adversarial review was explicitly requested, with xhigh implementation and max review preferences. Do not dispatch new work while paused. Do not create user-owned chats as a substitute for internal agents.

Each implementation assignment should include exact base/allowed files, required typed identities, invariants, failure schedules, real data/input requirement, reviewable atomic commit boundary, build-slot protocol and the evidence needed to clear it. Require an explicit list of unrun checks and unsupported platforms. Source edits that fail compilation must be reported as such, not as a completed slice.

Each independent reviewer should challenge:

- **Authority:** exact owner attachment/root/binding/visit at paint, event and deferred flush; saved addresses and seed data never grant live authority.
- **Lifetime and memory:** capacity includes worker and queued payload residence; cancellation, partials, cloning, retries, teardown and slow consumers release exactly once.
- **Work avoidance:** unchanged input avoids the claimed work; cache key includes all purity/environment/model dimensions; cold/restart behavior verifies witnesses rather than trusting a cache hit.
- **Failure:** partial send/encode, process death, stale generation, ambiguous publication, malformed/oversized data, replay and same-root replacement have typed visible outcomes.
- **GUI:** true native focus/action/semantic roles, inert retained subtrees, motion interruption, resize/text scale, settled content and actual hyperlink traversal.
- **Abstraction:** one dependency/admission plan is reused rather than duplicated readiness/watch/display state; boundaries expose domain contracts rather than transport/debug details.
- **Evidence:** independent oracle, actual execution on exact composed source, no erased failure and no fixture-to-production claim inflation.

Dan Luu's testing notes were requested as inspiration earlier. The operative testing direction here is concrete: independent oracles, deterministic bad schedules, preserved failures, actual installed/real-data paths and measurement of the workload users experience. This stopping pass performs no fresh article research. Avoid implementation-mirroring assertions that merely restate the code under test; use exact content/lineage/root receipts and mounted user actions to falsify it.

Root's final integration review should read the whole selected patch, not just the agent summary or changed type names. Cross-slice behavior—particularly authority, optional versus required data, focus return and capacity—is where the latest real defects were found.

## 15. Open gates at a glance

| Gate | Status at stopping point |
|---|---|
| Save agent work and freeze active builds | Done; clean source checkpoints, no Cargo observed in final query. |
| Archive latest completed evidence | Done: 143 records, 720 copied files hash-verified. |
| Compose selected reviewed GUI source | Done in candidate: 116 atomic commits, uncompiled as a whole. |
| Root-review all late isolated source | Open. |
| Required Tree failure precedence | Confirmed defect, unfixed. |
| GUI completed-read residency / fairness bound | Audited proposal only; unimplemented. |
| Composed observer fresh admission + producer expiry | Saved separately; review/type/runtime gates open. |
| Independent README/link lifecycle | Saved source; review and execution open. |
| Full shell on current final composition | Open. Focused 16-test fluid pass is earlier primary only. |
| Latest actual native Ask32 capture | Failed before execution; source-only repair saved in primary. |
| Find36 and comprehensive resize/motion/native matrix | Open. |
| Seven official registries joined CLI/MCP/GUI restart/restore | New harness source only. |
| All-language semantics and real embeddings | Open; latest live health semantic 0/18, embeddings unconfigured. |
| Persistent selected IR lineage / stable producer cutover | Last recorded production gaps remain open. |
| Store/GC/publication crash safety | Recorded red gates and proof gaps remain open. |
| Installed multi-machine Iroh/Bao/index/compiler/client | Controlled local-wire positives only; deployment acceptance open. |
| Search faster/more accurate than lib.rs | No executed common quality/performance comparison. |
| Canonical code merge/push | Not performed at this checkpoint. |
| Production-readiness goal | Paused at user request; not achieved. |

## 16. Further source documents and handoff limits

Use these documents alongside this checkpoint:

- [Production readiness ledger](../production-readiness.md): historical issues, actual gates and preserved failures.
- [Full remaining GUI inventory](../gui-remaining.md): the comprehensive design/system rollout brief; its old item statuses need current flow evidence before closure.
- [GUI architecture](../../../operations/gui-architecture.md) and `apps/desktop/ARCHITECTURE-V3.md`: snapshot, reducer, actor and motion boundaries.
- [Markdown responsiveness review](../../../reviews/sol-gui/markdown-responsiveness.md): preparation/lifecycle mechanism and its original scoped evidence.
- [Live index readiness](../../../benchmarks/live-index-readiness-2026-09-30.md): broader live failures, authored lifecycle, registry/semantic limits.
- [Index tentpole measurements](../../index-tentpole-measurements.md): structural/search/storage experiments and their scope.
- [Compiler IR lineage](../../compiler-ir-lineage.md), [durable typed history](../durable-typed-ir-history-transport.md), [materialization policy](../ir-generation-materialization-policy.md), [compiler parity](../../compiler-semantic-parity.md): existing foundation and remaining producer/history cutovers.
- [Deployment runbook](../../../operations/index-compiler-deployment.md): current setup contracts, to be validated on real machines.
- [Workspace snapshot](workspaces.json), [additional test-slice snapshot](additional-stopped-test-slices.json), [116-commit inventory](integration-commits.tsv): exact local source preservation.

This brief is exhaustive about the current continuation state and known open gates gathered from the root's integration work, agent freeze reports and broader readiness audit. It is not a fresh line-by-line audit of every historical worktree, a new production benchmark, or a claim that all old readiness issues remain unchanged. The next pass must confirm old issues against the exact composed product and record both closures and new failures with the same precision.
