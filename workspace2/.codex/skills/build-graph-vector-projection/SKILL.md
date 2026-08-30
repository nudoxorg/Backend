---
name: build-graph-vector-projection
description: Scope and evidence rules for workspace2 pinned graph queries, Trustfall adapters, immutable vector projections, and Qdrant integration. Use when designing, implementing, or reviewing graph/vector requests, leased batches, projection publication, filtering, ranking, or local/remote equivalence.
---

# Build graph and vector projections

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first, then
`../../../INDEX_GREENFIELD_PLAN.md` and `../../../TESTING.md`. This skill owns the graph/vector
projection boundary only. Canonical IR and index snapshots remain upstream truth; application and
placement policy remain downstream.

## Freeze the public terminal first

Before a backend or dependency enters production, freeze closed typed request, result, provenance,
and terminal records. Every request pins one immutable snapshot and carries explicit fan-out, item,
byte, deadline, and in-flight lease bounds. Results never infer completeness from an empty backend
response.

The terminal distinguishes at least complete, partial with exact missing partitions/segments,
degraded with named approximation/provider cause, cancelled, and failed with its original source.
Graph and vector terminals are distinct even when their stream machinery is shared.

## Trustfall is a synchronous adapter boundary

Trustfall 0.8's public `Adapter` methods return boxed synchronous iterators. Do not describe a direct
implementation as async, lending, monomorphized, or allocation-free. Keep Trustfall in a server
adapter over an already acquired immutable graph snapshot, or demonstrate a bounded bridge whose
blocking, channels, wakeup, cancellation, retained owners, and allocations are measured explicitly.

The portable async seam owns partition acquisition and leased batches; Trustfall does not own I/O,
runtime, retries, caches, or canonical graph storage. Never call async I/O from a synchronous adapter,
block an executor thread without a named blocking boundary, or collect an unbounded graph merely to
enter Trustfall. Use Trustfall's adapter-invariant checker to prove context order and cardinality.

```text
DON'T: async remote reads hidden inside Box<dyn Iterator> returned by Adapter.
DO:    pinned graph lease -> resident borrowed view -> Trustfall server adapter.

DON'T: call the Trustfall adapter itself a zero-cost generic core abstraction.
DO:    keep the typed graph request/stream core concrete; admit Trustfall's dyn boundary in its adapter.
```

## Qdrant is a disposable projection

Qdrant point IDs are physical `u64` or UUID coordinates, not Nudox 32-byte content identities.
Qdrant payloads are JSON-like and exact filters operate over supported keyword/integer/bool/UUID
fields. Store the full canonical snapshot, segment, model, and source identities in indexed typed-
encoding keyword payload fields; never truncate or reinterpret them as signed integers, UUIDs, or
raw byte filters. Any deterministic physical point-ID derivation needs collision detection and an
exact typed rejection.

Array filters have member semantics, and independent conditions on an array of objects may match
different elements unless a nested condition is used. Add literal service tests for every payload
shape and filter the product relies on. An unindexed filter rejection is not an empty result.

Choose collection geometry by vector schema/model, not by snapshot truth. Snapshot and segment IDs
are indexed projection selectors. User-defined shards and tenant indexes are placement controls;
their IDs never enter logical result identity. Publish a projection receipt only after batch upload
completion plus an independent count/identity validation for every expected immutable segment.

Qdrant point updates do not use Raft data consensus. Write ordering, write consistency, `wait`, read
consistency, shard selection, and route affinity must be explicit adapter policy and observable in
provenance. A missing/lagging replica or shard yields partial/degraded, never complete empty success.

HNSW and quantization are approximate. Deterministic exact results require exact search or exact
local scoring over a proven complete candidate set. Overfetch plus local reranking improves quality
but does not prove completeness; label it approximate and specify stable tie-breaking over typed
source identity.

## Ownership and testing

- Local and remote graph/vector paths consume the same immutable logical segment grammar. Qdrant
  responses are candidates validated against the pinned projection, not a second schema.
- Runtime, Trustfall, Qdrant, protobuf, TLS, serde/JSON, and service health stay in nested adapters.
- Core streams lend caller/lease-owned batches, register before pending, recheck after registration,
  conserve every item/byte lease, and fuse after one terminal.
- Test zero/one/limit/+1 leases, context reorder/drop/duplicate, cancellation at every pending point,
  stale snapshot, wrong model/vector width, physical ID collision, payload type mismatch, array/nested
  filter traps, unindexed filters, missing shards, replica lag, duplicate ingestion, compaction, and
  local/remote differential results.
- Provision Qdrant integration tests explicitly under Nix with a pinned local service. Ordinary
  offline gates must not require network access or a globally installed daemon.
- Measure payload bytes, vector bytes, batch allocations/copies, filter index RAM, upload/query work,
  candidate count, exact rerank work, queue/lease high-water, first/warm latency, and adapter binary
  text. Reject a cache or retained collection that lacks a named lifetime and bound.

## Current primary sources

- Trustfall `Adapter` 0.8.1: https://docs.rs/trustfall/latest/trustfall/provider/trait.Adapter.html
- Qdrant points and IDs: https://qdrant.tech/documentation/concepts/points/
- Qdrant payload/filter semantics: https://qdrant.tech/documentation/concepts/payload/ and
  https://qdrant.tech/documentation/search/filtering/
- Qdrant consistency: https://qdrant.tech/documentation/scaling/consistency-guarantees/
- Qdrant sharding/multitenancy: https://qdrant.tech/documentation/manage-data/multitenancy/

Recheck these sources when dependency versions change. Do not turn a current backend limitation into
the portable logical protocol.

