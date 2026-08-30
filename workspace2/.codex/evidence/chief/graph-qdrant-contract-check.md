# Chief graph/Qdrant contract check

Date: 2026-08-30. This is research input for the active graph/vector controller, not implementation,
review approval, or a roadmap closure claim.

## Trustfall boundary

Trustfall 0.8.1's public `Adapter` methods return `Box<dyn Iterator<...>>` from synchronous methods.
The API requires context order/cardinality preservation and offers invariant checking, but it is not
an async lending-stream interface. Therefore the product contract must not hide remote awaits inside
the adapter or advertise this boundary as monomorphized/zero-allocation. The clean split is:

```text
typed async request -> bounded partition leases -> pinned resident graph view
                                                   -> Trustfall server adapter/query
```

A channel-backed blocking bridge remains eligible only as a measured adapter control; it must name
executor blocking, capacity, wake registration/recheck, cancellation, owner return, and per-query
allocation. Primary source: https://docs.rs/trustfall/latest/trustfall/provider/trait.Adapter.html

## Qdrant identity and payload consequences

Qdrant point IDs are `u64` or UUID. Payload is JSON-shaped; searchable exact scalar fields include
keyword, signed 64-bit integer, bool, and UUID. Our 32-byte typed identities therefore cannot be
honestly represented as raw point IDs or integer payload filters. The adapter must store the complete
canonical encoding in indexed keyword fields. A physical point coordinate is disposable and any
deterministic truncation needs collision detection. Sources:

- https://qdrant.tech/documentation/concepts/points/
- https://qdrant.tech/documentation/concepts/payload/

Array filtering is existential over members. Independent conditions on object arrays may bind
different elements unless Qdrant's nested condition is used. Literal integration tests must cover
the exact payload schema rather than mocking filter truth. Source:
https://qdrant.tech/documentation/search/filtering/

## Consistency and projection publication

Qdrant's Raft consensus governs cluster/collection metadata, not point data. Default point writes are
weakly ordered and default reads use one replica; concurrent writes and lag can make repeated results
blink. Immutable Nudox segments avoid same-point concurrent mutation, but a projection still cannot
become queryable until upload completion and an independent expected-segment validation succeed.
Write `wait`/ordering/consistency and query read consistency/shard selection/route affinity are
adapter policy that must appear in typed provenance. Missing or lagging data yields partial/degraded,
not empty complete. Sources:

- https://qdrant.tech/documentation/scaling/consistency-guarantees/
- https://qdrant.tech/documentation/scaling/horizontal-scaling/

Payload tenant indexes can co-locate snapshot data, while user-defined sharding provides stronger
placement isolation at higher shard overhead. Neither mechanism is semantic snapshot identity.
Collections should follow vector schema/model compatibility; snapshots/segments remain indexed
payload selectors. Source: https://qdrant.tech/documentation/manage-data/multitenancy/

## Ranking law

HNSW and quantization are approximate, and Qdrant documents that changing result limits can change
the overlapping candidate set. Stable exact equivalence therefore requires exact search or exact
local scoring over a proven-complete candidate set. Overfetch plus exact local reranking is a useful
quality control, not a completeness proof; it must return approximate provenance with a stable typed-
identity tie-break. Source: https://qdrant.tech/documentation/faq/qdrant-fundamentals/

## Required chief attacks on the eventual candidate

1. Attempt to pass a 32-byte snapshot/segment ID through a Qdrant point ID or signed integer filter.
2. Force a physical point-ID collision while canonical payload identities differ.
3. Change payload scalar type, use independent array-object filters, and remove the payload index.
4. Make one shard/replica lag after projection publication and reject empty-complete substitution.
5. Reorder/drop/duplicate Trustfall contexts and run the upstream adapter invariant checker.
6. Cancel while an async graph lease is pending and prove every item/byte credit returns once.
7. Compare exact local scoring with Qdrant exact mode, then prove approximate mode is typed as such.

