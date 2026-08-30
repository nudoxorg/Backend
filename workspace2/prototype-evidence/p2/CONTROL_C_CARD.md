# P2 durable-publication candidate C: receipt-gated compact head

## Candidate and authority boundary

Candidate C changes one axis after a retained B result only: one single writer receives a stable
private prototype receipt and conditionally publishes a compact head through compare-and-swap or
write-temp/fsync/rename semantics. It makes no shared `Published` constructor, identity edit, or
cross-process correctness claim. The only admissible head target is journal bytes already proven
stable by the supplied receipt; a head must never name absent, unflushed, partial, or merely
enqueued bytes.

The head format, permanent naming grammar, platform CAS primitive, and public authority type are
intentionally unfrozen: absent a stable B journal/receipt, choosing any would manufacture an
unreviewed permanent protocol. C therefore has no builder-authorized path or public surface.

## Required future proof matrix

A calibrated C card must bound one writer, head bytes, CAS/rename attempts, temporary-file bytes,
pending head receipts, and waiters. It must fault every journal write length/error, file and
directory sync, group fan-out, duplicate/corrupt/torn frame, every crash prefix/restart, shutdown,
primary-plus-cleanup/join error composition, temporary head write, head file sync, directory sync,
rename, CAS mismatch, head corruption, and stale/duplicate receipt. It measures allocation, copy,
writes, sync grouping, progress, latency, retained bytes, release text, dependency graph, and
recovery work.

The decisive test is a prefix schedule: after every journal/write/sync/head step, reopen both
journal and head; any visible head must decode to an exact receipt whose durable end is no greater
than the existing checksum-valid journal length and whose named sequence/frame is present and
valid. A stale or failed receipt must leave the old head byte-identical. This proof is private
prototype authority only.

## Rejection before builder authority

**REJECT CANDIDATE C / RETAIN BASELINE.** There is no Control A or B stable receipt or retained
journal byte image in this branch. Any C head written now could only name a synthetic/absent frame,
which is the exact condition C must reject. CAS/rename semantics also remain UNVERIFIED on the
current platform and cannot be inferred from a non-existent head test.

The next admissible C card requires a retained B journal with a calibrated stable receipt, a
chosen platform-scoped CAS/rename policy, fixed head grammar, explicit resource bounds, and the
full prefix/fault/reopen proof above. No C source, manifest, lockfile, dependency, measurement,
or shared authority is claimed here.
