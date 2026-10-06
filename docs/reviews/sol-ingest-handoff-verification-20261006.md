# Ingest capture and semantic-shape verification handoff, 6 October 2026

This follows the repository history audit. The review scope is the full source-facts/capture cohort in `codex/nudox-ingest-facts-20261005`, prepared-intent identity, selected semantic freshness, shape payload units, and real public package lifecycle across cold reopen. Root owns final integration. This is a separate report from acceptance of the combined product candidate.

## Checkpoints and build custody

The lane started clean at `f73a0076f728ec23e070e03924e8c8b7b055cf98`. Root's unchanged-primary-root lazy-closure repair `3e34f658c6a1e18148761b529a3aa16db1a18b6b` was absent, so it was applied as `232c168f21051d5e8a0763f5368466bcafeb5232`. Its patch is already present in the corpus-control candidate under Root's commit. The lane stayed frozen during its guarded test build.

Before running, the worker inspected live Cargo processes and their working directories. Other source-frontier, TypeScript, and current-Mac lanes were active; none was building this ingest worktree. The existing `ingest` role and `.nudox-cargo/slot-0` were reused. No parallel Cargo invocation was started in this lane and no guard was bypassed.

The wrapper is `/Users/mileswirht/Downloads/nudox-active-20261005/build-tools/cargo-wrapped`. The warm role is `/Users/mileswirht/Downloads/nudox-active-20261005/luna_ingest_pages_recovery/.local/build/ingest`, and the target receipt directory is that worktree's `.local/target/.nudox-provenance`. The pinned Cargo/rustc runtime is `/nix/store/ff5chd1i7bm0d7ki0ahkbkgwij973qvx-rust-1.97.1-with-components-2026-07-16/bin`.

The first exact command was:

```sh
CARGO_BUILD_BUILD_DIR=/Users/mileswirht/Downloads/nudox-active-20261005/luna_ingest_pages_recovery/.local/build/ingest \
CARGO_BUILD_JOBS=2 \
/Users/mileswirht/Downloads/nudox-active-20261005/build-tools/cargo-wrapped \
  test --locked --offline -p backend-local-service --lib \
  paged_source_facts_and_typed_semantic_refusal_survive_cold_capture_reopen -- --nocapture
```

Its log is `/tmp/sol-ingest-paged-cold-20261006.log`. The provenance token is `1791271401090397000-42636`, with HEAD `232c168f21`. A cached executable from before `f73` was deliberately not used as evidence for this checkpoint.

## Review conclusions

### Checked capture basis and one-shot preparation form a dependency cohort

The compact source row carries checked identity and a truthful bounded-retention summary. Complete facts are persisted separately as an exact manifest and pages. The cold test builds 900 actual TSX functions with the syntax frontend, compares the complete declaration set and reconstructed locations after reopening the store, and requires the compact summary to report incomplete retention. This proves the complete-facts persistence path, rather than treating a truncated row as a complete source inventory.

BPI9 adds an exact selected basis: workspace root, workspace sequence, closure identity, and capture relation root. Cold admission authenticates the basis through selected publication history, then checks the predecessor relation roots. A structurally valid older closure, a same-manifest/current-sequence hybrid basis, and legacy BPI6/BPI7/BPI8 capture intents without the required basis are rejected. A snapshot paired with a later store is rejected before it can lend persisted transition authority.

`PreparedBuiltinIntent` owns the final bound intent and request identity. Preparation adds the selected basis before hashing. Commit consumes the prepared object, checks the current root/sequence/closure, and verifies the returned request identity and exact next sequence. `finish_add` inserts the operation key before preparation, so the durable operation journal and owner commit use the same final intent identity. The committed capture collection is retained before later scan/admission work can fail. These pieces should be integrated together: partial cherry-picks can reintroduce identity disagreement or leave durable capture rows Pending.

Root's lazy-closure patch is required for capture-only transitions. An explicitly supplied typed root matching the target schema/version remains a retained target even when the primary manifest root does not change. The cold test exercises this boundary with an unrelated capture-only transition, then checks that the selected predecessor binds the actual closure and sequence.

### Selected freshness and image extent have independent checked abstractions

`SelectedSemanticPublicationKey` is a private checked capability obtained from the live selected publication. The freshness query requires it. A historical generation key does not silently qualify as a selected key. The public version query reconstructs the selected publication key before asking for current freshness. The minimal behavioral freshness repair is already in corpus-control; the capability type in `7b2fad0ca2` makes that requirement explicit.

`SemanticVersionRecord.semantic_bytes` measures encoded manifest bytes. It is not an image payload bound. `f73a0076f7` introduces `SemanticImagePayloadBytes`, checks nonzero/overflow/residence limits when summing the exact selected image authorities, and binds each shape's image authority/profile to that checked payload extent. Wire/product admission compares image length against the same-unit payload bound. Restoring a `ViewRevision` to `ViewStateRoot` conversion would undermine the checked-root API and is not an appropriate fixture repair.

### Crash boundaries retain uncertainty honestly

The index-operation journal independently stores acceptance, source-capture base, source-capture receipt, preparation, and terminal publication/failure. A post-selection receipt write failure leaves Prepared durable and permits checked reconstruction. A live terminal outcome with a durable Pending capture receipt is reported as `ReceiptPersistenceFailed`; a cold owner interrupted before recording semantics remains `SemanticWorkInterruptedAfterCapture`. A missing exact predecessor after the head advances is unresolved rather than inferred from a merely plausible current root.

`b68c0e4678` preserves the primary typed compiler refusal while appending an additional source-capture terminalization error. Its existing `index_operation_failure` returns only reason/detail, so the separate Sol CLI/MCP worker is repairing the typed `PackageCompilerFailure` journal/status propagation. The patches directly overlap in that helper and failed-transition callers. Root should combine the typed terminal field with this lane's distinct Published/None diagnostics, `pending_capture_unresolved`, and `capture_terminalization_failed`; neither whole-file replacement is safe because compiler fault vocabulary has advanced in corpus-control.

The source-facts unit lifecycle injects `BuildError::InvalidOccurrenceSpan` as a controlled typed refusal. It is a cold authority/persistence proof, not a real package compiler acceptance result. Real Cargo package lifecycle evidence is tracked separately below.

## Confirmed fixture defects

The prior `f73` guarded library-test receipts exited 101 without source changes. Compiler diagnostics identify:

1. `crates/library/semantic_shape.rs`, `response_admission_preserves_exact_root_source_and_selector_order`: `request.basis().into()` attempts the removed `From<ViewRevision>` conversion. The fixture request was created from `crate::view_state_root(&[])`; it must reuse that checked root.
2. `crates/library/surface.rs`, publication receipt roundtrip fixture: `IndexOperationStatus` omits `source_capture`.
3. The same omission in the malformed nested receipt admission fixture. Both fixtures should explicitly use `source_capture: None`.

These are compile-time fixture/API defects. They do not justify restoring unsafe authority conversion or dropping the source-capture status field.

All three were repaired in `6d801caaeabb85dbc1dd6498b937bf6c607119cb`, a two-file patch with three additions and one deletion. This changes test fixtures only.

## Ordered integration cohort

Against corpus-control `1aedf4ca773fa902ce94e074acb886f8d3ccc291`, the following chronological sequence preserves both production dependency changes and the associated repaired test fixtures. Root should apply atomic diffs and reconcile conflicts against the current closed compiler diagnostics.

```sh
git cherry-pick \
  f6a5701bb1 4d2d0d46c3 82afd4e4ef 28d0413543 3c55b161c1 \
  92edd85f73 c83f74f4c9 eb4e37f125 d27b870bf2 f5b58886d0 \
  e07c423438 0d5132954e e87c2f0c9a 488fc5e51c ee4ed42432 \
  4807e1ae5f 0cfd8824c7 4302466e88 b343a77fa9 e0698d9e72 \
  a59db0d570 a23150faaa 1121d895d7 42c795dd9c 2e334ee03b \
  30ca8837eb 4bdbc85209 670c5d847a b68c0e4678 ada4816181 \
  8ea9dcd250 7b2fad0ca2 f73a0076f7
```

Append the fixture repair `6d801caaeabb85dbc1dd6498b937bf6c607119cb`. The public lifecycle gate `07b9520eb8ac1b4a8baeeb2cf960ed367b141231` completed with a real member-authority failure described below. Preserve the assertion and test; this is a regression gate, not an accepted result.

Do not repeat patch-equivalent `c0cd79ef7b`, `a8251eeb4a`, `ce03df5f7e`, or `232c168f21`. `0cfd8824c7` is not wholly equivalent to the registry repair already in corpus-control: it also consolidates registry use in new BPI9 preparation/admission paths. Preserve those hunks.

The checked selected-key type and manifest/image-unit repair are independently reviewable after the existing freshness fix, with their fixture repairs. The authenticated BPI9 basis, paired snapshot/store API, prepared capture identity, final operation identity, and durable terminal capture updates are a cohesive foundation. Integrating only the late repair commits without that foundation is insufficient.

## Execution evidence

| Gate | Exact source | Result and boundary |
| --- | --- | --- |
| Complete paged facts and BPI9 cold capture reopen | Clean `232c168f21051d5e8a0763f5368466bcafeb5232`, tree `1845b50000b23c249b312592ff5103df2738779a` | PASS: one test, 971 filtered, 6.37 seconds runtime. Actual TSX syntax producer and persisted store reopen; controlled typed refusal, not real package compilation. |
| Original public real Cargo lifecycle | Clean `07b9520eb8ac1b4a8baeeb2cf960ed367b141231` | FAIL: initial package publishes and callable/carrier facts pass, but `MorningSignal` aggregate has zero members instead of its declared `pulse` field. No cold/same-add/refused-refresh phases reached. |
| Final member producer/common/native proof | Clean `821d5704f40da159c3659290653352b41c23144d`, tree `d66ec1f0bdb82b12a0879d87a4616b309f8a7fd4` | PASS: five focused tests, including actual OXC/Ruff/libclang and a closed Go image fixture; repeated observations are bounded by unique valid emitted IDs. |
| Reopened-image known-empty versus unavailable reader | Clean `77b056723ef591285cda2cca4e9fce1b21e078bf` | PASS: one actual encoded/reopened-image test. Reader code is unchanged at final `821d`. |
| Repaired public real Cargo lifecycle | Clean final `821d5704f4` | Warm Published/source-capture Published/name/callable/carrier/aggregate assertions PASS, including exact `pulse` field and type. Overall FAIL at initial owner report: one owner failure from deliberate stale-generation probe. No cold/same-add/refused-refresh phases reached. |

The first complete receipt is `.local/target/.nudox-provenance/1791271401090397000-42636.json`: 07:23:21–07:49:27 UTC, Cargo status 0, `source_changed_during_build=false`, identical before/after source digest `5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456`, and unchanged Cargo.lock digest `8467227b9623f00e3e4d1950333a97bcc8c869c179d0847befd26affedf2da0c`. Receipt SHA256 is `4be502ff8ac1be4bb9e886601e79092de0ebc80d4e26c2f02791c269b3055055`.

The executed local-service test binary was `.local/build/ingest/.nudox-cargo/slot-0/debug/deps/backend_local_service-dae9c947bddaefd9`, SHA256 `63762bc215815bcd33e5d7c60d92c9c6fad66df7ab49277c0c48056aa3215891`. The first guarded build took 25 minutes 57 seconds. It emitted existing warnings and an Apple linker unwind-table-size warning; none was suppressed.

The public test uses this exact command in the same role:

```sh
PATH=/nix/store/ff5chd1i7bm0d7ki0ahkbkgwij973qvx-rust-1.97.1-with-components-2026-07-16/bin:$PATH \
CARGO_BUILD_BUILD_DIR=/Users/mileswirht/Downloads/nudox-active-20261005/luna_ingest_pages_recovery/.local/build/ingest \
CARGO_BUILD_JOBS=2 \
/Users/mileswirht/Downloads/nudox-active-20261005/build-tools/cargo-wrapped \
  test --locked --offline -p backend-local-service --test index_operation_lifecycle \
  public_index_operation_replays_and_conflicts_across_restart -- --nocapture
```

Its log is `/tmp/sol-ingest-public-cold-lifecycle-20261006.log`. It runs a real authenticated embedded owner and public client, not an acquired-source or helper-only substitute. The test deliberately rejects Published or Unresolved for the invalid dependency refresh, requires the source-capture terminal receipt to retain the previous generation, and queries that exact selected generation as Historical after refusal. Typed `PackageCompilerFailure` on the durable operation status remains a separate Sol CLI/MCP repair until Root joins it.


## Newly confirmed member-authority regression

The public Cargo test at clean `07b9520eb8` exited 101 after 19.90 seconds runtime, following an 8 minute 14 second build. The initial package genuinely reached Published with terminal Published source capture, and public name, callable and parameter-carrier queries passed. The next unchanged aggregate assertion at `crates/local-service/tests/index_operation_lifecycle.rs:380` expected one member for `pub struct MorningSignal { pub pulse: u32 }` and observed zero. The failure fixture is `/tmp/b-VxcsnS`. The test did not reach cold shapes, same-add, refused refresh or cold failed-receipt replay. None of those later phases can be counted as passed.

Receipt `.local/target/.nudox-provenance/1791273171086604000-67861.json` records 07:52:51–08:01:27 UTC, status 101 and no source change. Receipt SHA256 is `8aaa458f26123c0cc7cfc00d16f224c447b41f1ed207209caa95ab628318a602`. The executed binary was `.local/build/ingest/.nudox-cargo/slot-0/debug/deps/index_operation_lifecycle-0c400900acdd4380`, SHA256 `e5f7e3d39accfe3f8e6a3d71f96940909db4ddf08cca9030aefbbcb32fffee33`. The wrapper's clean-tree dirty digest is paired with the exact Git HEAD/tree; it is not treated as a full source-tree commitment on its own.

This is the same missing producer contract in TypeScript, Python and Go, rather than a Rust-specific shape problem. `49fd2011ab93d020da5070413880739b28be99fb` introduced the default-Unavailable member capture flag at 10:05 UTC on 4 September. `135bc439769113221ef49d235e296ecc9ab97058` then gated owned-IR member scatter on that flag at 16:16 UTC. TypeScript, Python, Go and Rust emitted declarations and bound parents without completing the member capture flag, so their reachable owned member lists disappeared. The old `compiler/driver/lower.rs` diff explicitly replaces unconditional parent-to-child scatter with the Captured guard. The later path rename is `c37daab79c` into `crates/engine/src/driver/lower.rs`.

The public false claim was added by `76c7f7e3ea8` on 3 October at 23:53 UTC: the aggregate reader accepted an empty entity list without consulting `entity.authority.members`. `9808bca63a2` changed iteration style on 4 October, retaining that omission. Absence of an inventory proof therefore became Available(Aggregate([])) rather than a typed unavailable result.

Root authorized a coherent producer contract repair. `43a1e23b6cd433cf25ee9704dde2199d49e40c37` freezes explicit direct-declaration inventories, independent of containment; inherited/effective/runtime structural membership remains outside this plane. Unavailable stays the default. Duplicate observations canonicalize idempotently, conflicts retain bounded typed counts and the first differing member, and supplied member ownership is checked before mutation. The reader returns `MemberInventoryNotCaptured` for an unproven aggregate. OXC body sites, Ruff class-body proof metadata, Go explicit signatures/non-promoted methods and Rust HIR struct/union field lists are the producer proof sites. Unsupported or incomplete scope remains unavailable.

Root's adversarial review found a compatibility defect in the first repair: Clang's existing complete nonempty native inventories used `mark_members_captured`, whose revised empty adapter would erase them. C# at that checkpoint used the marker only after proving no native declaration names the row as owner. The completed `77b` correction below passes Clang's explicit native declaration-to-representative inventory and states the proven-empty C# inventory explicitly. No arbitrary parentage inference is restored. Final focused source and reader gates passed; the actual public lifecycle remains incomplete at the later boundary described below.


### Focused repair checkpoints

The first member checkpoint `43a1` engine gate (`/tmp/sol-declared-members-engine-20261006.log`) completed at 08:31:45 UTC, receipt `1791275341271242000-26700.json`, with status 101 and no source change. It did not run the new tests: three unrelated cohort fixtures failed compilation. `semantic_capture_relation.rs:635` must use `backend_library::interface::CompilerAttempt`, and two `file_facts_relation.rs` page visitor fixtures call `source_declaration()` on an already decoded `SourceDeclaration`. Those three test-only corrections are committed separately as `9be3b5b143b31ebd2f259c2d7ff765db15338ca4` (one addition, three deletions).

`77b056723ef591285cda2cca4e9fce1b21e078bf` fixes the reviewed compatibility defect by capturing Clang's exact native declaration-to-representative list; missing representatives leave it unavailable. C# now explicitly supplies its proven empty list. TypeScript checks the parameter property's producer-owned class coordinate rather than any nested descendant span. Its fixture includes an outer class containing a nested constructor parameter property, requiring the outer direct method inventory to remain captured.

The next focused gate ran at clean `77b056723` / tree `7a26249bc405bcd88a614a79825e96262d7bdc4c`: guarded `test --locked --offline -p backend-engine --lib declared_member_inventory -- --nocapture`, same ingest role, jobs2. The native Clang preservation fixture selects `/nix/store/0ypy29sk95sj1nv9gybidhxp4871a3v8-clang-22.1.8/bin/clang` and `/nix/store/k96r415w891gpaym31jb6zjkwady7mfd-clang-22.1.8-lib/lib`, an explicitly probed matched 22.1.8 runtime. It completed PASS: 5 tests, 762 filtered, 0.50 seconds runtime; receipt `1791275667489332000-46268` records 08:34:27–08:38:50 UTC, status 0, no source change, SHA256 `885581fd9acb6c68b167790fec8c11ba9efe38e1dc67f1c9fcdbd1fc297152ca`. The TS and Python cases use actual OXC/Ruff source; the Go case is a closed image fixture; the Clang preservation case uses actual libclang. This is producer/source proof, not product acceptance.

Append all proof and repair commits, rather than cherry-picking only production changes:

```sh
git cherry-pick 6d801caaeabb85dbc1dd6498b937bf6c607119cb \
  07b9520eb8ac1b4a8baeeb2cf960ed367b141231 \
  43a1e23b6cd433cf25ee9704dde2199d49e40c37 \
  9be3b5b143b31ebd2f259c2d7ff765db15338ca4 \
  77b056723ef591285cda2cca4e9fce1b21e078bf \
  821d5704f40da159c3659290653352b41c23144d
```

The member inventory abstraction and producer/reader repair are independently reviewable from BPI9 and do not touch the store/query or index-operation adapters. The source proof checkpoint used here contains the full BPI9 cohort because the public regression was exposed by its real lifecycle gate. Root's independent exact-view reuse and refused-add cold admission repairs must still be joined for combined product acceptance.


The reopened-image reader test completed PASS at the same `77b` checkpoint: one test, 972 filtered. Receipt `1791276009650474000-67696` records 08:40:09–08:49:36 UTC, status 0 and no source change, SHA256 `2b3259fa359608bd38cf1336111d1b6da46c6054da1e457ec07b0536fa90ccd9`. It serializes and reopens an actual image containing known-empty and unavailable owners, then requires Available(empty) only for the former. The following `821d` correction normalizes repeated member observations to bounded unique IDs. This follows directly from Clang's retained contract that duplicate native declarations may share one representative. The earlier raw-count guard could refuse a canonical inventory which fits; the final shared proof covers that case.


`821d5704f40da159c3659290653352b41c23144d` closes the repeated-observation bound: all IDs are validated first, and canonical set allocation is bounded by unique valid emitted IDs rather than raw repetition count. The shared test first observes the same member five times in a four-fact plan, then checks idempotence, exact conflict operands, invalid coordinates and non-mutation. The final frozen source is tree `d66ec1f0bdb82b12a0879d87a4616b309f8a7fd4`.

All five focused producer/common/native tests passed again at this final source (762 filtered, 0.37 seconds runtime). Receipt `1791276660299757000-10889` records 08:51:00–08:55:17 UTC, status 0, no source change and SHA256 `7e8c632b55f6e157c094e40df0eba235b8fbec3dee20cdaaf9b358be7e0d4bdb`. The log is `/tmp/sol-declared-members-engine-r3-20261006.log`. Append `821d` after `77b` in the ordered member slice; the reader code and its known-empty/unavailable contract did not change after the reader gate.

The actual public package gate completed at immutable `821d`, log `/tmp/sol-ingest-public-cold-lifecycle-members-821d-20261006.log`. The original `pulse` field assertion now passes, including its builtin type, following actual Cargo compilation, authenticated publication, terminal Published source capture and public name/callable/carrier reads. This is warm product evidence for the repaired common Rust path, rather than a helper-only substitute.

The overall test exited 101 after 23.41 seconds runtime and a 7 minute 16 second build. It stopped at `index_operation_lifecycle.rs:416`: the initial owner report has one failure where the fixture requires zero. Receipt `1791276968507698000-28246.json` records 08:56:08–09:03:50 UTC, status 101 and no source change; SHA256 `6d269092694d79d1a7c648be946310b931d67db57b1f40781d7ffbf820b8edb4`. The executed binary SHA256 is `94144fff5abc3833d370947ea755f5febffe9b4b00329ec300d569dd440ff873`. Fixture `/tmp/b-GPN4a8` is retained.

Read-only cause tracing identifies an inconsistency in the existing expected-refusal path: the fixture deliberately changes the selected generation at lines 300–303. `semantic_shapes.rs:253` converts that exact-source mismatch into `Err(BuiltinModelError)`, `service.rs:335` maps it to `ProtocolError::CommandExecution`, and `listener.rs:677` counts it as an owner failure. The preceding stale-freshness probe instead returns the typed `CommandReply::Failed(InvalidQuery)` at `semantic_shapes.rs:273`, so it does not increment that counter. The test therefore rejects the counted deliberate negative probe at shutdown. This observation does not establish a transport retry regression or justify ignoring an unexpected owner failure. Root owns the query refusal classification and final integration; neither the assertion nor query implementation was altered in this lane to bypass the failure.

Cold shapes, distinct-key same-content add, actual missing-dependency compiler refusal, retained prior generation and exact cold failed-operation replay remain unreached in this run. Only completed reached phases constitute evidence.


## Equal-generation re-add exposed a second capture regression

Root's selector-refusal correction `ddee8d6aaef90621f31b78758b1ab332a9554a79` was read and applied as `1f452fbd86f85dd4fc17a3b5c7b40cc5a8ecd5de` (tree `6af555fb3d81131fdcca2410601c4475b8736fb8`). It preserves the source-binding comparisons, chooses the immutable selected record first and classifies caller mismatch as typed InvalidQuery. The original fixture and zero-owner-failure assertion were unchanged.

The matched public rerun passed that assertion, initial cold restart, exact operation status/replay/key-conflict and cold shape equality. It then failed at line 553 after unchanged-package add under key `6e`: the operation was Published, but its source-capture profile was Failed(ProjectAuthority), retaining the original complete generation. Runtime was 65.30 seconds after a 2 minute 45 second build. Receipt `1791278105496790000-87161` records 09:15:05–09:18:58 UTC, status 101 and no source change, SHA256 `6c9d99e8bff3d0fb9c7fc40f08595de8bf3e42d9788ec3fba41d764d2034599d`; binary SHA256 `e4c1a90f5f56f81032af4396daeac8557dca93636c4d03c169bd276a8b46857c`. Log `/tmp/sol-ingest-public-cold-lifecycle-refusal-1f452-20261006.log`; fixture `/tmp/b-xL4YQE` is retained.

Both independent Root storage/view fixes were reviewed, then applied only after that run finished: `9fde90931581e5a44c1d28485ef9ba11c1633b2b` as `44cf64afe5aec757ac001a3b732d0dcb5d4dac27`, and `850a22533398e4c0f826fa8dd611695d85e49a60` as `ad10805c696e13ebf63827d177fcdcc53d57aabf`, tree `2e165ac865a9ace25d0a28428fbfabab420b3a63`. Empty view patches still require a changed admitted capability. The journal's generation now binds both workspace root and capability fingerprint, including capture-only commits with an unchanged manifest root. A changed authority generation persists a full snapshot; its cost is O(view), and no constant-size persistence claim is made.

The unchanged public gate at `ad108` repeated the same line-553 failure after the earlier cold/replay/shape phases passed. Runtime was 90.60 seconds after a 12 minute 33 second build. Receipt `1791278463496799000-15779` records 09:21:03–09:35:10 UTC, status 101 and no source change, SHA256 `2e4d0662c2359216b62ed71813f0fcbfc16215f0c5aa8af3b9d2d7ab8374c987`; binary SHA256 `a5883c70f25d8ea3ea98e37ed1b25320e07efe7f74790288f481d4a11ec0d345`. Log `/tmp/sol-ingest-public-cold-lifecycle-cohesive-ad108-20261006.log`; fixture `/tmp/b-AC8M6L` is retained. Read-only SQLite-compatible journal inspection confirmed Published operation `6e` with Failed(ProjectAuthority) capture. Neither fixture's authority credential was read during diagnosis.

The precise cause is independent of view rebinding. `completed_capture_changes` searched only semantic relation deltas. `record_semantic_publication` deliberately suppresses historical and selected deltas when the newly admitted record equals the retained record. A successfully admitted equal generation therefore produced no delta and was incorrectly labelled Failed(ProjectAuthority) for Rust's zero syntax-source count. Suppression of identical selected records dates to `d042ae57807` (25 September); immutable-history suppression dates to `4faee5661c4` (28 September); these are intentional optimizations. The incompatible capture interpretation was introduced by `f0a563dc7e51265be67084e0f45ae79f71761abe` at 00:56 UTC on 6 October. Both synchronous completion (old line 792) and deferred completion (old line 2195) used the helper. Canonical `6ac9` has no such helper; integrating `983` has the faulty delta-only interpretation.

Root authorized a typed completion repair. `f0cf63b7f0d4a0eea84bbc2770eca44ba9d44725` carries a private AdmittedCapturePublication, containing exact selected key, coverage and claim, independently of delta emission. The constructor is the recorder called after successful local/remote admission and immutable-history checking. A missing delta requires exact equality with the retained selected record; absent admission, wrong key, changed claim, changed coverage, duplicate completion or contradictory refusal cannot prove publication.

Its first focused gate genuinely failed at the final actual owner commit after the admission controls: `profile.rs:2573` requires a Published capture to name its exact semantic publication after-image in the same intent. That storage guard was preserved. Receipt `1791279520618070000-35433` records 09:38:40–09:43:52 UTC, status 101 and no source change, SHA256 `2ac0eec88cb74d73499eeefc32c886b7f7ff5f609e624f30693d1a6d955fa4d3`; runtime 0.89 seconds after 5 minutes 8 seconds build. Log `/tmp/sol-ingest-admitted-capture-controls-f0cf63-20261006.log`.

`f2f8a9ddc0454aad77ffa4f69ee1bf848e956cf9` adds named CompletedCaptureChanges: capture terminal rows plus exact retained publication observations. Both callers include that identical after-image in the terminal intent; it does not invent an ordinary semantic relation change. Duplicate selected after-image keys are now rejected before first-match lookup. Controls cover wrong Published delta versus proof, identical/contradictory duplicate keys, non-mutation on refusal and unchanged semantic root after the real owner terminal commit. The exact focused source is tree `d27f373fb2c1f01ee5f72457625b4a0b3eb0ad65`. The focused control completed PASS: one test, 974 filtered, 1.06 seconds runtime after a 4 minute 2 second build. Receipt `1791280180649668000-83510` records 09:49:40–09:53:46 UTC, status 0 and no source change; SHA256 `f97d8701d21f2304b08c82204f356d8ee470665239a0cc9c01ef120d8e4934be`. Executed lib-test binary SHA256 is `7bd9682e6e40511093ed6ee0bfc1e956623e4942bf5d38c9852ef8cd0c1b8c00`; log `/tmp/sol-ingest-admitted-capture-controls-f2f8-20261006.log`. These are real relation/owner persistence controls with deterministic structural claims, not actual compiler acceptance. The unchanged public lifecycle is running at the same frozen source; product evidence remains pending.


The post-member common-path dependencies are the Root atomic selector refusal correction, capability rebinding, and capability-bound view journal generation:

```sh
git cherry-pick ddee8d6aaef90621f31b78758b1ab332a9554a79 \
  9fde90931581e5a44c1d28485ef9ba11c1633b2b \
  850a22533398e4c0f826fa8dd611695d85e49a60
```

They are patch-equivalent to this verification lane's `1f452`, `44cf64` and `ad108`, respectively. Root must skip any equivalent already integrated. The capture completion repair is the ordered two-commit slice below, including its focused controls. Both changes are confined to `crates/local-service/src/builtin/commands/index.rs` and preserve the existing public lifecycle fixture.

```sh
git cherry-pick f0cf63b7f0d4a0eea84bbc2770eca44ba9d44725 \
  f2f8a9ddc0454aad77ffa4f69ee1bf848e956cf9
```

The pair repairs admission-versus-delta interpretation and binds the exact unchanged publication into the capture's terminal intent. The first commit alone is insufficient: its preserved storage check correctly rejects the terminal publication missing from the intent. The BPI9 foundation above is a prerequisite. Root's separately verified durable typed compiler-failure journal/status slice must also be integrated for that public field; this private lifecycle source does not include it. Root requested a later modest representation cleanup to derive selected claims from admitted completions instead of parallel vectors. That cleanup is deferred while the frozen real public gate runs and is not necessary evidence for the present repair.


The first unchanged public `f2f8` run failed earlier with bare `Disconnected(InvalidInput)` after 19.11 seconds runtime, following a 2 minute 4 second build. It reached initial operation `6d` Published and terminal source capture Published. Read-only durable journal inspection found no same-add `6e` or refused-refresh `6f`; those phases were not reached. The retained fixture is `/tmp/b-cKOGgG`, and its valid original Cargo manifest remains unchanged. Receipt `1791280546098988000-1154` records 09:55:46–09:58:12 UTC, status 101 and no source change, SHA256 `5474099317d33bc0ef9a016381dfc31f375ba7f37feb5e267a4db8535bc3fa47`; binary SHA256 `4a4370009a895af1514e60b0954dd2e12094d8e085b7c92e37c02290ad3473bf`. Log `/tmp/sol-ingest-public-cold-lifecycle-capture-f2f8-20261006.log`.

The error's timing and bare form are consistent with a read hitting the listener's deliberate frame-retirement boundary after keyed status polling finishes. `listener/transport.rs:46` closes a stream after 256 frames; `client/src/lib.rs:458` performs one request, and its `is_disconnect` documentation at line 1772 identifies macOS InvalidInput on a closed Unix peer. The fixture's status poll helper at line 1125 explicitly reconnects and repeats the exact keyed read, while later bare name/version reads do not. This is an inference supported by reachable paths, not a captured exact failing request. The failure is retained as evidence; it neither disproves admitted capture equality nor proves later lifecycle phases. One exact unchanged-source rerun repeated the bare disconnect after 18.67 seconds, without rebuilding or changing retry policy, assertions, fixture or product behavior. Receipt `1791280817507289000-7957` records 10:00:17–10:00:37 UTC, status 101 and no source change; SHA256 `0a2db622259372e654f09203820aae581624d09644d3cc35d7d9b03845e2057a`. Log `/tmp/sol-ingest-public-cold-lifecycle-capture-f2f8-r2-20261006.log`; retained `/tmp/b-gmKdC0` again contains only initial `6d` Published with capture Published. The failure requires exact read-phase diagnosis before later same-add/refusal/cold gates can be accepted.


Root approved the exact test-only reconnect at the forced-publication polling boundary. Commit `c2626af12f7370893a57bad380adb268e83c9648`, tree `eb2ffccdff7c4a3c6307532dd2101d1ddc5a2301`, changes only the Published return of `wait_for_published`: explicitly authenticate a replacement Session stream before ordinary reads, then return the same status. All forced 260-keyed-read retirement checks, mutations, receipts, owner failure oracle and cold assertions remain unchanged.

The matched full gate now passes M2's actual equal-generation re-add: operation `6e` and its capture are both Published, select the identical prior generation, and preserve public shapes. It also reaches the real missing-dependency compiler refusal: operation `6f` is Failed and its capture is Failed retaining exactly that generation. It then fails the original freshness assertion at line 604: the selected generation must be Historical with differing input digests. Initial, same-add and refused captures all contain identical source version and input digest despite the changed Cargo.toml. This is a newly exposed config-input/freshness boundary, not a repaired capture-completion regression. Cold failed-operation replay remains unreached.

Receipt `1791281099435153000-14601` records 10:04:59–10:05:46 UTC, status 101 and no source change, SHA256 `5a6b1c55503e946edaf523e934d0fb7adb0fbf1a75ecca09dce7154a60e400e2`; runtime 41.26 seconds after 3.80 seconds build. Binary SHA256 `be903bc521d0f502dbd171d2ef22e2bc374b701c633052d7ba2c4778620526c2`. Log `/tmp/sol-ingest-public-cold-lifecycle-capture-reconnect-20261006.log`; retained `/tmp/b-gF9cyj`. Compiler job terminal contains an actual typed Authority/Resolve/Binding refusal for src/child.rs, while this private operation journal still predates the separate typed-failure persistence slice. The original freshness assertion is preserved.


### Config-only freshness follow-up

The bounded configuration reader and `semantic_input_digest` already support discovered manifest/lock bytes. The common production `run_index_scan` always chooses the unproven-authority cancellable scan; `ingest.rs:1398` passed `capture_configuration_contents=false`, and the reader therefore substituted a default empty configuration snapshot at line 1688. Exact introduction is `4faee5661c4` (28 September), which adds both the semantic observation digest and this optimization; the asynchronous production scan retains it in `7c8d1c57f0` (30 September; equivalent sibling `1f02a5d17d`). Disallowing reuse without a complete authority read set is intentional. Removing configuration bytes from the factual latest-input observation is an incompatible consequence.

Commit `8b144c8da83e3fe078a1d2ca9c22776c72ea3281`, tree `174b1682531e6714eb3886d79fda88ac19196103`, changes the two unproven scan entrypoints to retain bounded configuration observations and corrects their documentation. It leaves all `ReadSetCompleteness::Unproven`, exact-input-witness, dynamic/negative read, cancellation, path confinement and memory bounds intact. The source test uses the same cancellable scan as production with actual TS/Python/Go/Rust source, changes only each language's manifest, requires the source frontier to stay unchanged while the profile digest changes, and requires Unproven profiles to compile even if an exact-current set is supplied. An oversized package.json remains incomplete and is excluded from complete configuration evidence. The former empty-config test expectation is corrected rather than preserved as authority.

The exact frozen `compiler_input_witness_tests` module completed PASS: six tests, 970 filtered, 0.33 seconds runtime after a 3 minute 17 second build. Existing unproven dynamic/newly-present input, alias, source snapshot and pre-Offer controls also pass. Receipt `1791281437315748000-29401` records 10:10:37–10:13:57 UTC, status 0 and no source change; SHA256 `d581c76eebf4e0800c06a552f075f91e4e6fd38ed73ec3fc454fb47fd2438b8c`. Executed lib-test binary SHA256 `841635efeb1ac61bcba9ebf93b1fc5e95b98990ac795e7f96e7c391d018d01b9`; log `/tmp/sol-unproven-config-freshness-20261006.log`.

The matched full `8b144` public lifecycle still FAILED the original Historical assertion, after 49.53 seconds runtime and a 3 minute 25 second build. Earlier warm publication, initial cold replay/shapes, unchanged re-add publication/capture and actual compiler refusal retaining the exact prior generation passed. The durable configuration repair worked: initial/same-add capture input was `d626c6…`, while the refused manifest-only edit recorded `dd9b21…`. The public freshness projection nevertheless remained Current. This proves a second defect after the factual input digest, rather than invalidating the configuration repair. Failed cold reopen was not reached. Receipt `1791281704256238000-40725` records 10:15:04–10:19:21 UTC, status 101 and no source change; SHA256 `87a100fe3e70b8391d2b480b00800e8e6562634aec62f648b92fe4ea76f4b5ec`. Executed integration binary SHA256 `82f3ff64a757d1e352f9554c8264203131e17fcf5cdc97b57d24ace0dc26803a`; log `/tmp/sol-ingest-public-cold-lifecycle-config-8b144-20261006.log`, preserved fixture `/tmp/b-vDJCYY`.


Append the explicit fixture lifetime correction and configuration observation repair after the two capture completion commits:

```sh
git cherry-pick c2626af12f7370893a57bad380adb268e83c9648 \
  8b144c8da83e3fe078a1d2ca9c22776c72ea3281
```

These four recent commits do not replace the earlier BPI9, member inventory or Root source/view dependencies. They also do not subsume the separate durable typed compiler-failure status fix. Source and dependency proof remains associated with the exact preceding checkpoints in this ledger.

### Latest capture projection and refused publication seam

The selected-generation serving fence in `cc1251519f8a3bd4996f0b861c58cb851e1103a5` (30 September; sibling `ba9f290d901545ea1386f3d910e7cfa2151851bc`) installs `committed_observations` only after a semantic selection commits and restores the selected generation's observation during cold reconciliation. That is appropriate selection authority, but it is insufficient factual latest-input authority once a source capture can commit without a compiler selection. The separate capture-plane cutover `44fdc36abc155adbc8178e1cc0fef180a81883a7` (5 October 22:15 UTC; sibling `3402cc7de1686d83d1aa9f8d81a6d6e9cb48d768`) collects authenticated `capture_rows` during cold reconciliation without using them for freshness. Its reachable warm path also retains the successful generation observation after a refused capture. The alternative earlier combined-marker implementation `4167d3d0a47cbf2c31efa3d41c5e97486092f7f6` had an explicit committed-source observation update; it is historical salvage evidence, not a compatible whole-file replacement.

Commit `a209ebffe25c8c02dd85816ccc0dd8fdcc2b9a57` supplies the same immutable `WorkspaceSnapshot` to every freshness query. The latest input is constructible only from that snapshot's admitted capture relation. The selected input comes from the exact retained compiler generation, with full claim equality against the same snapshot's immutable generation row. Missing proof returns Unverified; contradictory claims remain hard errors. Pending and refused inputs can make a retained generation Historical without selecting or manufacturing a generation. `semantic_versions`, both initial/final `semantic_shapes` checks, and the explicit historical-selection response use this shared API; none consults advisory Turso latest state or overlays only a response.

Test commit `adf55dd2e1494e2351c56761eac0b4e6b16b11f1` adds a second real Cargo/public-owner lifecycle: a missing dependency refuses the first compile, all captured profiles remain Unavailable, public versions stay empty, and a caller-invented selected source receives typed NotFound. It cold reopens the exact terminal operation and repeats authority absence and keyed failed replay with unchanged workspace root and zero owner failures. This adversarial invented source is only a negative request, never compiler-publication evidence. The original prior-generation lifecycle and its Historical assertion remain unchanged. `c4f743c2031ba681e70d19eb1e0b55c547589f42` adapts the fixture's rejected coordinate error to its existing boxed IO error boundary; the production coordinate admission remains unchanged.

Mac SOL's narrow terminal repair `e4a4808bfff4e00eeed510cef10a265836a7a716`, privately joined as `13224d7af6ff22153870b649e8fd53123c344485`, invokes the existing authenticated `publish_view` after a failed capture successfully commits. It preserves the primary typed compiler refusal if the capture commit or view reconciliation fails. Its original own test build was guard-stopped, and a subsequent source compile exposed an unqualified test-only CommandReply import; neither is a test pass. Mac SOL owns the successor initial Pending-capture publication and qualification repair. The public frozen gate below completed; the successor will be joined after Mac SOL freezes its reviewed diff.

The exact two-test real public gate PASSED at clean `c4f743c2031ba681e70d19eb1e0b55c547589f42`, tree `3a5825c6ac92e1047b2ba99a02bd028d47bc0c65`, with the existing warm ingest graph, two Cargo workers, locked/offline dependencies and sequential tests. Runtime was 47.69 seconds after a 2 minute 23 second build. The first real Cargo refusal gate passed authority absence, typed NotFound, exact cold status/replay and zero owner failures. The original lifecycle passed initial publication and exact members, forced retirement, cold shapes/status/conflict, distinct-key equal-generation re-add with Published capture, actual config-only refusal with Historical freshness and retained shapes, then cold failed-operation status/shapes and exact-key replay without root mutation. The original assertions were not weakened. This private cohort predates the separate `08e3` typed compiler-failure journal field, so that specific durable public field is not claimed.

Receipt `1791283036525539000-32255` records 10:37:16–10:40:30 UTC, exit 0 and no source changes; SHA256 `4a0ae85fce32af2ea2fc4c01841d62ddf9f2cc3bb235094229b9218c3a0d3d54`. Executed integration binary SHA256 `52795b5c80b3ab62782aeb4c0c5afc87db5c5db370e80fade7dfaad4df9ab4dd`. Root explicitly permits this mature graph up to 16 GiB allocated, requires at least 18 GiB free to launch and stops own growth below 16 GiB free. It launched with 22.13 GiB free and 13.839 GiB allocated and finished without a guard stop. The external guard records `/tmp/sol-ingest-cohesive-freshness-c4f743-20261006-{disk.jsonl,guard.json}`; guard receipt SHA256 `10e0d1f7bc7ed0d22878acbb2ed616294632d373b394324ec9f0ed195bb3c55b`. Test log `/tmp/sol-ingest-cohesive-freshness-c4f743-20261006.log` retains the actual typed job refusal and both test results.

After the previous four recent commits, integrate these atomic diffs in order, resolving current vocabulary without whole-file replacement:

The explicit fixture prerequisite is `07b9520eb8ac1b4a8baeeb2cf960ed367b141231`. It changes only `crates/local-service/tests/index_operation_lifecycle.rs` and introduces the complete initial cold-shape, distinct-key unchanged add, real config-only failed refresh, Historical retained-shape, failed cold-status and replay assertions and helpers. It was included in the earlier member/test handoff but must not be assumed integrated merely because the member production slice was integrated. Root's 3a checkpoint lacked this fixture extension, so applying `adf` directly conflicted and could not introduce the dependent cold block coherently. Direct `3a3f283316` to `c4f743` file comparison confirms the missing fixture chain is exactly `07b` → `c262` → `adf` → `c4`; no 3a assertion or typed-status expectation is removed. Root chose to review and transfer the complete final `c4` test file. If applying atomic commits instead, include `07b` before `c262`; prior source-capture/name/shape fixture prerequisites are in the earlier 33-commit cohort.

```sh
git cherry-pick a209ebffe25c8c02dd85816ccc0dd8fdcc2b9a57 \
  adf55dd2e1494e2351c56761eac0b4e6b16b11f1 \
  e4a4808bfff4e00eeed510cef10a265836a7a716 \
  c4f743c2031ba681e70d19eb1e0b55c547589f42
```

Skip `13224` when the original `e4a4808` has been integrated. After the full public PASS, the requested modest cleanup was committed as `e12a80432be1ffc5b39a9747fe1681ca106f940e`: sync and deferred selectors derive from the single admitted key/claim/coverage collection; a named compilation result replaces the old quadruple return. An explicit selected `after: None` is rejected as a contradiction, with controls both with and without completion proof. Its initial focused gate PASSED at `a6fd3fbcf227922b61d786f4b72e99d95047c01c`, tree `177c5066b50993839b7fd38a819ec21ca48bee30`: one test, 976 filtered, 1.05 seconds runtime after a 3 minute 29 second build. Receipt `1791283589211950000-54881`, 10:46:29–10:50:01 UTC, exit 0, source unchanged, SHA256 `476fd796b5d08972fe817f245c748650e2c13294412849269ccae904268bd920`. Binary SHA256 `8acb7d45dbe7735da97c49f11d7b5b272e9106f25e102e1de995bc4cba9bc106`. The extra `a6fd3` change only qualifies the old Mac fixture's reply type; the successor `a090` contains that equivalent qualification, so Root can skip it.

### Final combined proof and saved evidence

Mac's initial Pending publication successor `a090166cebe34697ea8b2aa872b1f2cb96b41897` was joined privately as `436abdaad` after preserving the equivalent qualified assertion and adding its previously absent evidence document. Its documentation hash clarification `d3b9e19cceea7b3b9cd2d5880f64d8cd14bd9410` is private `d67f12d89cde4fc191e64e1b34d95cd8cc700a6d`. The final clean source tree is `3028e6cdca01d87f8e382740045a2e6832dcd38a`. Production and test source whitespace checks pass; exact upstream raw log bytes retain their EOF blank-line warnings and SHA256 identity.

At that exact source, the actual Pending/terminal resident-view unit PASSED 1/1, 976 filtered, 1.62 seconds runtime after a 3 minute 43 second build. Receipt `1791283945105445000-69174`, 10:52:25–10:56:12 UTC, exit 0, source unchanged; SHA256 `4423b6e01b1b4adabbb9fd16906236df69e3f223afd9f2b8674ee7c1c83a7c28`. The admitted-publication controls then PASSED again on the same source, receipt `1791284267405248000-90471`, 10:57:47–10:57:49 UTC, SHA256 `208d703ceb1324d8e8e674042acf78b52096a134915991ed6f4545728d21cf57`. Both executed the same verified lib-test binary SHA256 `8238fa9c0c29b87337bed936910b7f24f3a3795d2a276d97f3c50000017f0e38`.

The complete unchanged two-test real public lifecycle PASSED 2/2 on that same final cohort: 64.10 seconds runtime after a 2 minute 56 second build. Receipt `1791284272807525000-90775`, 10:57:52–11:01:55 UTC, exit 0 and no source changes, SHA256 `b3c29ce27036d19dbf3a0c28b79083729d46dcca81473c2b89e71bb200b6d55d`; binary SHA256 `88f9b721a53b3bb9d00ddd120332bfe35064772cafabf96af57be50be39c4e0a`. This repeats all prior-generation, equal-add, config-only refusal/Historical retained-shape and cold failed-replay assertions after the representation cleanup and Pending publication change. The first-refusal test separately asserts zero owner failures for both of its owners; the original test asserts zero on its initial owner. Its later restarted/final-cold close counts are not inspected, so no all-owner counter claim is made.

The exact final guard is `/tmp/sol-ingest-public-final-d67f12-20261006-guard.json`, SHA256 `c14383a721c16b42346d2a72bb10a7a97bbe7838300526d0ae51a322efbc93ec`; runtime log `/tmp/sol-ingest-public-final-d67f12-20261006.log`. No guard stopped the run. Last sample had 17,525,858,304 bytes free and 14,876,995,584 allocated in the owned mature graph plus target. No new local Cargo invocation was launched below Root's 18 GiB floor.

The referenced logs, wrapper receipts, exact resource guard receipts and samples are copied without alteration into `evidence/sol-history-capture-cold-20261006/`. `sha256-index.json` records their original paths, saved paths, byte lengths and independent hashes, and explicitly lists any referenced historical receipt unavailable for copying. It includes earlier failures rather than replacing them with green results. These are source and embedded-owner proofs; they do not claim an installed matched successor image, all-language compilation, positive Tantivy pagination, or the separate durable typed compiler-failure status field. Root's integrated `039c360d286962a6cf19488fb86caabd1d8c93de` includes the missing `07b` fixture prerequisite and the reviewed repair slices; its own full service suite and successor images are separate gates.

### Dormant build retirement after final proof

Root authorized retirement of this agent's dormant mutable Cargo graph after the final proof. Before removal, both exact final d67 binaries were copied by streaming into independent, private files under `luna_ingest_pages_recovery/.local/frozen-verification/capture-cold-d67f12-20261006/`; each has one link, mode 0555 and the macOS immutable flag. Their hashes are the final lib-test `8238fa9c0c29b87337bed936910b7f24f3a3795d2a276d97f3c50000017f0e38` and public lifecycle `88f9b721a53b3bb9d00ddd120332bfe35064772cafabf96af57be50be39c4e0a`. Both independent copies were rehashed before removal. Dynamic linkage inspection names only system libraries/frameworks; a 28-path available Nix-root closure, including the pinned Rust toolchain, is saved with SHA256 `3f51248275ccbd8a5c0cb2560d028477371b52cda811ab862989fc4497afc0ee`. This records the source-test executables and their available toolchain roots, not an installed product image or exhaustive optional language toolchain closure.

The exact owner stamp, clean d67 revision and tree were rechecked. Repeated `lsof -nP +D` on the owned slot returned exit 1 with empty stdout/stderr: no open references. At 11:16 UTC on October 6, only its `debug/deps`, `debug/build`, `debug/incremental` and `debug/examples` were deleted. Their unique-inode allocation was 14,815,010,816 bytes (13.797 GiB), with zero external regular-file hardlinks and zero symlinks. The owner stamp, `.rustc_info.json`, `debug/.fingerprint`, `.cargo-build-lock`, `tmp`, all guard and execution receipts, runtime fixtures, source and immutable binary archive remain. No other graph was touched. Shared-host free space changed from 14,159,753,216 to 28,852,699,136 bytes; the 14,692,945,920-byte observed change includes concurrent host writes, so the measured owned allocation is the precise retirement figure.

Exact archive, closure, linkage and cleanup metadata are committed under `evidence/sol-history-capture-cold-20261006/frozen-binary-metadata/`, with an independent metadata hash index. The roughly 632 MB of preserved binaries remain private and are not Git objects. This agent launches no replacement build while Root's matching integrated source gates run.
