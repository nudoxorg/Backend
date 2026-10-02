# Persistent semantic IR history V1

This slice adds durable ancestry and named navigation over the existing
semantic generation records. It does not create a second semantic row model.
Commit bodies point to the existing immutable generation record and exact
manifest; canonical entities, links, plane coverage, and segment identities
remain owned by `backend-semantic` and the current FileStore.

Each history commit identity hashes an explicit generation-root discriminator,
ordered parent commit IDs, the local generation-record locator, exact manifest
root, selected authority stamp, and admission provenance. V1 currently admits
only `HistoryGenerationRoot::NxfiV1(GenerationId)`. The local record locator
is a separate field and is not the semantic root. Discriminator 2 is reserved
but rejected until history admission consumes the semantic crate's typed
`SemanticGenerationRootV2` verifier capability. That schema cutover must make
the NXFI locator optional and keep the V2 root distinct from content root and
live admission evidence; raw 32-byte claims cannot mint it. No on-disk legacy
migration is required for this unreleased V1 sidecar.

Commit publication writes the immutable generation record, immutable history
commit, recoverable append-index intent and entry, optional cumulative
FileStore closure root, atomic `refs.catalog`, then local cache `HEAD`. A crash
before `HEAD` may leave the `local-cache` navigation ref ahead of the cache
head. The ref is never consulted for index selection. A retry rechecks the
current selected authority before it can align `HEAD`; a moved authority leaves
the old cache head intact. Until that exact candidate is reconciled, a different
candidate cannot inherit the ref-ahead commit as its parent. The append-index
intent records the exact offset so a cold retry recognizes an already durable
entry rather than duplicating it.
Other named branches and tags use expected-value CAS, and rename replaces the
catalog atomically. Readers remain lock-free; writers serialize under the
existing OS-backed `state.lock`, after taking the FileStore shared GC pin.

Replay returns bounded pages along the first-parent chain. Checkpoint markers
help resume work but do not truncate ancestry. Each page retains validated
generation manifests and exposes borrowed `SemanticDeltaCursor` values for
adjacent snapshots. A history entry is metadata; only
`FileSemanticRangeStore::history_materialization` can report
`ResidentSegments`, after checking every manifest segment against the named
ref's cumulative closure and revalidating its exact bytes. A missing bridge or
payload reports `NeedsHydration`. This API serves typed segment ranges; it does
not reconstruct a full NXFI image or a `SemanticReader` from historical
segments.

The cumulative closure reuses the existing FileStore segment objects, so a
new history commit stores only a closure index update and small bridge records.
The current bridge catalog is content-addressed by semantic segment ID and
has a hard per-target limit of 65,536 unique mapping files. Each encoded map is
110 bytes, for at most 7,208,960 bytes of map payload, plus filesystem
metadata. The counter reserves capacity before writing a map; an interrupted
reservation may conservatively consume a slot until the next retention pass
reconciles the sidecar against the map directory. A bounded retention pass
marks maps reachable from every named ref and both-parent ancestry, plus the
current and previous local `HEAD` generations. It sweeps unreachable maps and
generation records, removes unreferenced commit objects, and compacts append
indexes. A durable delete intent makes map unlink and capacity-counter updates
restart-safe. Reaching the live-map limit returns backpressure before ref or
`HEAD` publication; reclaimed slots are available to later commits.
`HistoryGcProgress::stats()` reports live and reclaimed map, commit, and
generation-record counts and bytes.

Retention state tag 11 is the unreleased exact-accounting format. It persists
the durable byte length with each deletion intent and makes that accounting
survive cold restart and interrupted index compaction. A state with the prior
tag 10 is rejected before delete recovery or sweeping: this greenfield format
has no migration path, and it does not reconstruct byte totals for deletions
performed by an older epoch. Exact reclaimed-byte claims apply to work recorded
in tag 11 state only. Index-compaction cursors are committed only after the
staging file and its directory entry are durable, so restart can verify the
exact output offset before either replacing or retaining the index.

History GC acquires the FileStore's exclusive cross-process collection lease
before `state.lock`, matching the shared-lease-then-`state.lock` order used by
writers, admitted proposals, and historical readers. This protects bridge
metadata and FileStore objects as one reclamation boundary. Segment readers
report their pin age, `GcPinGuard::active_count()` reports process-local pins,
and history GC returns an explicit retryable backpressure error instead of
waiting indefinitely while shared readers hold the lease.

`FileSemanticRangeStore::checkout_history_image` now reconstructs the exact
selected V1 image when its core plane has the producer's ordinal NXFI byte
layout. It captures the selected commit ID once, holds the FileStore GC pin
and owner lock to snapshot the reference, immutable generation, and closure.
It drops the owner lock before reading segment bridges and immutable payloads,
verifies each segment, and streams manifest-ordered bytes to a unique private
bounded temporary file. It then verifies full image identity, `GenerationId`,
and NXFI grammar before returning a borrowed `SemanticImageView` over an
anonymous mapping. File leases let concurrent startup recovery skip active
scratch files; abandoned files are removed within a bounded scan. The reader
lease retains the GC pin, and reports segment-validation bytes, segment-copy
bytes, scratch bytes written, and image-validation bytes. Missing payload or
bridge records return a typed `NeedsHydration` value; non-ordinal V2 typed
planes are not reconstructed by this V1 path.

| Earlier `nudox-ir-vcs` behavior | Restored in V1 | Remaining boundary |
| --- | --- | --- |
| Persistent commit ancestry and replay | Immutable commit DAG, cold metadata reopen, bounded first-parent replay pages | Replay does not yet emit a complete two-parent semantic merge plan |
| Branches and tags | Named refs, expected-value CAS, atomic rename, navigation-only authority | The implicit branch is local cache history, not an index selection ref |
| Patch deltas | Borrowed manifest-segment deltas using existing exact-root cursor checks | No materialized whole-snapshot patch archive |
| Merge semantics | First-parent DAG replay is paged, and V1 rejects persisted or proposed two-parent commits | Two-parent publication remains disabled until payload closures are unioned; no three-way IR merge engine |
| GC and serving pins | Bounded indexed history mark/sweep, bridge-map reclamation, generation-record sweep, and append-index compaction; FileStore closure roots retain segment objects for named refs; segment and image readers pin against FileStore GC | Live bridge maps remain limited to 65,536 per target; long-lived readers delay collection; no scan-resistant whole-archive serving cache |
| Cold checkout | V1 ordinal NXFI images can be reconstructed from retained segment closures after image pruning, forced FileStore GC, and restart, then read through the borrowed semantic view | V2 typed-plane checkout and hydration are not implemented; the V1 checkout reports `NeedsHydration` for missing segments or unsupported layouts |
| Collision and failure handling | Domain-separated IDs, checksummed records, atomic ref catalog, fail-closed missing/corrupt live ancestry, retry-safe append and delete intents, and typed rejection of unmaterialized merge proposals | The full gated Cargo suite is pending |

The retention regressions use deterministic fixtures to check ref deletion,
index compaction recovery, orphan-commit cleanup after a crash, bridge-map
unlink recovery and reuse, fail-closed corruption, and a third-old segment and
full-image checkout after retention, FileStore GC, and cold reopen. Cargo
execution remains gated; formatting and static diff checks do not establish
that this slice is production-ready.
