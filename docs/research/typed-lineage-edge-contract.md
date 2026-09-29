# Typed immutable lineage-edge contract

**Implementation base:** `531d6c8c8` (`docs: map IR generation materialization policy`).
The typed edge contract is implemented, and typed V2 history now has an
additive locator wire revision that commits exact edge bytes through the
existing locator ID in the history commit. The durable API is intentionally
staged: cold replay returns candidates as `UnprovenTypedLineageEdgeSetV1`
until a borrowed historical declaration reader and confirmation authority are
available. No Cargo command was run in this source-only change.

## Contract

`OwnedTypedLineageEdgeSetV1` is one immutable transition object. Its header
binds the exact parent history commit, verified parent `SemanticGenerationRootV2`,
and verified child `SemanticGenerationRootV2`. Each edge retains the exact
source `HistoryCommitId`, source generation root, source `DeclarationIdentity`,
target `DeclarationIdentity`, relation kind, and evidence status. The target
generation is the edge set's child root. The child history commit ID is
intentionally absent: embedding it would create a hash cycle because the
commit ID will commit to this edge-set object's ID.

The wire is fixed-width and owned at write time, borrowed at validation time.
It has a 108-byte header and 198-byte edge rows, with big-endian counts and
status metadata. The maximum is 65,536 edges and 16 MiB; ambiguity groups are
capped at 4,096 candidates. At the edge-count cap, the wire is 12,976,236
bytes. `BorrowedTypedLineageEdgeSetV1::parse` keeps the original byte slice;
verification decodes fixed-size IDs in place and reserves at most one `usize`
offset per possible ambiguous row (512 KiB on a 64-bit process). The owned
encoder copies the bounded typed edge input once and emits canonical bytes.

`Rename` source endpoints must be from the exact immediate parent root. The
source identity must be present in that parent and absent in the child; the
target must be absent in the parent and present in the child. `Resurrection`
source endpoints must bind a strict ancestor commit **and that commit's exact
generation root**. The source and target identity must be the same for a
resurrection. That identity must exist at the origin and be absent from the
immediate parent, then present in the child. A known contradiction is rejected
for every status. Unknown facts can survive only as `Unresolved`; they never
imply identity continuity.

`Ambiguous` is a full candidate group, with a nonzero group ID, a count from
2 through 4,096, and one unique index for each edge. Missing or repeated
indices, repeated endpoint pairs, and group-ID collisions that merge two
candidate sets are rejected. This proves the recorded alternatives are
complete relative to the producer's declared set; it cannot prove the
producer's similarity heuristic was good, so ambiguity never chooses a
winner. `Confirmed` carries only an attestation object ID. Validation requires
a caller-supplied `LineageAttestationVerifierV1` to validate that object for a
domain-separated digest of the exact canonical edge statement and the parent
commit, parent root, and child root. The digest includes the `Confirmed` tag
and all endpoints but omits the trailing attestation ID, preventing a
content-addressed proof self-reference. The child history commit ID is
excluded because it commits to the lineage object and including it would form
a hash cycle. The reject-all policy is the default. No
heuristic matcher or structural similarity score can mint confirmation.

The verifier context also names the exact child history commit and proves
that the header's parent is its direct first parent. A set cannot claim a
multi-commit jump as a direct rename. Raw parsed `LineageEdgeViewV1` and
`LineageStatusViewV1` remain claims; only `VerifiedTypedLineageEdgeSetV1`
yields the distinct `VerifiedLineageEdgeViewV1` and
`VerifiedLineageStatusV1` types. The owned encoder rejects a zero-valued
`Some` evidence digest instead of silently converting it to wire `None`.

No edge operation changes declaration family IDs, variant fingerprints,
semantic row keys, links, or typed generation roots. A confirmed edge is
display/query metadata only. If a consumer later wants aliasing or migration,
that requires a separate explicit product policy and cannot be inferred from
this record.

## Existing producer and consumer seams

`backend-semantic::ir::SemanticEntityChanges` already performs a borrowed
merge over exact composite declaration identities. A variant change is
deliberately a deletion plus an introduction; it does not guess a rename.
`SemanticReader` supplies canonical rows and exact identity lookups for owned
`Ir` and complete borrowed `SemanticImageView`. `SemanticDiff` also exposes
stable graph-link changes. Those are suitable inputs for a caller to collect
direct additions and removals and then emit only `Ambiguous` or `Unresolved`
candidate records.

`HistoryTypedV2RootClaim` already commits the content root, generation root,
FileStore closure, and locator ID. The new locator tag 15 optionally carries
the exact lineage bytes; because the commit hash includes the locator ID, the
same existing commit identity now binds the lineage without changing the V2
commit body. The edge set names its exact child semantic root and parent
commit/root but omits its own child commit ID, avoiding a hash cycle. Lineage
bytes are retained in the commit's history locator metadata and follow its
existing GC lifecycle. They are not added to the typed payload FileStore
closure; the closure remains the exact segment/jumbo set.

The additive
`admit_typed_v2_history_commit_with_lineage` path checks canonical edge framing,
child root against the manifest and verified content, and parent commit/root
against the first-parent commit record's persisted V2 root claim before commit
admission. Cold replay rechecks that locator/commit binding, verifies child
semantic contents, and requires the exact commit to remain reachable from the
supplied named-ref tip. It does not yet cold-replay the parent payload.
`TypedV2HistoryReplay::lineage_candidates` exposes the borrowed edge stream as
`UnprovenTypedLineageEdgeSetV1`; this is the durable read consumer seam, with a
type name that prevents it being mistaken for verified identity evidence.
Tag-15 decode also validates edge framing, canonical order, endpoint
uniqueness, and ambiguity-group completeness. The byte-for-byte locator ID
check rejects payload mutation; child-root mismatch is rejected against the
manifest; parent commit/root mutation is rejected against the first-parent
commit record.

There is still no V2-backed `SemanticReader` or declaration-membership index
returned by cold replay. The V2 verifier currently retains family commitments
and bounded cross-family facts, not declaration rows. It also does not cold
verify a resurrection's origin content or an external confirmation proof.
Therefore replay does not mint `VerifiedTypedLineageEdgeSetV1` and callers
must treat every returned candidate—including a stored `Confirmed` status—as
unproven display metadata. A parent generation root that matches its history
record is still a persisted claim until that parent closure is cold-replayed.

No semantic identity changes are needed. Current V2 history rejects
second-parent-only payload closures, so lineage remains first-parent-only and
does not invent merge ancestry semantics. Locators retain the existing 24 MiB
bound; a commit whose manifest, bridge vectors, and optional lineage bytes
exceed that total is rejected without widening the accepted metadata budget.

## Row-tree and bridge-index constraint

The related locator issue is separate from lineage: c005 plus the current flat
segment/jumbo bridge vectors are repeated per commit. A typed persistent bridge
tree can localize the bridge projection, but the existing APIs have different
update geometry:

* `PersistentTree::prepare_update` accepts a strictly sorted unique batch. Its
  structural update finds the first and last touched leaves, merges every row
  in that interval, and then path-copies parent levels. A batch of scattered
  edits can therefore re-encode a large intervening range.
* `StableRowIndex::prepare_tree_changes` deliberately applies sparse changed
  keys as single-key path copies. It counts the accumulated `TreeWork`; it
  does not claim a batch splice is always cheaper.
* `LazyTree::prepare_update_bounded` applies sorted edits sequentially through
  an authenticated overlay. It preflights a conservative metadata charge and
  reports peak retained metadata, rebuilt nodes, loaded nodes, and emitted
  bytes.

For a bridge relation, use the lazy bounded overlay for sparse one-row edits,
or use a single `PersistentTree` batch only when the changed key interval is
known to be dense. Record actual `TreeWork`/bounded metadata before claiming
locality. A path-copy bridge root still cannot replace cold c005 inventory
verification, payload-closure checks, GC marking, or range selection. The
bridge index maps semantic object IDs to physical FileStore IDs; c005 family
and stable-key ranges must still select semantic IDs before the bridge lookup.
The detailed byte model and limits are in
[`bridge-index-path-copy-model.md`](bridge-index-path-copy-model.md).

## Checks in this source-only patch

The Rust module includes tests for exact parent/child root binding, complete
ambiguity groups and group-ID collisions, strict resurrection ancestry with an
exact origin root, and rejection of heuristic-only confirmation. Those Rust
tests are authored but unrun because this assignment has no Cargo/build slot.
V2 locator tests cover legacy tag-14 decoding, tag-15 byte commitment, child
root mismatch, parent-commit mismatch, and a mutated lineage payload.
The independent Python reference model in
[`typed_lineage_reference_model.py`](typed_lineage_reference_model.py) runs
generated small edit histories and fault mutations without calling the Rust
producer/consumer. Its results validate the contract's expected outcomes, not
Rust compilation.
