# V2 reference model

`reference_model.py` is a small Python standard library model of the V2 laws.
It is a correctness oracle, not a performance implementation. Every map
mutation rebuilds all logical nodes from sorted values (`O(N)` construction),
while a content addressed cache reuses equal immutable nodes to model CAS
structural sharing. It makes no production efficient update claim.

The canonical map uses sorted string keys, complete `ObjectVersion` values
(payload, availability, and authority references), key-hash anchored leaf cuts
with minimum 8, target 32, and forced maximum 64, plus the same anchored rule
for parents (minimum 2, anchor probability 1/8, maximum 16). Anchor hashes are
domain-separated by level, avoiding correlation with previously selected cuts.
The final short tail, including a singleton parent tail, is allowed; each full
grouping pass still strictly reduces node count. Value changes leave
key cuts unchanged. Roots are independent of insertion history.

`MapDelta` checks the exact base root and every before value. It supports
composition only for adjacent roots and exact inverse application. `WorkspaceRoot`
hashes all relation roots together, so a workspace delta cannot accidentally
combine a relation from one snapshot with another relation from a different
snapshot. This reference root is intentionally simplified to relation-name/root
pairs; production must add schema, basis, and coverage bindings to the root
header. `join_delta` implements the three old-state terms, including the
simultaneous cross term and signed weights.

`Trace` is a deliberately one-dimensional timestamp example. Compaction keeps
timestamps at or after a released frontier and therefore preserves observations
that remain allowed; historical observations require the old trace or an
immutable historical checkpoint. This models hot trace retention separately
from historical state roots.

Run:

```text
python3 -m unittest -v test_reference_model.py
```

The value vocabulary is intentionally restricted to null, booleans, integers,
strings, ordered sequences, and dictionaries with string keys. Canonical bytes
are frozen; reading a value returns a detached copy. Floats, bytes and opaque
Python values are rejected rather than assigned ambiguous encodings. This is
not the backend's canonical schema or production hash format.

Plain and zlib physical encodings decode into the same canonical map. The toy
codec is only a round-trip law example, without production decoder quotas,
authentication, storage durability or safe malformed-pack admission.

The tests include a seeded randomized whole-map oracle comparison and a 10,000
object CAS diff count. They assert structural counts and changed-node visits,
not throughput. On the final reference run, the 10,000-object map contains 332 nodes
and 291 leaves; a one-value edit makes 26 node-pair probes and finds one unequal leaf.
Unequal tree shapes use a deliberately broad fallback that charges enumerated
nodes and compares leaf hashes; this instrumentation does not emit row deltas.
The separate `delta_to` full-map merge is the row-diff oracle. Neither path is
a production point updater. Antichains, branch conflict merge, crash safety,
remote trust, factorized views and native compiler semantics are not modeled.
The exact twelve-test output is saved in `test_output.txt`.
