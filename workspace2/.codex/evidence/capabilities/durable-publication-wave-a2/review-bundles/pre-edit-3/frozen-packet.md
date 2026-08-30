# Review packet: durable-publication-wave-a2 / pre-edit-2

Review the exported snapshot and this frozen packet only. Do not edit and do not infer builder
rationale.

## Bound scope and custody

Future source paths are `adapters/durable-journal/**` except Sol-owned `tests/wave_a2_red.rs`; this
evidence directory is manager-owned. Hydration source remains read-only. The sole direct consumer of
the first authority is the Sol journey. The chief authorized only the existing `nudox-hydration` path
dependency and an adapter-owned concrete `PublishedGeneration`; no hydration edit, generic receipt
trait, shared typestate, second backend, unsafe, SIMD, `Arc`, `Box`, async facade, transport, serde,
or new third-party dependency is permitted.

## Required terminal

Sol committed feature `wave-a2-publication-red` in `5e4c1b6f`. Its exact feature-gated command is
`cargo test --manifest-path workspace2/adapters/durable-journal/Cargo.toml --locked --offline --features wave-a2-publication-red --test wave_a2_red`;
it has no `-- --ignored` suffix. Its exact public terminal is
`PublicationPaths::in_directory(&Path)`; `PublicationLimits::new(NonZeroUsize queue_capacity,
NonZeroUsize group_capacity) -> Result<_, PublicationLimitError>`;
`DurablePublisher::{create(&paths, limits), try_publish(VerifiedGeneration) -> PendingPublication,
reopen(&paths, limits), published() -> Result<Option<PublishedGeneration>, PublicationOpenError>,
shutdown(self)}`; `PendingPublication::wait`; and `PublishedGeneration` fields `pinned_root`,
`dep_set`, and `publication`. `PublicationFacts` has public `stable: ReceiptFacts`, with equality
across reopen. The public journey creates paths/limits, submits a real `VerifiedGeneration`, waits,
shuts down, reopens, and compares these facts. Private published construction must consume a verified
fact plus stable immutable publication/head path; reopen validates journal, immutable publication
bytes, and head independently. Cancellation remains a focused DP-04 requirement and is not called by
the Sol journey.

## Laws to attack

Full admission returns exact input/accounting unchanged; distinct item/byte/waiter/receipt/group
bounds remain exact; producers never own file/probe; exact duplicate is idempotent and one changed
fact conflicts; all admitted/queued/grouped/synced/pre-head/post-head cancellation boundaries,
receiver loss, poison, shutdown/join source composition, every physical write/sync/head/CAS-or-rename
fault and crash prefix have independent-reducer falsifiers; no visible head names absent or invalid
bytes; a published authority is unforgeable and mixed-owner construction fails.

## Existing baseline and controls

`FileJournal` alone owns frame bytes, reduce/write/sync/poison/replay. The warmed nonempty
`FileJournal::append` control is zero post-warm heap allocation, one retained file, one transient
fixed frame, one logical write/sync, and exact second receipt sequence/end. The candidate must report
its fixed group and per-submission resource delta. Pinned focused/all-target/Clippy/red commands are
in the brief and must be checked for exact manifest, locked/offline, stable-toolchain PATH, and
current consumer binding.

## Snapshot and role bindings

Review manager commit `4c60694ddf25744dcde60936eef486a6c492df4e`, tree
`a15886a9609860b01be80b01b106b9cc7e494a63`, exported without Git history or manager evidence. This
packet SHA-256 is recorded in `index.toml`; its prior reviewer receipt and raw tripwire table remain
in `reviews/pre-edit-1.md`. The fresh reviewer must be the registered `nudox_terra_reviewer` at
`gpt-5.6-terra`/`xhigh`, with the direct consumer ledger (`wave_a2_red.rs`, D0 tests, allocation
control) and every pinned gate above bound in its raw result.

## Required reviewer output

Return ranked evidence-backed findings, the complete literal tripwire table, strongest counterexample,
cleared suspicion, simplest standard-library control, platform/tooling gaps, effective sandbox/root
facts, source before/after digest, and approval/rejection. A missing Sol refinement or resource proof
is a finding, never assumed green.
