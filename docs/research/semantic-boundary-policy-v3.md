# Typed semantic segment boundary policy: c007 wire revision 3

The V2 typed-plane profile now commits a per-family SPIR boundary policy in
the c007 manifest. Its schema and payload wire revision are 3; the revision-2
encoding is rejected because it has no policy fields. The closed algorithm is
`StableKeyHashRampV1`, followed by `minimum_bytes`, `target_bytes`, and
`maximum_bytes`. All sizes include the SPIR header and row framing. The
maximum is bounded by `MAX_SEMANTIC_SEGMENT_BYTES`, and invalid or unknown
parameters fail during manifest decode.

At a current segment size below minimum, the producer keeps adding rows. At
and above target, it cuts before the next row. Between those limits, it uses a
family-scoped BLAKE3 hash of the complete stable key and an integer threshold
that increases linearly with current bytes. The derive-key context is
`backend.semantic.ir.segment-boundary.family-key.v1`; the hash input includes
the family ID, extension profile where applicable, and full stable key. A row
larger than maximum is rejected. A row larger than target but within maximum
can occupy a terminal singleton, and an empty family still carries its policy.

Cold verification runs through `CanonicalSemanticPlaneBoundaryFamilyVerifier`
one strict-decoded segment at a time. It retains O(1) boundary state, enforces
family-wide key order and expected cuts, and closes against manifest row and
segment totals. Segment claims above the policy maximum are rejected before
payload reads. Both materialized and lending aggregate-verification paths must
use the same policy check. This is boundary verification only; it does not
establish a complete reader frontier or make V2 selectable.

The FileStore producer sinks hand caller-owned receipt callbacks one durable
object at a time. A callback can contain a prefix if a later row or family
fails, and a crash before the caller persists the semantic-to-physical mapping
leaves safe but non-resumable orphan objects. The returned GC pin only protects
pending objects; it is not type-bound to complete-family verification or
closure publication. `SemanticObjectAdmissionBuffer` is an unbounded
test/small-pass convenience and keeps repeated occurrence receipts for equal
rope leaves; large producers need a bounded streaming sink, and closure
composition must deduplicate physical IDs. These publication and resumption
guarantees remain unsolved by the boundary-policy change.

The producer still retains an O(rows) key/handle index to sort the family.
That allocation is reported separately from bounded row and segment scratch.
Key-hash metrics count candidate rows and their exact 32-byte key inputs;
payload-hash, stored-envelope, and segment-churn metrics are reported
separately by the real-reader fixture. The deterministic source tests cover
family/key domain separation, hash skew, noncanonical splits, mismatched cold
policy, terminal singleton behavior, and rejection above maximum. The real
reader edit/insert/delete corpus compares ramp SPIR bytes against the
prefix/size layout, 16 KiB ordinal chunks, and ordinal V1 NXFI chunks. Its new
metrics are not reported as measured until the shared Cargo slot runs them.

There is no worst-case locality guarantee: an adversarial run of keys can
miss ramp anchors and keep using size-triggered cuts, so later segment
membership may remain shifted even though each segment stays bounded.
Likewise, flat history metadata can dominate payload deltas: the current c005
descriptors and V2 locator bridge are estimated at about 180 bytes per
segment per commit. At 65,536 segments, that is around 11.8 MiB of repeated
metadata for a one-row edit. A later design should evaluate the existing
backend-version lazy persistent tree over family/key ranges plus a physical
object bridge, with cold verification and GC-reachability requirements.
