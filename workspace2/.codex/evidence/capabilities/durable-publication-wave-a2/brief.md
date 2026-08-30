# Durable publication Wave A.2 — frozen Phase 0 brief

## Public terminal

The Sol-owned journey in `adapters/durable-journal/tests/wave_a2_red.rs` starts with a real
`nudox_hydration::VerifiedGeneration`. A bounded MPSC admission path must accept a legal publication
command, reduce and append its workflow frames through the already-accepted single file owner, and
fan one stable physical group-commit result out to its exact submissions. Only after the immutable
publication fact is stable may it durably publish a compact checksum-validated head and release a
non-forgeable published-generation authority. Reopen must independently reduce the journal and
reconstruct the same published authority named by the visible head.

The journey is deliberately still red at baseline: its last branch returns `StableButUnpublished`.
It is Sol-owned and is never edited by this capability.

## Laws and negative space

- Admission has separate bounded item, frame-byte, waiter, receipt, and group capacities. A full
  queue returns the original command/owner unchanged and changes no accounting.
- Exactly one blocking owner holds the file and typed probe. Producers own neither; this is a bounded
  `std::sync::mpsc` design, not a lock-free claim.
- One bounded reusable group buffer maps one physical write/sync outcome to the exact submissions in
  order. Coherent duplicates are idempotent; one changed fact is a source-bearing conflict.
- Cancellation is visible at admitted, queued, grouped, synced/pre-head, and post-head boundaries;
  it neither releases an effect nor leaks a slot, byte, waiter, or receipt credit.
- Receiver loss, poison, explicit shutdown/join, and simultaneous primary/cleanup failure preserve
  every causal source and owner in typed errors. Any pending API registers then rechecks its wake.
- Every journal and publication/head write length/error, file sync, temp/head sync, rename/CAS,
  directory sync, and every crash prefix reopens through an independent reducer. No visible head may
  name absent, torn, or checksum-invalid immutable publication bytes.
- No async facade, HTTP/transport/object store, serde, dynamic dispatch, `Box`/`Arc` convenience
  owner, unsafe, SIMD, new third-party dependency, workflow/roadmap edit, or product-wide API is in
  this capability. The chief has authorized only the existing workspace path dependency
  `nudox-hydration` so the adapter can consume `VerifiedGeneration`; typed lazy probe events may
  observe enqueue/group/sync/receipt/head only.

## Baseline and custody

- Baseline commit/tree: `b2eb3b249cd2f78826285f7f1e232973dbd2a96b` /
  `9c86a4321dc1f7768a134064d1dc70b6a937144e`.
- Manager checkout: `/tmp/nudox-wave-a2-durable-manager.llvVwW/checkout`, branch
  `codex/wave-a2-durable-manager`; it was clean before Phase 0.
- This capability owns `adapters/durable-journal/**` except the Sol-owned
  `tests/wave_a2_red.rs`, plus this evidence directory. The hydration `publication.rs`, `lib.rs`,
  and its tests remain shared/Chief-owned and unchanged. The adapter may move its already-existing
  dev dependency on `nudox-hydration` to the normal dependency set and owns the first concrete
  `PublishedGeneration` authority because one stable-publication consumer exists.
- The existing `FileJournal` is the invariant owner for header/frame checksum, exclusive file
  ownership, reduction, append sync, poison, torn-tail repair, and independent replay. The new
  bounded service may not duplicate those rules in a producer or test oracle.

## Coupling skeleton

| Module / terminal | Invariant owner | Dependency direction | Control boundary |
| --- | --- | --- | --- |
| `FileJournal` frame group append | durable journal | service -> private journal | reduce -> immutable frame -> group write -> sync -> stable receipts |
| bounded submitter / owner thread | durable journal adapter | producer -> owner, never producer -> file | item/byte/waiter/receipt credits and cancellation |
| immutable publication fact | durable journal adapter | stable receipt -> publication log | fact bytes exist and sync before head work |
| compact publication head | durable journal adapter | immutable fact -> head | temp/head sync -> CAS/rename -> directory sync |
| published authority | durable journal adapter | verified generation + reopened head | private construction consumes real stable proof; cannot be a marker-only transition |
| `wave_a2_red.rs` | Sol | public consumer -> adapter | consumes actual verified generation, not hand-built IDs |

## Safe control and measurements

The safe control is the accepted `&mut FileJournal::append`: one caller, one frame, one file sync,
batch high-water one, zero producer slots/waiters, and a private `StableReceipt`. The candidate must
show why its only added structures earn themselves: fixed admission capacity supplies explicit
overload; one reused bounded frame group amortizes a shared sync; a compact fixed head supplies an
atomic discoverable publication after immutable bytes. Measurements are exact accepted/rejected
item/bytes/waiters/receipts/group high-water, logical frames per sync, write/sync/CAS/rename counts,
allocations after setup, retained buffers, and no new normal dependency. A candidate rolls back any
structure that does not remove one of those observable costs or invalid states.

Terra reproduced the warmed nonempty append control in
`adapters/durable-journal/tests/allocation.rs::warmed_nonempty_append_has_no_heap_allocation_and_exact_control_receipt`.
After one warm append, a legal duplicate append allocated zero heap objects/bytes and returned frame
sequence 1 with durable end `JOURNAL_HEADER_BYTES + 2 * JOURNAL_FRAME_BYTES`. The existing
`persist_frame` control owns one logical fixed-frame write and one `sync_all`; its retained owner is
one `File`, its transient frame is `[u8; JOURNAL_FRAME_BYTES]`, and it makes one 68-byte canonical
workflow-record-to-frame copy. Candidate measurement must report the corresponding group values
rather than claim an unmeasured syscall or copy count.

## Current consumers, proposed terminal, and exact gates

The current consumers are the accepted D0 unit/integration/allocation corpus and the single Sol-owned
red journey. `wave_a2_red.rs` is the only consumer that needs the new authority; no second
publication backend or generic receipt trait has a consumer. Sol incorporated the authorized terminal
refinement in `5e4c1b6f9217cb0d5500b541a3da30dbb0149ffd`; it only changes the owned manifest feature
and the Sol journey, and intentionally fails to compile until this capability exports the terminal.

The frozen Sol-facing terminal is:

```text
PublicationPaths::in_directory(&Path) -> PublicationPaths
PublicationLimits::new(NonZeroUsize queue_capacity, NonZeroUsize group_capacity)
  -> Result<PublicationLimits, PublicationLimitError>
DurablePublisher::create(&PublicationPaths, PublicationLimits) -> Result<DurablePublisher, PublicationOpenError>
DurablePublisher::try_publish(&self, VerifiedGeneration) -> Result<PendingPublication, SubmitError>
PendingPublication::wait(self) -> Result<PublishedGeneration, PublicationError>
DurablePublisher::shutdown(self) -> Result<(), ShutdownError>
DurablePublisher::reopen(&PublicationPaths, PublicationLimits) -> Result<DurablePublisher, PublicationOpenError>
DurablePublisher::published(&self) -> Result<Option<PublishedGeneration>, PublicationOpenError>
```

`PublishedGeneration` is adapter-owned with private construction and exposes the verified root and
dependency-set facts plus public `PublicationFacts`, whose named `stable: ReceiptFacts` is nonempty and
compares equal across reopen. The service derives the single-file workflow key/output from the verified
fact; callers cannot supply a rebindable output. The refined journey is create -> submit its real
verified generation -> wait -> shutdown -> reopen -> recover published, then compares verified and
immutable-publication facts. `PendingPublication::cancel` remains required by DP-04 but is not needed
by this minimal red journey. No Luna card starts until a fresh review approves this packet.

The frozen representation requirement is
`contracts/published-generation.md`: both `PublishedGeneration` and `PublicationFacts` are
`#[non_exhaustive]`, the published authority has an adapter-private seal, and no public constructor or
raw-fact conversion exists. The two public readable verified facts and publication facts remain
available to the Sol journey, while adapter-owned downstream compile-fail doctests must reject an
ordinary literal and a mixed A-publication/B-verified literal. The required runtime companion rejects
mixed immutable-head bytes on reopen.

The resource and physical-fault test schedule is frozen in
`contracts/resource-and-fault-ledger.md`. It binds exact future test names to DP-01..DP-15, separate
item/byte/waiter/receipt/group bounds, every cancellation boundary, every journal/fact/head physical
fault, and every crash prefix. It is review material, not a claim that the tests already exist.

Pinned gates use `nix develop ./workspace2#quality --command sh -c 'export PATH="$NUDOX_STABLE_TOOLCHAIN/bin:$PATH"; …'`:

- focused allocation control: `cargo test --manifest-path workspace2/adapters/durable-journal/Cargo.toml --locked --offline --test allocation`;
- accepted D0 control: `cargo test --manifest-path workspace2/adapters/durable-journal/Cargo.toml --locked --offline --all-targets`;
- strict control: `cargo clippy --manifest-path workspace2/adapters/durable-journal/Cargo.toml --locked --offline --all-targets -- -D warnings`;
- chief red: `cargo test --manifest-path workspace2/adapters/durable-journal/Cargo.toml --locked --offline --features wave-a2-publication-red --test wave_a2_red`.

Before implementation the explicit chief feature gate must fail only on the missing terminal imports,
never by panic. The journey makes no `cancel` call; cancellation remains a focused DP-04 test row.

## `TESTING.md` mapping

SHA-256 of `TESTING.md` is recorded in `index.toml`. Applicable clauses map to matrix rows:

| Clause | Matrix rows / exclusion |
| --- | --- |
| Universal negative state, exact error, unchanged resource | DP-01..DP-06 |
| Durable crash/reopen and exact source errors | DP-07..DP-12 |
| Runtime capacities, cancellation, producer/owner overlap | DP-01..DP-06, DP-13 |
| Publish/replay has no shadow state | DP-09..DP-12 |
| Typed authority and compile-fail construction | DP-14 |
| Allocation/layout/reuse | DP-15 |
| Async wake law | DP-13; excluded until a pending public API exists |
| Foundation/object frame grammar | excluded: accepted `FileJournal` owns it; mutation/reopen is replayed through DP-07 |
| End-to-end actual domain journey | Sol-owned red journey is consumed unchanged; focused adapter integration tests must still kill constant/ignored-input mutants |

## Resolved authority boundary

The chief authorized `nudox-durable-journal` to take the existing `nudox-hydration` workspace crate
as a normal path dependency and to own the first concrete, non-forgeable `PublishedGeneration`.
`VerifiedGeneration` remains unchanged. The adapter-owned type exposes the two verified facts and the
immutable publication/head identity facts needed by the current Sol journey; private construction is
tied to validated stable publication/head work, and reopen reconstructs it only by independently
validating journal, immutable publication bytes, and head. No generic receipt trait, shared typestate,
hydration edit, or future-backend abstraction is authorized.
