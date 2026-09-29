# IR generation materialization: current boundary and experiment

## Existing V1 adaptation

`AdaptiveIrResidency` already adapts a selected V1 semantic segment among a
borrowed in-memory owner, a checked historical delta-CAS object, and the exact
selected generation's pristine CAS object. Its byte key is the plane plus the
admitted segment identity; ranges alone never authorize reuse. Every exposure
rechecks the current selection and exact target descriptor. Delta routes are
bounded by hop, changed-byte, and action limits, and their measured cold costs
can select pristine CAS when the route is not worthwhile. Prepared routes scan
the bounded action stream once and borrow descriptor indices for subsequent
segment reads. The ordinary local path borrows through `&mut` policy access;
shared ownership is reserved for an explicitly requested lease.

The policy also has scan-resistant hot admission: a fixed aging frequency
sketch, separate probation/protected byte accounting, and cost evidence stored
with an admitted owner. This keeps cold one-pass observations from erasing the
history that justified keeping an interactive hot segment. Relevant code is in
`crates/replication/src/ir_residency.rs`; deterministic pressure, lease, stale
selection, changed-content, prepared-route, and cost-choice tests are in
`crates/replication/src/ir_residency/tests.rs`.

## V2 history is a different proof problem

Typed V2 replay currently has no typed delta cursor or compositional generation
certificate. `FileSemanticRangeStore::replay_typed_v2_history` checks the
supplied ref ancestry, loads the exact commit and locator, opens the claimed
closure, verifies exact membership/schema/length/object identity, spools the
payloads, and runs the all-family semantic verifier before returning
`TypedV2HistoryReplay` (`crates/replication/src/ir_hydration_store/history_v2.rs`).
The returned `VerifiedTypedPlaneContentV2` is a claim-only token: it contains
build/input/family commitments and roots, not the row payloads. It cannot serve
as an in-memory materialization by itself.

The V2 admission path binds the exact locator and closure claim and verifies
all families before storing the commit. Durable closure composition is
path-copy capable in the local history path, but membership only establishes
which physical objects are present. It does not prove row equivalence or
cross-family reference closure for a changed generation. The V2 verifier
derives seven family commitments from one verified inventory and checks them
against the manifest; there is no stored per-family dependency certificate
authorizing a partial replay. The
sparse `StableRowIndex` frontier likewise is not a V2 replay certificate:
`row_index.rs` explicitly says V2 aggregate admission currently indexes all
decoded family records and no production caller supplies a frontier.

Two concrete stale-proof failures rule out a proof-only hot cache:

* If an object file is modified after a successful replay, a cached proof plus
  a fresh GC pin can return success without detecting that the current
  FileStore payload no longer hashes to its object ID. A GC pin protects
  collection lifetime; it does not validate bytes. The cold spool path
  reopens and verifies physical objects, so skipping that check changes the
  observed corruption behavior.
* Two commits may share a segment range or even content root while differing
  in segment identity, input claim, or generation root. Range/content-only
  keys can then attach an old proof to a changed generation. Keys must bind
  the full target, commit, content and generation roots, exact closure and
  locator, verification tier, and jumbo limits; ancestry must still be checked
  for each use.

For comparison, the V1 test
`same_range_with_changed_content_never_reuses_the_old_owner` demonstrates the
first identity rule: matching key bounds with different segment IDs takes the
target CAS path. V2 needs equivalent two-generation and changed-object
adversaries before adding any reuse route.

## Safe experiment, in two stages

Stage A should test exact-generation memory materialization, not proof-only
memoization. A caller-owned, non-persistent policy can admit a complete
generation only after cold verification succeeds and only when the deduplicated
closure payload fits an explicit byte budget. Store immutable owned payload
bytes together with the verified token and exact key; do not retain a mutable
spool path or a borrowed FileStore reader. Keep probation and protected byte
budgets, require repeated use before protected promotion, and reject an
oversized generation without changing the cold path. This makes a hit mean
“the previously verified exact payload is available in this live immutable
memory materialization,” not “the current object files were freshly checked.”
The returned replay must still carry its normal fresh GC pin and fresh ref
ancestry check. A new policy after process restart is empty and takes the full
cold path.

This bounded live-byte stage can avoid repeated physical FileStore reads,
temporary spool writes/reads, and semantic verification for the exact same
generation while it remains resident. It cannot provide a delta-hop benefit
between generations. To avoid one-pass scans displacing interactive
generations, admit into a byte-capped probation pool, promote only on a second
exact-generation use, and admit to protected bytes only when its measured
reuse value per byte beats the protected victim. Keep all entry metadata and
the fixed aging sketch inside the same explicit policy budget. Use a
caller-owned `&mut` policy so the local path does not acquire an `Arc` or
`Mutex` per row.

Stage B may investigate cross-generation family reuse only after defining a
typed compositional certificate. It must bind each family proof to its exact
family root, segment identities, build/image/input roots, and the complete
cross-family dependency boundary. Changed families and all dependent families
must be reverified. If the certificate cannot prove an unchanged dependency
frontier from the parent generation, replay falls back to the existing
all-family cold verifier. Do not infer that frontier from closure membership,
similar key ranges, or a shared content root.

## Deterministic experiment and acceptance counters

Use fixture payloads with known byte totals and event counters rather than
wall-clock claims. Compare the existing cold path, a disabled policy, and the
bounded live-byte policy with these traces:

1. Replay one exact generation repeatedly until protected; then replay a
   one-pass sequence whose aggregate size is more than ten times the cache
   budget. Confirm the protected generation remains resident and subsequent
   exact replay is a hit.
2. Replay the same full scan repeatedly. Confirm all bytes and entries stay
   within limits, probation churn is bounded, and a scan generation is not
   promoted solely from a sketch collision.
3. Use two commits with identical payload/content root but different input or
   generation root, plus two manifests with the same range and different
   segment IDs. Confirm no cross-generation proof/materialization reuse.
4. Change an object file after a cold replay. With an empty/restarted policy,
   confirm cold replay detects the mutation. With a live-byte hit, confirm the
   result is explicitly served from the cached verified bytes and report zero
   FileStore payload bytes read; do not describe that hit as a fresh disk
   integrity check.
5. Make the named ref stale, drop/pin replay leases around collection, and
   corrupt or omit one member before cold replay. Confirm fresh ancestry is
   required, returned tokens keep their GC pins through the existing lifetime,
   and cold failure never admits a cache entry.

Record at least: full cold verifications; cache hits/misses; admissions,
oversize rejections, and evictions; resident bytes/entries split by probation
and protected; FileStore payload bytes read (including the object verification
and spool passes); spool bytes written/read; semantic verifier invocations;
unique closure objects and bytes; and live-memory bytes retained. If Stage B is
attempted, also count per-family verifier invocations, rows/segments decoded,
dependency invalidations, and exact changed-family bytes. A positive result
must show the same roots and failure behavior on cold/restart paths, bounded
memory under scans, and a measured reduction in one of physical bytes read,
spool I/O, or verified semantic rows for repeated exact generations. Report
work counters separately from wall-clock timing.
