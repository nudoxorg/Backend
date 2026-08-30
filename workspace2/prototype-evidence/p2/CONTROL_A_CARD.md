# P2 durable-publication control A: canonical build card

## Custody, terminal, and scope

| Row | Frozen decision |
|---|---|
| Baseline | The source-candidate baseline is `be015582d6705d5cf0a059fda4a9bacc1732efd0`, clean in manager worktree `/private/tmp/nudox-prototype-durable-publication-heart-build`, branch `codex/prototype-durable-publication-heart-build`. The current card is a later manager commit and its commit/digest are recorded separately in each fresh-calibration evidence record; it never replaces the source-candidate baseline. This successor card incorporates, without amendment chains, every final decision in `CONTROL_A_CLOSURE.md` and `CONTROL_A_PREEDIT_REVIEW.md`. `orchestra-shared` is read-only and is never edited, merged, rebased, or cherry-picked. |
| Capability | One exclusive `std::fs::File` journal reduces a `WorkflowEvent`, makes one 92-byte frame, writes it, calls file sync, replaces its compact state, and returns one prototype-only stable receipt paired with that reduction. Reopen streams records without retaining them, repairs only an incomplete final frame, and otherwise rejects the first invalid physical/semantic frame. |
| Observable terminal | Starting from a new journal, append `Requested`, `Admitted`, `Staged(output)`, `Verified(output)`, and `PublicationStarted(output)` for one fixed key/output. The fifth `append` returns only after file sync; its reduction has `EffectAction::Publish { output }`. Drop, then reopen, yields `Recovery { state: Publishing(output), pending_effect: Some(Publish { output }) }`. |
| Out of scope | Bounded MPSC, worker thread, group buffer/commit, CAS head, rename, shared `Published`, `DurableAppend`, async/futures, tracing, serde, database/ORM, network, unsafe/SIMD, lock-free claim, shared/`Arc<File>`, and any active workflow/identity edit. B is a later bounded `sync_channel` control only after A is reproduced; C is a later compact receipt-gated head only after B. |
| Final decision | Retain or reject this A control as prototype evidence only. It does not close P2 or authorize a shared merge. |

## Exact paths and baseline facts

The Luna builder may edit only these paths, staging only these paths in each checkpoint:

```text
workspace2/adapters/durable-publication-heart/Cargo.toml
workspace2/adapters/durable-publication-heart/Cargo.lock
workspace2/adapters/durable-publication-heart/src/lib.rs
workspace2/adapters/durable-publication-heart/src/journal.rs
workspace2/adapters/durable-publication-heart/tests/control.rs
workspace2/adapters/durable-publication-heart/tests/receipt_surface.rs
workspace2/adapters/durable-publication-heart/tests/ui/forge_receipt.rs
workspace2/adapters/durable-publication-heart/tests/ui/forge_receipt.stderr
workspace2/adapters/durable-publication-heart/tests/ui/receipt_from_raw.rs
workspace2/adapters/durable-publication-heart/tests/ui/receipt_from_raw.stderr
workspace2/adapters/durable-publication-heart/tests/ui/forge_append_success.rs
workspace2/adapters/durable-publication-heart/tests/ui/forge_append_success.stderr
```

Only the manager may create or edit these evidence paths:

```text
workspace2/prototype-evidence/p2/CONTROL_A_CARD.md
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_CALIBRATION.md
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_PREEDIT_REVIEW.md
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_BUILD_HANDOFF.md
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_PASS_1.txt
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_PASS_2.txt
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_METRICS.csv
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_TRIPWIRE.txt
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_SURFACE.txt
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_REVIEW.md
workspace2/prototype-evidence/p2/CONTROL_A_FRESH_CLOSURE.md
```

Read-only direct facts are `crates/nudox-workflow/src/{durable.rs,reduce.rs,recovery.rs,lib.rs}` and `crates/nudox-workflow/tests/durable_shared.rs`. Their digest ledger is the already committed `CONTROL_CARD.md` baseline table: `durable.rs` `82bcae…e325a`, `reduce.rs` `fbc061…d25b`, `recovery.rs` `895a5d…a6fa4`, `lib.rs` `020d71…b08e`, and the shared test `a49e42…1147c`. The focused baseline gate is `cargo test -p nudox-workflow`, which passed 9 unit tests and one integration test at this SHA.

## Format, ordering, and arithmetic

All integer cells are little-endian. There is exactly one 32-byte header:

```text
0..8   magic b"NDXP2J\0\0"
8..10  physical_version u16 = 1
10..12 header_bytes u16 = 32
12..14 workflow_bytes u16 = 68
14..16 frame_bytes u16 = 92
16..32 first 16 BLAKE3 bytes of b"nudox.p2.header.v1\0" || header[0..16]
```

Every frame is exactly 92 bytes:

```text
0..8   sequence u64
8..76  one canonical 68-byte WorkflowRecord
76..92 first 16 BLAKE3 bytes of b"nudox.p2.frame.v1\0" || physical_version LE || frame[0..76]
```

`FIRST_SEQUENCE` is zero. The checked durable end is `HEADER_BYTES + (sequence + 1) * FRAME_BYTES`. `MAX_DURABLE_SEQUENCE` is exactly `200508087757712516`; its durable end is `18446744073709551596`. The first invalid sequence is exactly `200508087757712517`, and its formula would produce `18446744073709551688`. The one private, testable control-module arithmetic boundary must return `AppendError::SequenceExhausted { sequence, maximum: 200508087757712516 }` for both that first invalid sequence and `u64::MAX` before a frame, write, state replacement, receipt, or effect is formed. It is not a public normal-build API. A focused `#[cfg(test)]` unit in `journal.rs` is expressly authorized to call this private pure boundary; every externally observable law remains in the top-level integration tests.

Append has one successful order only:

```text
reduce(current_state, event) -> WorkflowRecord/frame -> write logical frame -> File::sync_all()
  -> replace state -> AppendSuccess { receipt, reduction }
```

Reduction failure changes no bytes/state. A write or file-sync error produces source-bearing `OutcomeUnknown`, retains the old in-memory state, emits no success/effect, and poisons the handle; only drop/reopen may reconcile. One logical frame write may make multiple physical partial writes, but must not retry after an injected terminal write error. For the deterministic `0..=91` transferred-then-error schedule, reopen truncates and syncs exactly to the prior completed prefix. For `92` transferred-then-error, caller still gets `OutcomeUnknown`/no receipt/effect/advanced memory and the handle poisons, but checksum-valid reopen accepts the complete frame and recovers its effect.

## Stable path and reopen contract

This control claims stable-path best effort only. Before journal header I/O, stable fixtures whose observed metadata is symlink, directory, or another nonregular kind return `OpenError::Path { observed: Symlink | Directory | Other }`. It makes no no-follow/race-free claim: symlink/path replacement TOCTOU and hostile cross-process writers are explicit UNVERIFIED surfaces.

New-file creation writes the header, file-syncs it, opens the parent directory read-only, directory-syncs it, then returns the journal. Existing file length `0..=31` is exactly `OpenError::HeaderTruncated { observed }` before magic/geometry/checksum interpretation, with no repair; observed is the physical byte count. At length 32 or more, header priority is magic, physical version, header bytes, workflow bytes, frame bytes, header checksum; then, for each complete frame: frame checksum, exact expected sequence, canonical record decode, reducer transition. A header mutation returns the named header variant with its observed raw cell; a header checksum mutation returns `HeaderChecksum { observed }`. A full invalid frame never triggers repair. A suffix of `1..=91` after a valid prefix is a torn tail: truncate exactly to prefix end, file-sync, then return a recovered healthy owner. Read/truncate/sync failures are `OpenError::Io { step, source }`; no source is erased.

After checksum and sequence validation, canonical decode failure is exactly `OpenError::FrameDecode { sequence, source: WorkflowRecordError }`; reducer rejection is exactly `OpenError::FrameReduction { sequence, source: ReductionError }`. Both leave full length unchanged, make no repair, and return no journal. `FrameSequence { expected, observed }` also leaves bytes unchanged. Scanning owns exactly one transient fixed `[u8; 92]` frame buffer. Production has no dynamic container, slice owner, reference-counted owner, collection builder, preallocation, or equivalent collection path over `WorkflowEvent` or `WorkflowRecord`: this rejects `Vec<...>`, `VecDeque<...>`, `LinkedList<...>`, `BTreeMap`/`BTreeSet`, `HashMap`/`HashSet`, `BinaryHeap`, `Box<[...]>`, `Arc`/`Rc`-backed storage, `Cow`, `SmallVec`, `ArrayVec`, every alias/wrapper whose field or item contains either workflow type, and every `collect`, `from_iter`, `with_capacity`, `try_reserve`, `reserve`, `extend`, `push`, `insert`, or equivalent collection route. The production audit scans every dynamic-owner/collection declaration and builder in `src/lib.rs` and `src/journal.rs`; the manager manually classifies every hit and rejects any direct or indirect event/record containment or construction path. No retained log, replay cache, or record/event collection is permitted.

The only parent-authorized outgoing-byte boundary is the existing
`nudox_id::FixedCanonicalRecord::canonical_bytes` implementation for `WorkflowRecord`. The
adapter may add precisely `nudox-id = { path = "../../crates/nudox-id" }` and use that trait
solely to copy the canonical 68 bytes into frame `8..76`. The production source audit requires
the `FixedCanonicalRecord` import and the `record.canonical_bytes()` fill at that frame cell;
a local encoder, `zerocopy`/`IntoBytes` substitute, or test-only trait call fails. It may not add a shared
`nudox-workflow`/identity API, reexport the trait, change any identity implementation, use
unsafe/raw representation access, or add any other normal dependency. A feature-off integration
golden must import the trait in the adapter test, construct `WorkflowRecord::from(event)`, call
`canonical_bytes`, and prove the resulting 68 bytes exactly equal the independently constructed
frame-record bytes before frame checksum construction.

## Surface, fault seam, and resource law

Normal-build public surface is exhaustively this literal skeleton (all unshown fields are private):

```text
FileJournal::open_or_create(path: impl AsRef<Path>) -> Result<FileJournal, OpenError>
FileJournal::append(&mut self, event: WorkflowEvent) -> Result<AppendSuccess, AppendError>
FileJournal::recovery(&self) -> RecoveryObservation
AppendSuccess::receipt(&self) -> &StableReceipt
AppendSuccess::reduction(&self) -> &Reduction
AppendSuccess { receipt: StableReceipt, reduction: Reduction } // both fields private
StableReceipt::{sequence, durable_end}
StableReceipt::sequence(&self) -> u64
StableReceipt::durable_end(&self) -> u64
StableReceipt { sequence: u64, durable_end: u64 } // both fields private
PathKind::{Symlink, Directory, Other}
OpenIoStep::{Metadata, Open, HeaderRead, HeaderWrite, HeaderFileSync, DirectoryOpen, DirectorySync, FrameRead, TailRepair, TailRepairSync}
AppendIoStep::{WriteFrame, FileSync}
OpenError::{Path { observed: PathKind }, Io { step: OpenIoStep, source: io::Error }, HeaderTruncated { observed: u64 }, HeaderMagic { observed: [u8; 8] }, HeaderVersion { observed: u16 }, HeaderBytes { observed: u16 }, WorkflowBytes { observed: u16 }, FrameBytes { observed: u16 }, HeaderChecksum { observed: [u8; 16] }, FrameChecksum { sequence: u64, observed: [u8; 16] }, FrameSequence { expected: u64, observed: u64 }, FrameDecode { sequence: u64, source: WorkflowRecordError }, FrameReduction { sequence: u64, source: ReductionError }}
AppendError::{Reduction { source: ReductionError }, SequenceExhausted { sequence: u64, maximum: u64 }, OutcomeUnknown { attempted: WorkflowRecord, sequence: u64, step: AppendIoStep, source: io::Error }, Poisoned}
RecoveryObservation::{Healthy(Recovery), Poisoned}
```

`RecoveryObservation::Healthy(Recovery)` carries recovery; `Poisoned` carries no guessed state. The two `StableReceipt` accessors reveal only the two declared durable facts, not a raw representation. `StableReceipt` and `AppendSuccess` fields are private. There is no public success constructor, conversion, default, bridge, receipt trait implementation, reexport, raw-byte exposure, integer/byte conversion, shared published conversion, or head operation. The mandatory downstream mixed-pair fixture attempts exactly `AppendSuccess { receipt: first.receipt(), reduction: second.reduction() }` from two successes and must fail because this opaque/private paired authority has no accessible fields or constructor. Consumers may place independently borrowed receipt/reduction facts in their own type; that type is not an `AppendSuccess` authority witness and is outside this negative claim. No other normal public item, derive, conversion, or reexport adds authority.

Only under `fault-injection`, the named public `fault_injection::FaultScript::{write_error_after, file_sync_error, directory_sync_error, tail_repair_sync_error}` and `FileJournal::open_or_create_with_fault(path, FaultScript)` are reachable. Cargo consequently permits any dependent that explicitly enables this feature to reach that test API; no narrower package-only visibility is claimed. Both are absent from the feature-off docs/surface. The script may inject a terminal write error after a cumulative `0..=92` transferred bytes, file-sync error, creation directory-sync error, or tail-repair sync error. Feature-off/on share identical format, reduction, recovery, state, and poison code; the sole feature-gated production selection is the individual I/O outcome. Both configurations must run the same nonfault golden-frame, five-event append/reopen, complete-corruption, poison, and resume corpus and produce byte-identical successful files/receipts/reductions/recoveries. The feature-off surface and the feature-on **nonfault projection** must be identical; the entire allowed feature-on-only delta is exactly the named `fault_injection` module with `FaultScript` and its four methods plus `FileJournal::open_or_create_with_fault`.

The two feature-on-only open faults have literal integration falsifiers. Both constructors inject `io::ErrorKind::Other`; each test asserts that exact source kind. `control::directory_sync_failure_has_exact_persisted_header` uses `directory_sync_error` on new-file creation and must observe `Err(OpenError::Io { step: OpenIoStep::DirectorySync, source })` with `source.kind() == io::ErrorKind::Other`, no returned `FileJournal`, and exactly the 32-byte independently constructed golden header at the path (length 32, byte-for-byte equal). `control::tail_repair_sync_failure_has_exact_truncated_prefix` starts from the independently constructed 32-byte header plus one checksum-valid 92-byte `Requested` frame and one torn trailing byte, then uses `tail_repair_sync_error`; it must observe `Err(OpenError::Io { step: OpenIoStep::TailRepairSync, source })` with `source.kind() == io::ErrorKind::Other`, no returned `FileJournal`, and exactly the original 124-byte header-plus-frame prefix at the path (length 124, byte-for-byte equal; the torn byte is absent). These tests assert a concrete faulted I/O step and source, never merely `is_err()`.

Resources: one retained exclusive journal `File`; during creation only, one transient parent-directory `File` may be open to sync the directory and must be dropped before the journal is returned. This is a structural proof, not an unapproved public descriptor counter: the creation function owns the directory `File` in a lexical block that encloses its `sync_all`, that block ends before `FileJournal` construction/return, and the private `FileJournal` field audit permits exactly one `File` field (the journal). Therefore retained journal owners are `1`, creation peak descriptors are `2`, and reopen/append peak descriptors are `1`; a directory `File` field or close-before-sync falsifies the law. Other storage is `[u8; 32]` header; exactly one transient `[u8; 92]` frame/scan buffer; and stack BLAKE3 state. Normal nonempty append/reopen allocates zero heap objects after setup and retains no record collection. The honest copy claim is one necessary 68-byte canonical-record-to-stack-frame copy plus OS copying the 92-byte stack frame to kernel I/O; it must not claim zero copies. A accepts one logical frame write and one file sync per accepted command, one header write/file sync/directory sync on creation, batch size one, group-buffer high-water zero.

Nested package: `nudox-durable-publication-heart` edition 2024 with normal direct dependencies only `blake3 = { version = "1.8.2", default-features = false }`, `nudox-id = { path = "../../crates/nudox-id" }`, `nudox-workflow = { path = "../../crates/nudox-workflow" }`, and `thiserror = { version = "2", default-features = false }`; dev-only `allocation-counter = "0.8.1"`, `trybuild = "1.0.118"`; feature `fault-injection = []` only. `nudox-id` is charged only for the existing canonical-byte trait and no other normal dependency is authorized. No default fault feature is allowed.

## Evidence matrix and budgets

| Law | Exact falsifier |
|---|---|
| reduce before write | first-frame `Admitted` append returns source-bearing reduction error and preserves bytes/state |
| fixed bytes/checksums | golden header/frame and mutation priority table including all header/frame classes |
| outgoing canonical bytes | feature-off adapter golden imports `FixedCanonicalRecord`, copies `WorkflowRecord::canonical_bytes()` to frame `8..76`, and compares all 68 bytes to an independently constructed record before checksum; production source audit requires the same trait call and rejects substitute encoders |
| sync/effect/poison | completed `PublicationStarted` write then sync error returns `OutcomeUnknown`, old recovery, no receipt/effect, poisoned handle |
| all write prefixes | each `0..=92` error: `0..=91` repairs to prefix; `92` reopens as the new valid effect |
| source-bearing reopen | checksum-recomputed invalid event tag returns `FrameDecode`; checksum-recomputed first-frame `Admitted` returns `FrameReduction`; both preserve full length |
| arithmetic before I/O | private boundary covers `u64::MAX` and first overflow, exact error operands/boundary, no observable state/effect/receipt/I/O |
| reopen/resume | normal and one-byte-repaired reopen append next event at exact end/next sequence and preserve prefix |
| tail/corruption/path | all `1..=91` tails repair; complete checksum/sequence/decode/reducer corruption rejects; stable directory/symlink/nonregular fixtures reject |
| negative authority | external `trybuild` rejects a receipt literal and raw conversion |
| paired success authority | external `trybuild` rejects an `AppendSuccess` literal mixing a receipt/reduction from separate successes |
| allocation/retained-log audit | isolated warmed allocation counter reports zero post-setup heap allocations for nonempty append and reopen/replay; the exhaustive production audit manually classifies every dynamic collection/alias/wrapper/builder hit and rejects direct or indirect `WorkflowEvent`/`WorkflowRecord` containment, while allowing only the one transient `[u8; 92]` scan buffer; seeded `VecDeque<WorkflowRecord>` and wrapper-held `BTreeMap<u64, WorkflowRecord>` must make that audit fail |
| descriptor ownership | structural source proof requires the parent-directory `File` lexical block to enclose `sync_all` and end before `FileJournal` construction; private field audit permits exactly one journal `File` field, proving creation `2` then retained/reopen/append `1` and rejecting a retained directory field or early close |
| directory-sync source/persistence | feature-on `directory_sync_error` open test asserts exact `DirectorySync`, `source.kind() == Other`, no returned owner, and exact 32-byte golden-header image/length |
| tail-repair-sync source/persistence | feature-on `tail_repair_sync_error` open test asserts exact `TailRepairSync`, `source.kind() == Other`, no returned owner, and exact 124-byte golden prefix image/length after removing the one-byte tail |
| feature/dependency cleanliness | the identical nonfault golden/reopen/corruption/poison/resume corpus runs feature-off/on and compares byte/result plus nonfault-surface identity; feature-on-only inventory is exactly the named fault module/type/methods and open method; source audit permits `#[cfg(feature = "fault-injection")]` only on fault-script/I/O selection; normal tree has exactly four direct dependencies with `nudox-id` used only for `FixedCanonicalRecord`; report release test-executable bytes and dependency tree alongside the retained control, exact surface/tripwire, and two clean transcripts |

The test oracle must independently model only the five legal event/recovery rows; it may not call production transition, encoding, checksum, or replay helpers. Except for the expressly authorized private arithmetic unit, tests are ordinary top-level integration tests. Production cap is 500 normally formatted LOC (estimate 432; reserve 68): manifest 22, `lib.rs` 55, and one `journal.rs` 355. Test cap is 560 (estimate 488; reserve 72): private arithmetic unit 24, `control.rs` 380, `receipt_surface.rs` 12, and three UI source/stderr pairs 72. File ceilings are guardrails, not extra authorization: arithmetic unit 30, `control.rs` 420, `receipt_surface.rs` 18, and all UI fixtures 84, but the simultaneously authorized test forecast is only 488 and the 72-line reserve remains untouched. For each file, stop before starting another file when actual normally formatted LOC exceeds its named estimate by either 20% (rounded up) or 25 lines, whichever is reached first. For a phase, stop before further edits when actual written LOC plus the named estimates for every unstarted owned file exceeds the cap minus reserve (432 production; 488 tests). Stop and split immediately on an unplanned surface/dependency/authority.

## Commands and next decision

```text
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2 && cargo test -p nudox-workflow
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo fmt --check
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo test --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo clippy --all-targets -- -D warnings
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo test --release --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo test --features fault-injection --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo clippy --features fault-injection --all-targets -- -D warnings
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo test --release --features fault-injection --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart-build/workspace2/adapters/durable-publication-heart && cargo tree --edges normal
cd /private/tmp/nudox-prototype-durable-publication-heart-build && rg -n '(^|[[:space:]])(pub|unsafe|dyn|Box|Vec|Arc|Rc|unwrap|expect|unreachable!|map_err)' workspace2/adapters/durable-publication-heart/{src,tests}
cd /private/tmp/nudox-prototype-durable-publication-heart-build && rg -n 'Workflow(Event|Record)|Vec|VecDeque|LinkedList|BTree(Map|Set)|Hash(Map|Set)|BinaryHeap|Box|Arc|Rc|Cow|SmallVec|ArrayVec|collect|from_iter|with_capacity|try_reserve|reserve|extend|push|insert|type |struct |enum ' workspace2/adapters/durable-publication-heart/src/{lib.rs,journal.rs}
cd /private/tmp/nudox-prototype-durable-publication-heart-build && rg -n 'FixedCanonicalRecord|canonical_bytes|FileJournal|\bFile\b|directory' workspace2/adapters/durable-publication-heart/src/{lib.rs,journal.rs}
cd /private/tmp/nudox-prototype-durable-publication-heart-build && git status --short
```

Run a fresh Luna reader/plausible-misreader and fresh blind Terra review against this card before build. Their fresh proof boundary and a committed pre-edit review must clear all blockers/majors before authorizing exactly one Luna builder checkpoint.
