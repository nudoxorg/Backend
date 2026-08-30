---
name: build-durable-publication
description: Design, implement, or review workspace2's single-owner durable journal, bounded group commit, immutable publication facts, compact publication head, and published-generation authority. Excludes remote transport and object-store policy.
---

# Build durable publication

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first, then `../../../TESTING.md`.
For the current Wave A.2 contract also read
`../../evidence/capabilities/durable-publication-wave-a2/brief.md`, `proof-matrix.md`, and
`contracts/published-generation.md`. Those files are retained design evidence, not a closure claim;
their valid `EVIDENCE_BLOCKED` receipt must not be relabeled as implementation authority.

This skill owns the local durable transition only:

```text
verified generation
  -> bounded submission
  -> one file owner reduces and appends a reusable group
  -> one physical sync yields exact per-record stable receipts
  -> immutable checked publication fact
  -> checked compact head
  -> non-forgeable published-generation authority
```

Remote quorum, range transport, object-store placement, and general application policy are separate
capabilities.

## Keep one physical owner

`FileJournal` remains the sole owner of the file cursor, workflow state, next sequence, poison state,
and ordered probe. Producers never hold a file or reproduce reduction. A bounded standard-library
MPSC owner service is the safe control; do not call it lock-free. `&mut FileJournal` is honest
exclusive ownership, not an API defect to hide behind a mutex or a misleading `&self` wrapper.

Admission accounts item slots, encoded frame bytes, waiter slots, receipt slots, and group storage as
separate named bounds. Full admission returns the exact command/owner and leaves every counter and
byte unchanged. One reusable fixed-capacity group buffer must replace per-submission allocation; any
retained allocation names its setup point, exact bound, reuse lifetime, and overload behavior.

```text
DON'T: producer -> Mutex<FileJournal> -> one sync per future.
DO:    bounded producers -> one owner -> ordered group write -> one sync -> exact fan-out.
```

Blocking filesystem work stays in the named owner thread. A later async adapter may await bounded
completion, but a ready future around blocking I/O is forbidden. Pending adapters register a wake,
then recheck; cancellation returns every credit exactly once and never converts a stable physical
outcome into an absent one.

## Stable means physically stable

Reduce before writing. A `StableReceipt` is privately constructed only after the complete frame range
has been written and the declared file-sync boundary succeeds. It carries the exact frame sequence and
durable end; no boolean, marker-only typestate transition, or user-supplied receipt is authority.

One group maps its single write/sync outcome to ordered individual receipts. Define short writes,
write failure, sync failure, poison, receiver loss, shutdown, and join/cleanup fan-out without losing
the attempted record, sequence, I/O step, source, or rejected owner. Unknown physical outcome releases
no external effect and requires reopen/reconciliation. Never infer "not written" from an error.

Replay streams fixed records, validates checksums/canonical workflow records, reduces independently,
and retains no log collection. Torn incomplete tail repair is distinct from a corrupt complete frame.
Sequence, checksum, canonical decode, reduction, and I/O failures remain separate typed causes.

## Publication authority follows two durable artifacts

First write a content-addressed immutable publication fact that names the verified root/dependency
facts and stable journal receipt. Validate and sync it before head work. Then publish a compact,
checksummed head using explicitly proven platform semantics: temp/head write, file sync, compare or
rename, and parent-directory sync. Do not call a rename a compare-and-swap or claim crash atomicity
without the exact collision and restart proof.

The visible head may name only an existing checksum-valid immutable fact. A coherent duplicate is
byte-identical and idempotent; changing exactly one fact yields a source-bearing conflict and leaves
the old head unchanged. Reopen independently validates journal, immutable fact, and head before it
reconstructs authority.

`PublishedGeneration` consumes the real verified capability plus an adapter-private validated
publication result. Its readable facts may be public, but construction is sealed and non-exhaustive;
two valid owners cannot be mixed into a third authority. Do not add a generic receipt/backend trait
until two shipping backends need the same invariant.

## Cancellation and shutdown are durable states

Test cancellation at admitted, queued, grouped, synced/pre-head, and post-head boundaries. Before
stability it may suppress work and must return ownership/credits. After stability it cannot erase the
receipt; it may only suppress the caller notification or continue reconciliation according to the
frozen contract. No boundary releases publication effect early.

Shutdown closes admission, drains or rejects every accepted owner deterministically, joins the file
owner once, and returns primary plus cleanup failures without erasure. Drop is a last-resort cleanup
path, not the proof of durable completion.

## Diagnostics and proof

The writer emits coarse typed events for admission outcome, group formation, stable sync, receipt
fan-out, head decision, reconciliation, and terminal shutdown. Producers expose only bounded overload
totals. Disabled probes construct no fields; no per-record string logs or high-cardinality labels.

Required attacks include:

- every capacity at zero/one/exact/+1 and unchanged rejected ownership/accounting;
- groups from one through the bound, exact sequence/offset fan-out, and one sync observation;
- every partial group write and sync failure with independent reopen of the resulting image;
- cancellation at every boundary, receiver loss, poison, simultaneous primary/cleanup failure;
- every immutable-fact/head write, sync, rename/compare, directory-sync, and crash prefix;
- duplicate versus one-fact conflict and mixed head/fact substitution;
- downstream compile-fail construction and mixed-owner `PublishedGeneration` attacks;
- warm nonempty allocation/copy/high-water measurements and retained-buffer accounting;
- exact typed event order with disabled-probe construction at zero.

Use the existing `FileJournal` and its fault/restart corpus as the scalar control. SIMD, unsafe,
custom allocation, a second backend, serde, dynamic dispatch, and HTTP are out of scope unless a new
parent-approved capability and measurements establish them.
