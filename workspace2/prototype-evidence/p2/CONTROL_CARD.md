# P2 control journal: frozen first-terminal card

## Scope and custody

| Card row | Literal decision |
|---|---|
| Capability | A blocking, single-owner file journal reduces one `nudox_workflow::WorkflowEvent` before forming its canonical `WorkflowRecord`, writes one fixed frame, synchronizes it, and only then returns a paired private-prototype receipt/reduction result. On reopen it streams physical records to `replay_stream`, retains no log, repairs only a short final tail, and rejects any complete invalid frame. |
| First observable terminal | For the exact legal prefix `Requested → Admitted → Staged(output) → Verified(same output) → PublicationStarted(same output)`, the fifth `append` returns `AppendSuccess { receipt, reduction }` only after its file sync succeeds. Its `reduction.effect` is exactly `EffectAction::Publish { output }`. Drop/reopen returns `Recovery { state: Publishing(output), pending_effect: Some(Publish { output }) }` from the independent oracle. |
| Named baseline | Source candidate `44c22154fd5238e4769562590420371979306050`, clean `/private/tmp/nudox-orchestra`; manager worktree `/private/tmp/nudox-prototype-durable-publication-heart`, branch `codex/prototype-durable-publication-heart`. This card starts at manager commit `c4dad629416178de531a552af5f10e6a14dddf7d`; all named baseline source remains clean and read-only. |
| Exact writable paths | The Luna builder may edit only `workspace2/adapters/durable-publication-heart/Cargo.toml`, `workspace2/adapters/durable-publication-heart/Cargo.lock`, `workspace2/adapters/durable-publication-heart/src/lib.rs`, `workspace2/adapters/durable-publication-heart/src/journal.rs`, `workspace2/adapters/durable-publication-heart/tests/control.rs`, `workspace2/adapters/durable-publication-heart/tests/receipt_surface.rs`, `workspace2/adapters/durable-publication-heart/tests/ui/forge_receipt.rs`, `workspace2/adapters/durable-publication-heart/tests/ui/forge_receipt.stderr`, `workspace2/adapters/durable-publication-heart/tests/ui/receipt_from_raw.rs`, and `workspace2/adapters/durable-publication-heart/tests/ui/receipt_from_raw.stderr`. The manager alone may create or edit only `workspace2/prototype-evidence/p2/{CONTROL_CARD.md,CALIBRATION_ROUND_1.md,CALIBRATION_ROUND_2.md,CONTROL_BUILD_HANDOFF.md,CONTROL_PASS_1.txt,CONTROL_PASS_2.txt,CONTROL_METRICS.csv,CONTROL_TRIPWIRE.txt,CONTROL_SURFACE.txt,CONTROL_REVIEW.md}`. No wildcard path is authorized. |
| Read-only dependencies | `workspace2/crates/nudox-workflow/{src/durable.rs,src/reduce.rs,src/recovery.rs,src/lib.rs,tests/durable_shared.rs}`, every identity/root/runtime/observe path, and all parent workspace manifests/locks. No shared workflow or identity API changes are allowed. |
| Checkpoint decision | This is only control A. Commit and reproduce it before authorizing B's bounded MPSC/group writer. It cannot close P2, export shared authority, or create a head layout. |

## Frozen baseline ledger

Formatted Rust LOC uses `rustfmt --edition 2024 --config skip_children=true --emit stdout`; TOML uses physical lines. SHA-256 is exact file content.

| Path | Role | LOC | SHA-256 |
|---|---|---:|---|
| `workspace2/crates/nudox-workflow/src/durable.rs` | canonical 68-byte record and stream replay | 291 | `82bcae1507e16dabc71f1808f08054cd690b7842d8252108286b45d0160e325a` |
| `workspace2/crates/nudox-workflow/src/reduce.rs` | workflow invariant owner | 536 | `fbc061c9a9e03e9239f1591839d91e7642c1d9738be5a13007cf39f67ac7d6c3` |
| `workspace2/crates/nudox-workflow/src/recovery.rs` | compact recovery owner | 29 | `895a5ade0f4d33c1db239ab5b253639d394d8a634c0246d05bfd0101e198d25b` |
| `workspace2/crates/nudox-workflow/src/lib.rs` | shared exports | 34 | `020d71fcf4d56c217759ed9f2f586c5132eb9a4276f6b10d673ec0514632b08e` |
| `workspace2/crates/nudox-workflow/tests/durable_shared.rs` | contrast only; no file durability | 73 | `a49e42c275ed12d61d0c05f71754024a9b629dd20a54636e00558171c771147c` |
| `workspace2/Cargo.toml` | frozen nested-workspace parent | 79 | `eaf077ae010dc1b464f6a6a5535ce4c019c6017bd1004b2907e3208341fd4286` |
| `workspace2/Cargo.lock` | frozen parent resolution | 1069 | `df7df61d5e1821d259c93791dc7c66d1309b93107c035bdf044f9bedadea6fa4` |
| every allowed adapter path | new nested adapter | absent | absent |

The focused baseline command `cd workspace2 && cargo test -p nudox-workflow` passed 9 unit tests, 1 integration test, and 0 doctests. The repository-root Cargo workspace remains outside this card because its declared `workspace/transport/Cargo.toml` is absent and `cargo metadata` fails before P2 code is reached.

## Exact physical and durability contract

All integer fields are little-endian. `FIRST_SEQUENCE = 0`; `durable_end(sequence) = 32 + (sequence + 1) * 92`, computed checked and rejected with `SequenceExhausted` before any write. The scan begins `expected_sequence = 0`; a complete frame is accepted only when `observed_sequence == expected_sequence`, then advances expected by one checked. Any mismatch returns `FrameSequence { expected, observed }`, leaves every byte unchanged, performs no repair, and returns no journal. A frame's physical version is the validated header version injected into the checksum preimage; it is deliberately not repeated in every 92-byte frame.

```text
Header: exactly 32 bytes, once
  0..8   magic             [u8; 8] = b"NDXP2J\0\0"
  8..10  physical_version  u16 LE = 1
 10..12  header_bytes      u16 LE = 32
 12..14  workflow_bytes    u16 LE = 68
 14..16  frame_bytes       u16 LE = 92
 16..32  checksum          first 16 bytes of BLAKE3(
                               b"nudox.p2.header.v1\0" || header[0..16])

Frame: exactly 92 bytes, repeated
  0..8   sequence          u64 LE
  8..76  workflow_record   [u8; 68] = WorkflowRecord::canonical_bytes()
 76..92  checksum          first 16 bytes of BLAKE3(
                               b"nudox.p2.frame.v1\0" || header.physical_version LE || frame[0..76])
```

`FileJournal::open_or_create(path)` rejects a symlink, directory, or any non-regular metadata kind with `OpenError::Path { observed: Symlink | Directory | Other }` before opening/reading/writing any header byte; its symlink policy is reject rather than follow. It then opens an existing regular file or creates one new journal. For creation it writes the header with `write_all`, calls `File::sync_all`, opens the parent directory read-only, calls that directory `sync_all`, and only then returns a healthy journal. A write/file/directory sync error returns an exact source-bearing open error and no journal. The single-owner law applies to the live journal file; the transient directory descriptor is creation plumbing and is closed before the journal exists.

For an existing journal, validation order is: header read exactness; magic; physical version; header width; workflow width; frame width; header checksum; then each complete frame's checksum; sequence; canonical `WorkflowRecord` decode; reducer transition. A header of `1..31` bytes returns `HeaderTruncated { observed }`; an empty existing path is the same invalid-header class. A full corrupt frame is never truncated. A suffix of `1..91` bytes after a valid complete prefix is a torn tail: truncate exactly to the valid prefix end, `sync_all` the file, and then construct the recovered healthy owner. A read, truncation, or repair-sync error returns a source-bearing recovery error and no journal.

`FileJournal::append(&mut self, event)` has this only success order:

```text
reduce(current_state, event)
  -> encode WorkflowRecord and frame
  -> write_all(frame)
  -> File::sync_all()
  -> replace current_state with reduction.state
  -> return AppendSuccess { StableReceipt, reduction }
```

If reduction fails, no byte is offered and current state is unchanged. If frame write or file sync fails after invocation, `append` returns `AppendError::OutcomeUnknown { attempted: WorkflowRecord, sequence, step: WriteFrame | FileSync, source: io::Error }`, retains the pre-append in-memory state, poisons the journal, and returns no `AppendSuccess`. A poisoned journal permits no new append or recovery observation; only drop plus `open_or_create` reconciles it. No `BufWriter` exists: there is no journal flush operation and no flush durability claim.

The public surface is exactly `pub struct FileJournal` with `open_or_create`, `append(&mut self, WorkflowEvent)`, and `recovery(&self)`; `pub struct AppendSuccess { pub receipt: StableReceipt, pub reduction: Reduction }`; `pub struct StableReceipt` with private fields plus read-only `sequence()` and `durable_end()` accessors; `pub enum OpenError`; `pub enum AppendError`; and `pub enum RecoveryObservation { Healthy(Recovery), Poisoned }`. `OpenError` distinguishes `Path { observed: Symlink | Directory | Other }`, `Io { step: Open | HeaderRead | HeaderWrite | HeaderFileSync | DirectoryOpen | DirectorySync | FrameRead | TailRepair | TailRepairSync, source }`, exact header classes, `FrameChecksum`, and `FrameSequence`. `AppendError` is exactly `Reduction(source) | SequenceExhausted | OutcomeUnknown { attempted, sequence, step: WriteFrame | FileSync, source } | Poisoned`. No other public item, receipt field, receipt inherent method, trait implementation, reexport, conversion, or bridge is allowed. In particular StableReceipt has no constructor, `Default`, `From`, `TryFrom`, raw integer/byte conversion, shared `Published` conversion, head operation, or `DurableAppend` implementation. External compile-fail fixtures reject a receipt struct literal and `StableReceipt::from(0_u64)`; the manager's exact public-surface inventory rejects every unlisted route. A receipt is private prototype evidence, not a shared authority type.

## Method × phase matrix

| Method | Healthy | Poisoned after ambiguous I/O | Reopened/repaired |
|---|---|---|---|
| `open_or_create(path)` | validates/creates then returns recovered state or exact source/format error | not applicable | not applicable |
| `append(event)` | exact reduction error/no bytes; or synced `AppendSuccess` and state replacement; or `OutcomeUnknown` then poison | exact `AppendError::Poisoned`, no bytes/state movement | same healthy behavior after a successful scan |
| `recovery()` | current compact recovery, no I/O | exact `RecoveryObservation::Poisoned`, no guessed recovery | recovered compact recovery, no I/O |
| `drop` | closes the file, no task/join | closes the file, no task/join | closes the file, no task/join |

## Required evidence and fault matrix

| Law | Exact test or raw artifact | Falsifier | Hard cap / stop trigger |
|---|---|---|---|
| Reduction precedes append | `control::rejected_transition_preserves_file` | illegal `Admitted` as first event produces exact reduction source and byte-identical file | record-first write, lost source, or changed state stops |
| Fixed bytes and grammar | golden header/frame in `control`; mutation table | independent test constructs declared bytes, checks stored record bytes, and mutates every named header/frame field class | a second encoding, wrong offset, or undecidable first error stops |
| Stable receipt/effect boundary | `control::sync_fault_releases_neither_receipt_nor_effect` under `fault-injection` | full `PublicationStarted` write then forced file-sync error yields `OutcomeUnknown { step: FileSync }`, old recovery, poison, and no success value | return/retain effect before sync stops |
| Write length/error boundary | `control::write_prefixes_poison_and_reopen` under `fault-injection` | every transferred count `0..=92`, then I/O error; `0..=91` recovers old prefix after tail repair, while `92` preserves the full new frame/recovery without ever returning the original caller a receipt/effect | receipt/effect from failed append or full-frame truncation stops |
| Creation directory durability | `control::directory_sync_failure_returns_no_journal` under `fault-injection` and real create/reopen | header write/file sync success then forced directory sync error has exact source and no journal | ignored/erased directory sync stops |
| State replacement only after stable sync | `control::in_memory_recovery_tracks_each_receipt` | append the five-event prefix without close; after each success compare `recovery()` to independent table; force sync error and assert previous recovery remains | stale/repeated state or advanced failure state stops |
| Streamed reopen and independent oracle | `control::all_legal_prefixes_reopen_to_table` | each legal prefix/duplicate is reopened, consumed one frame at a time, and matches a table that calls no production transition/format/checksum code | retained record `Vec`, shared oracle, or record copy stops |
| Resumable reopen and repair | `control::reopen_and_repaired_reopen_append_next` | normal and one-byte-tail reopen each append the next legal event at the prior durable end, preserve every old byte, use next sequence, and match the next oracle recovery | header/prefix overwrite, reused sequence, wrong seek position, or fork stops |
| Torn tails vs full corruption | `control::all_torn_lengths_repair_but_full_corruption_rejects` | tail lengths `1..=91` repair and sync; complete checksum/sequence/canonical mutations reject and keep length | truncating any full invalid frame stops |
| Path-kind control | `control::non_regular_paths_fail_before_header_io` | directory and symlink fixtures return exact `OpenError::Path`; no journal or header I/O results | follow/hang/incidental I/O error stops |
| Negative receipt API | `receipt_surface` / two `trybuild` fixtures | downstream struct literal and raw conversion do not compile | public raw constructor/conversion/shared publication conversion stops |
| Clean/dependency boundary | two full candidate command transcripts plus `git status --short` | normal tree contains exactly direct runtime deps; every generated/lock state is tracked or fails | unplanned direct dependency/untracked path stops |

The private production-path test seam is feature-gated `fault-injection`, compiled only by this adapter's tests. It contains no queue, runtime, task, `Arc`, trait object, or generic public abstraction. It intercepts only one individual I/O outcome; frame encoding, reducer ordering, journal state transition, poison transition, and recovery implementation are the same code in feature-on and feature-off builds. Its script can: transfer exactly `0..=92` bytes and then return one `io::Error` from write; fail file sync after a complete write; fail directory sync after a header file sync; and fail torn-tail repair sync. Normal builds contain only `std::fs::File` operations. This seam is authorized only in `src/journal.rs`; it is never reexported in a normal build.

## Dependencies, resources, and budgets

The frozen manifest is a nested `[workspace]` package named `nudox-durable-publication-heart` on edition `2024`, with exactly these normal dependencies: `blake3 = { version = "1.8.2", default-features = false }`; `nudox-workflow = { path = "../../crates/nudox-workflow" }`; `thiserror = { version = "2", default-features = false }`. Its only dev dependencies are `allocation-counter = "0.8.1"` and `trybuild = "1.0.118"`. Features are only `fault-injection = []`; no default feature enables it. Expected normal tree has the adapter's three direct normal dependencies and their necessary transitives; no Tokio, futures runtime, serde, database, ORM, tracing, network, or allocator crate is allowed.

| Site | Mechanism/exact bound | Lifetime | Reason |
|---|---|---|---|
| journal | one `File` | one healthy or poisoned journal | exclusive physical owner; no `Arc<File>` |
| header | `[u8; 32]` | create/open | declared geometry, no parser buffer |
| frame | `[u8; 92]` | append/one scan iteration | declared geometry, no retained log |
| checksum | stack BLAKE3 state | one header/frame operation | fixed physical checksum; no bit loop |
| fault script | test-feature `Vec<u8>` only | top-level test | deterministic production-transition faults, excluded from normal graph |
| future B group buffer | exact `Vec<u8>` of `G * 92` | future service | prohibited in A; must earn itself by group result |

Production hard cap is **380 formatted LOC**, estimate **337**, reserve **43**: manifest 25; `src/lib.rs` 72 including `#![forbid(unsafe_code)]`; one proof-bearing `src/journal.rs` 240. Tests hard cap is **320 formatted LOC**, estimate **282**, reserve **38**: `tests/control.rs` 244; `tests/receipt_surface.rs` 12; two fixtures/expected diagnostics 26. Evidence raw files do not count; every source/manifest/lock line does. Recount after each file. Stop and split if any file exceeds estimate by 20% or 25 lines, if current diff plus forecast reaches 60% of either cap, or if any public item/dependency/unsafe/SIMD/allocation policy/platform branch was not named here.

Control metrics: exact header/frame bytes `32/92`; zero normal-path allocations and copies after setup for a nonempty append/reopen; one logical frame write and one file sync per accepted command; one header write/file sync/directory sync at creation; batch size one and reusable-group-buffer high-water zero; queue items/bytes and producer progress/latency are not applicable to `&mut self`. Capture receipt durable ends, recovery frames scanned, sync grouping `1`, normal dependency tree, and release test elapsed distribution. A system-call trace and physical power-loss correctness beyond a returned `sync_all` are not claimed.

## Commands, counterexample, and negative space

```text
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2 && cargo test -p nudox-workflow
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo fmt --check
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo test --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo clippy --all-targets -- -D warnings
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo test --release --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo test --features fault-injection --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo clippy --features fault-injection --all-targets -- -D warnings
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo test --release --features fault-injection --all-targets
cd /private/tmp/nudox-prototype-durable-publication-heart/workspace2/adapters/durable-publication-heart && cargo tree --edges normal
cd /private/tmp/nudox-prototype-durable-publication-heart && rg -n '(^|[[:space:]])(pub|unsafe|dyn|Box|Vec|Arc|Rc|unwrap|expect|unreachable!|map_err)' workspace2/adapters/durable-publication-heart/{src,tests}
cd /private/tmp/nudox-prototype-durable-publication-heart && git status --short
```

Run the complete candidate command set twice from a clean committed candidate. The manager records the exact command/status text in `CONTROL_PASS_1.txt` and `CONTROL_PASS_2.txt`, release measurements in `CONTROL_METRICS.csv`, tripwire in `CONTROL_TRIPWIRE.txt`, public-surface listing in `CONTROL_SURFACE.txt`, and review in `CONTROL_REVIEW.md`. The expected inventory contains only the public items enumerated above and no `unsafe`, `dyn`, `Box`, normal-path `Vec`/`Arc`/`Rc`, `unwrap`, `expect`, `unreachable!`, or source-dropping `map_err`. A generated nested lockfile is allowed only when it is staged with the adapter manifest in the same builder commit; any other generated/untracked path is a red gate.

`root-review-durable` `0ff6f988f449274d1ef49434ece249a6f2f709cc`, from `24be8e7a`, is rejected counterexample corpus only: its adapter gates passed but it added 2,212 lines across 12 paths, overran frozen 375/550 production/test ceilings, split a blocking control across multiple modules, and never reached bounded MPSC/group commit/CAS head. Nothing is copied or cherry-picked from it.

Control A forbids async adapter/future, detached thread/task, MPSC queue, reusable group buffer, head/rename/CAS file, shared `Published` typestate, `DurableAppend` implementation, serde, database, ORM, network/tracing export, lock-free claim, unsafe/SIMD, per-operation `Arc`, or any edit outside exact paths. B can compare only a bounded MPSC service after A reproduction; C can compare only a compact head layout after B. `std::sync::mpsc::sync_channel` is an allowed future bounded blocking control, never lock-free.

## Unverified, plan closure, and next decision

UNVERIFIED: device caches/filesystems that lie about flush; physical power loss; Windows directory sync; NFS/remote rename/CAS semantics; cross-process writer exclusion; MPSC cancellation/drop/join conservation; group fan-out; head never naming absent bytes; async completion; actual syscall tracing; x86/Linux performance. The A fault seam closes only deterministic write length/error, file/directory/repair sync errors, poison, restart, torn tail, duplicate, checksum/sequence/canonical corruption, and recovery prefix.

This card freezes one falsifiable terminal: the private publish effect appears only in a stable paired result, and restart recreates exactly that pending effect; every failed prefix returns no success authority. It intentionally leaves the bounded MPSC reusable group writer and compact CAS head for later cards.

Run fresh Luna reader and plausible-misreader trials plus a fresh blind Terra reviewer attack against this exact card digest. Authorize a Luna builder only if they agree on every literal row and leave zero blocker/major finding.
