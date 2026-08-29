---
name: build-operation-runtime
description: Scope rules for workspace2 operations, bounded lock-free runtimes, async streams, durable workflows, journals, and observability adapters. Use with deliver-reviewed-rust-slice for nudox-operation, runtime, workflow, observe, e2e, or approved durable/OTEL adapters.
---

# Operation and runtime scope

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first. It is authoritative for idioms,
review, lock-free/async/durable patterns, diagnostics, errors, unsafe, and tests. This skill adds only
operation-plane scope.

## Routing and ownership

Own operation/runtime/workflow/observe crates and parent-approved durable, transport, test-support,
and OTEL adapters. Ask before foundation/root/store changes. Read `ASYNC_STREAMING.md` for I/O and
`OBSERVABILITY.md` for diagnostic/export work.

## Required packet additions

Runtime work draws owners, capacities, atomic states, and linearization points. Durable work draws:

```text
untrusted event -> reduce -> canonical frame -> async append -> stable receipt -> released effect
```

State physical bounds at every pending/cancel/terminal phase; concrete future/stream and wake law;
durability definition; ordering/framing/checksum/torn-tail/retry/poison/shutdown; fault schedule; group
commit; diagnostic questions/events/correlation/flight trigger; adapter-only dependency boundary.

## Operation-plane invariants

- One permit owns one payload cell; work becomes terminal in place and credits return exactly once.
- Slots, bytes, waiters, terminals, queues, batches, and exporter buffers are separately bounded.
- Runtime/epoch identity prevents stale or cross-runtime handles.
- Core is concrete and `no_std` where intended; platform I/O/runtime/tracing stays in adapters.
- Replay retains no log and releases only the next idempotent effect.
- A real file test covers persistence/reopen/corruption; private fault seams cover write/sync/read;
  Loom covers the production atomic state machine; Miri covers initialized-slot reuse/drop.
- Top-level tests assert concurrency, zero/one/full/+1, every pending cancellation, disk/full/permission
  and sync failures, restart/replay prefix, exporter failure, recorder wrap, disabled-probe laziness,
  client dependency exclusion, and exact trace/span chronology.
- Diagnostic laziness tests install the adapter's real local dispatch and its production target
  policy, disable one exact target, and prove the typed builder was not called. An ambient/default or
  test-only subscriber does not exercise the adapter boundary.
- Export-overload tests block the actual exporter, require a separately scheduled producer to finish
  before release, and then prove exact retained/dropped batches and caller-owned shutdown. An SDK
  background error is not operationally observable until a public bounded health/lifecycle handle
  reports it; an exporter-local test counter cannot satisfy that contract.
- Adapter public journeys live in top-level `tests/`; `src/tests` is reserved for private unit laws.
  Correlation fixtures assert exact trace ID, owning span ID, semantic value type, event order, and
  zero unexpected attributes rather than merely counting signals.

## Closure

Return the shared handoff plus linearization/durability diagram, resource conservation, crash/fault
results, client dependency proof, and next parent decision. Never claim distributed completion.
