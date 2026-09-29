# Bounded borrowed membership seam for typed lineage

This note sketches the smallest path from the current durable
`UnprovenTypedLineageEdgeSetV1` to a real verified lineage view. It is a
design note, not an implemented reader or a claim that cold replay currently
validates endpoint membership.

## Reusable V2 row contract

SPIR already has the needed identity ordering. `CoreDeclarationRows` uses
`declaration_plane_key`, which is the exact 32-byte
`DeclarationIdentity` (`family || variant`), not a hash. Core rows are
emitted in that key order. `decode_core` reads the identity from the payload,
and `decode_semantic_plane_segment` checks each row against the stable key.
`CanonicalSemanticPlaneSegmentView::records()` is a borrowed cursor, so a
consumer can test exact membership without constructing `Ir`, an
`SemanticImageView`, or an all-declarations map.

The existing cold path already lends one verified segment at a time through
`TypedPlaneSegmentSourceV2`, and
`verify_semantic_typed_plane_inventory_v2_with_segment_source` visits the
Core family in manifest order. Add an optional Core identity probe at that
point, inside the existing Core row loop. The probe compares each validated
`record.key()` against a sorted, unique request vector. It advances one
request cursor as row keys increase and marks exact matches. Query IDs are
small indices into the caller's bounded request vector; no semantic rows or
their payloads are copied.

The probe result must not escape as verified evidence until the complete
seven-family verifier has succeeded and produced the exact
`SemanticGenerationRootV2`. A suitable result is a root-bound membership
receipt with `contains(identity) -> Option<bool>`: `Some(true)` and
`Some(false)` mean the complete Core scan answered that exact requested ID;
`None` means it was not requested or the receipt is for another root. The
history adapter must match both commit ID and the cold-computed generation
root before using the receipt to answer `LineageHistoryEvidenceV1`.

## Query bounds and replay flow

For one edge set, query only identities used by its rows:

* A rename asks for source and target in the direct parent and asks for both
  again in the child. The verifier needs source-present/target-absent in the
  parent and source-absent/target-present in the child.
* A resurrection asks for its unchanged identity in the exact origin, direct
  parent, and child. It also needs a verified strict-ancestor answer for that
  origin commit/root.

Deduplicate queries by `(commit, generation root, DeclarationIdentity)` and
sort each generation's identity list. Four requests per edge is a safe upper
bound before deduplication, so the current 65,536-edge limit caps raw identity
request bytes at 8 MiB. Allocate with checked arithmetic and fallible reserve.
Also cap the number of distinct resurrection origin commits (for example, 32)
and total cold-replay bytes across those origins. Exceeding either limit must
leave the whole lineage set `Unproven`; it must not produce a partial
`VerifiedTypedLineageEdgeSetV1`.

The child already goes through exact closure and V2 verification in
`replay_typed_v2_history`. Add the child probe to that same scan and retain only
its bounded receipt. Then load the exact first-parent commit, verify its V2
locator/closure/root with the same V2 cold path, and capture its receipt. For
each distinct resurrection origin, prove the commit/root relationship through
the first-parent DAG, cold-verify that exact V2 generation, and capture only
the requested membership answers. Reuse one origin's receipt for all edges
that name the same exact commit/root; discard it after the lineage check.

Ancestry and content are separate facts. Existing
`history_ref_ancestry_proof` proves a commit is on the named ref's exact
first-parent chain, but its record root remains a claim until that generation
is cold-replayed. The edge verifier should require both the chain proof and a
receipt rooted in the recomputed content. A same root on a sibling commit is
not an ancestry proof. Current V2 history rejects merge closures, so do not
interpret second-parent membership as sufficient.

## Why this stays bounded

The observer adds storage proportional to the requested edge endpoints, not
to Core row count. It reuses the existing validated SPIR segment cursor and
does not retain declarations after each key comparison. This does not remove
the current inventory verifier's own budgeted per-family `StableRowIndex`
work; the lineage change should not add another O(rows) map on top of it.
Future work may make that verifier more streaming, but it is not required for
the lineage membership seam.

Avoid reopening or re-lowering an NXFI generation to answer these queries.
The history locator's c005-to-FileStore bridge selects the physical object
for each semantic segment, while the manifest's Core family and exact
stable-key ranges select which semantic rows are being checked. A future
path-copy bridge index can replace the flat bridge lookup without changing
the Core identity probe. The complete c005 inventory, every selected segment
envelope, payload closure, cross-family verification, and GC pin remain
mandatory; the probe is an observer on the existing authenticated replay,
not an alternate authority path.

## Verification boundary

`LineageHistoryEvidenceV1` can be backed by three narrow providers: direct
first-parent record/root lookup; root-bound Core membership receipts; and
strict first-parent ancestry lookup for origins. The separate
`LineageAttestationVerifierV1` must load and validate the external proof for
the exact `LineageConfirmationStatementV1`. No similarity score may implement
that authority. If a receipt, ancestry proof, or configured authority is
unavailable, return the current explicit `Unproven` view or fail closed;
never translate missing evidence into a verified negative membership result.

The final verified object should borrow the replay's exact edge bytes and
remain tied to its GC pin. It may expose `VerifiedLineageStatusV1`, including
`Confirmed` only after the external authority succeeds, while preserving both
original declaration identities. It must never change semantic row keys,
stable links, generation roots, or the V2 content/identity rules.
