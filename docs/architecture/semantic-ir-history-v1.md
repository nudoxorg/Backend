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
reservation may conservatively consume a slot. Reaching the limit fails the
commit before ref or `HEAD` publication. The V1 slice does not yet reclaim
unreachable bridge files after branch deletion, so this fixed bound is an
explicit interim retention policy; extending it requires bounded map
mark/sweep under the same ref digest and owner lock.
The current history sweep also leaves unreferenced local generation records on
disk so replay metadata remains available. V1 has no generation-record
compaction yet, so metadata usage is not bounded by the segment-map limit.

| Earlier `nudox-ir-vcs` behavior | Restored in V1 | Remaining boundary |
| --- | --- | --- |
| Persistent commit ancestry and replay | Immutable commit DAG, cold metadata reopen, bounded first-parent replay pages | Replay does not yet emit a complete two-parent semantic merge plan |
| Branches and tags | Named refs, expected-value CAS, atomic rename, navigation-only authority | The implicit branch is local cache history, not an index selection ref |
| Patch deltas | Borrowed manifest-segment deltas using existing exact-root cursor checks | No materialized whole-snapshot patch archive |
| Merge semantics | Commits can name and validate up to two parents; mark/sweep follows both | No three-way IR merge engine; payload closure composition currently advances the first-parent line |
| GC and serving pins | Bounded indexed history mark/sweep; FileStore closure roots retain segment objects for named refs; readers pin against FileStore GC | Generation records are not compacted; segment-ID bridge files have the hard 65,536-entry bound above; no scan-resistant whole-archive serving cache |
| Cold checkout | Named-ref reads can serve verified segment ranges after forced FileStore GC and restart | Full NXFI image and `SemanticReader` checkout still need hydration/reconstruction |
| Collision and failure handling | Domain-separated IDs, checksummed records, atomic ref catalog, fail-closed missing/corrupt live ancestry, retry-safe index intent | Cross-process fault tests and the full gated Cargo suite are pending |
