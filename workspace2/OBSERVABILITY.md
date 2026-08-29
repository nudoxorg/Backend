# Observability contract

Observability is an adapter, not ambient string formatting in the data plane. A lean local client can
compile the core with no exporter, allocator, socket, runtime, or OpenTelemetry dependency. A remote
service can attach complete traces, metrics, and logs without changing semantic code.

## Three layers

1. A tiny typed probe vocabulary records semantic transitions and aggregated work facts. Core owners
   are generic over the probe implementation. The unit probe `()` is zero-sized, monomorphized, and
   does not evaluate its lazy event builder.
2. An optional fixed-capacity local flight recorder stores compact copyable events in an owner-local or
   SPSC ring. It allocates once, overwrites or rejects according to explicit policy, and dumps only on
   error/user request. It never formats on record.
3. A server-only adapter maps typed probes to `tracing` callsites and OpenTelemetry. It lives in a
   separate workspace/package graph so the portable client does not link OTEL, Tokio, tonic, HTTP, or
   TLS accidentally.

## Signal design

- One request/root/workflow stage is a span. A stream does not create a span per row, byte chunk, or
  cache probe.
- Hot loops update owner/shard-local counters and histograms. A periodic snapshot emits aggregate
  metrics. The current `tracing-opentelemetry` metrics layer performs a map lookup per emission, so it
  is not the hot counter path:
  https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/struct.MetricsLayer.html
- OpenTelemetry Rust 0.32 adds bound counters/histograms that resolve and cache their attribute-to-
  aggregator mapping once. The server adapter may bind one low-cardinality instrument set per owner or
  shard and publish periodic aggregates through those handles; it still does not emit a metric event
  per row, chunk, or cache probe:
  https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md
- Typed transition events use static names and fixed scalar fields. Formatting and source-chain
  rendering occur only in an enabled subscriber/exporter.
- Trace context is propagated once in the request/control envelope using the W3C context model. Data
  frames inherit that request context; they do not repeat baggage or string metadata.
- Content, generation, tenant, path, and request IDs are trace fields only when sampled. They are
  forbidden metric labels because cardinality is unbounded.
- Every exporter reports its own dropped events, queue high-water, export failures, batch sizes, and
  flush latency without recursively tracing the exporter.

## Cost and overload

`tracing` callsites cache subscriber interest, and release builds can remove disabled levels at compile
time. Prefer static `always`/`never` filtering; dynamic per-event filters force an enabled check on every
call. Sources:

- https://docs.rs/tracing-core/latest/tracing_core/callsite/index.html
- https://docs.rs/tracing/latest/src/tracing/level_filters.rs.html

OTLP uses bounded batch processors, never synchronous network export from a request task. The official
Rust example recommends batched traces/logs and periodic metric export:
https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/examples/basic-otlp/README.md

The OTEL logs design documents bounded batches, heap copying for OTLP, and drop-on-overload. Therefore
the application must choose and expose queue/batch/drop policy; telemetry cannot backpressure semantic
work or consume its byte credits:
https://github.com/open-telemetry/opentelemetry-rust/blob/main/docs/design/logs.md

## Events as executable documentation

A transition event names the reason and before/after states, so the trace reads as a durable execution
narrative:

```text
HydrationPlanned { root, present, promised, missing, fetch_bytes }
CreditReserved { class, requested, remaining }
WorkTransition { handle, from, event, to }
PublicationCommitted { root, dependency_set }
StreamTerminal { delivered, missing, disposition }
```

This replaces comments that merely narrate control flow, not public invariant documentation, safety
arguments, or the reason for a surprising choice. Event types must be tested like protocol types and
must not duplicate authoritative state.

## Integration contract harness

A contract driver inside an ordinary crate's `tests/` tree, assembled from small public-API
components, accepts a generic probe and deterministic fault schedule. Never create a dedicated
test-support crate. Concise `rstest` cases choose inputs and compare typed evidence; a driver may
execute in four modes without forking semantics:

- assertions: exact result, transition sequence, conservation, and terminal state;
- benchmark: unit probe `()`, fixed fixtures, wall time plus allocations/work counters;
- traced simulation: flight recorder or OTEL in-memory exporters, virtual time, reorder/loss/outage;
- fuzz/model: generated valid command/fault sequences with persistent replay corpus.

Use Divan for portable wall-time/throughput/allocation development benchmarks and iai-callgrind for
stable CI instruction/cache regression budgets where Valgrind is available:

- https://docs.rs/divan/latest/divan/
- https://docs.rs/iai-callgrind/latest/iai_callgrind/

## Required proof

- the unit probe `()` does not construct event fields and adds no retained state;
- disabled tracing evaluates no dynamic field expression and formats nothing;
- flight-recorder capacity, wrap/drop policy, multi-reader ownership, and crash dump are exact;
- OTEL in-memory exporters receive correlated traces/logs/metrics with correct parentage and attributes;
- OTLP export is batched, bounded, shutdown-flushed, and loss-accounted under collector outage;
- telemetry saturation cannot consume runtime/data credits or block publication;
- integration and benchmark cases in the same ordinary crate share driver and fixture constructors;
- cardinality audit rejects object/generation/request identifiers as metric attributes.

The executable adapter in `adapters/observability` proves the current seam with in-memory SDK
exporters: three correlated spans, the exact 15-event signal fixture, seven unlabeled aggregate gauges,
disabled-callsite laziness, bounded-queue overload, export-failure accounting, and shutdown flushing.
The adapter is a nested workspace, so none of its tracing, SDK, exporter, or async dependencies enter
the portable crate graph.
