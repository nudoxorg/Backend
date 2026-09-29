# Typed immutable lineage-edge contract

**Prototype base:** `d1d5a9a19` (`codex/index-compiler-tentpole`). The module
`crates/replication/src/ir_generation_store/history/lineage.rs` is source-only
in this change: no V2 history record points to it yet, and no Cargo command was
run.

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
a caller-supplied `LineageAttestationVerifierV1` to validate that object for
the exact canonical edge bytes and the parent commit, parent root, and child
root. The child history commit ID is excluded because it commits to the
lineage object and including it would form a hash cycle. The reject-all policy
is the default. No
heuristic matcher or structural similarity score can mint confirmation.

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

The V2 history cold verifier independently admits every typed family and
returns `VerifiedTypedPlaneContentV2`, from which the exact V2 generation root
can be captured. The current history `HistoryTypedV2RootClaim` commits the
untrusted content root, generation root, FileStore closure, and locator, but
does not include a lineage root. The cold typed history path verifies semantic
content; it does not yet return a root-bound `SemanticReader` suitable for
historical ancestry queries. Consequently this prototype deliberately has no
adapter that accepts an arbitrary reader/root pair. An implementer of
`LineageHistoryEvidenceV1` must bind each reader to the exact roots through
cold V2 verification and must walk validated history records for ancestry.

This keeps the API from becoming dead code only if V2 commit publication and
replay use it end to end. The required next consumer seam is:

1. Extend the child V2 history claim and codec with the lineage object ID. The
   publisher must include the object in the exact FileStore closure before
   commit admission; the history commit hash then binds the lineage root.
2. On cold replay, read the edge-set object only after closure admission,
   parse its bounded borrowed view, compare its roots with verified parent and
   child content, and implement `LineageHistoryEvidenceV1` from verified
   readers and commit ancestry. This is where `SemanticDiff` or
   `SemanticEntityChanges` can supply exact direct add/delete facts without
   allocating an `Ir`.
3. Add one consumer that answers a user-visible lineage query or explains why
   a candidate is ambiguous/unresolved. Do not retain and serialize these
   records without a consumer.

No change to semantic identity is needed. The history commit must bind the
edge-set object ID, but the edge set itself contains only the child generation
root, not its own child history commit ID; this avoids a cycle. Current V2
history rejects second-parent-only payload closures, so the first integration
should remain first-parent-only rather than inventing merge ancestry semantics.

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
The independent Python reference model in
[`typed_lineage_reference_model.py`](typed_lineage_reference_model.py) runs
generated small edit histories and fault mutations without calling the Rust
producer/consumer. Its results validate the contract's expected outcomes, not
Rust compilation.
