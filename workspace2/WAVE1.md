# Historical Wave 1 ownership and integration contract

This file freezes the first implementation wave; it is not the current agent topology or roadmap.
The active capability order and manager/worker review cycle live in [`ROADMAP.md`](ROADMAP.md).
Wave 1 used three parallel ownership lanes. Agents could read everything but edited only their
ownership set; root manifests, architecture documents, skills, and another lane's crates remained
with the primary reviewer.

## Foundation fabric

Owns `nudox-id`, `nudox-schema`, `nudox-frame`, and `nudox-view`.

Deliver a `no_std` typed identity kernel and a small, canonical, length-delimited binary frame with
borrowed validation/views. The schema is intentionally finite in Wave 1; do not build a general IDL.

## Object hydration

Owns `nudox-object`, `nudox-root`, `nudox-store-memory`, and `nudox-hydration`.

Deliver copyable object descriptors, canonical sorted generation roots, an immutable in-memory
first-write-wins store, a separately generation-bound sparse locality map for
resident/promised/overlaid facts, readiness proof, and a pure demand-to-hydration plan. Semantic root
bytes never contain provider/tier placement. Do not add networking, async, filesystem, or a cache
framework.

## Operation runtime

Owns `nudox-operation`, `nudox-runtime`, `nudox-workflow`, `nudox-observe`, `nudox-e2e`, and the
server-only nested `adapters/observability` workspace.

Deliver a statically dispatched operation contract, bounded batch/credit runtime, and durable
idempotent workflow reducer. Integrate the other Wave 1 crates in end-to-end tests once their public
surfaces appear. Do not add Tokio, trait objects, boxed futures, or a generalized executor.

`nudox-observe` is the tiny no-std typed probe/flight-recorder seam. The OTEL SDK/exporter lives only
in `adapters/observability`, with its own workspace and dependency graph, so the portable client cannot
link it accidentally. The adapter must prove correlated traces/logs/metrics through in-memory OTEL
exporters before any network exporter is considered complete.

The Wave 1 in-memory source is a synchronous lending cursor because it never waits. Its operation
contract must compose with a later statically typed async I/O stream adapter without changing batch,
lease, terminal, provenance, cancellation, or backpressure semantics. Do not simulate async with a
`Pending` value that has no registered wake source.

## Cross-swath scenario

The final integration test must prove this flow:

1. Canonical object bytes and a generation root are built.
2. A request names a pinned generation and required object/projection.
3. The planner composes the pinned semantic root with its coherent locality map and reports a precise
   promise/missing set.
4. Bytes arrive in bounded frames and are structurally validated.
5. The immutable store admits the object once and rejects conflicting publication.
6. The root transitions atomically to ready only after closure verification.
7. A concrete local operation emits a bounded borrowed/result batch with complete provenance.
8. Cancellation or capacity exhaustion leaves no published half-state and returns all permits.
9. Replaying the workflow produces the same result without repeating committed work.
