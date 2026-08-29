# Observability adapter manager cycle

## Frozen contract

| Item | Contract |
|---|---|
| Capability | A server-only, local-dispatch `TracingProbe` maps the Wave 1 public scenario to correlated spans, logs, and aggregate runtime gauges; trace/log export is bounded and nonblocking. |
| Observable consumer | A server composes `TracingProbe`, `dispatch`, bounded trace/log providers, and a periodic meter provider; the portable scenario remains unchanged. |
| Allowed writes | `adapters/observability/**` only. |
| Preserve | `Probe` remains typed and lazily built; all OTEL/tracing dependencies remain inside the nested adapter; the current 3 spans, 15 events, and 7 unlabeled gauges retain their exact semantic values; export is background/bounded; shutdown stays caller-owned. |
| Must prove | Exact span parentage and trace IDs; exact log/trace event correlation and fields; seven metric names, values, and no attributes; disabled interest does not run a builder; bounded overload drops rather than blocking producers; exporter failure is observable; force-flush then shutdown reaches both trace and logs; portable workspace dependency graph excludes adapter dependencies. |
| Resource law | No new dependency, public cross-core API, unsafe, SIMD, allocator, synchronous export, per-event application allocation, unbounded queue, or data-plane credit ownership. New production normally stays within 100 formatted net lines and leaves 25 lines reserve; deletion wins. |
| Explicit negative space | No global subscriber, transport/exporter protocol, retry policy, local flight recorder, new core event, source ID metric labels, OTLP integration, custom queue, or portability-core edits. |
| Final parent decision | Accept this adapter proof as the server-only observability seam, or authorize the smallest specifically named missing capability. |

## Evidence rubric

| Law | Artifact / command | Exact evidence | Falsifier / stop trigger |
|---|---|---|---|
| Typed lazy seam | public adapter-boundary test | A disabled `nudox.*` callsite leaves a builder-side cell false. | Delete the interest check: the test must fail. |
| Trace/log correlation | in-memory SDK integration | Exactly 3 spans and 15 logs; root/workflow parent the request; every log shares its active span trace ID and expected fields. | Reparent one event or alter one field. |
| Aggregate metrics | in-memory metric integration | Exactly 7 gauges, expected scalar values, and zero attributes. | Add an ID attribute or omit/alter a gauge. |
| Bounded overload | blocking exporter test | A full queue drops work while producer proceeds; retained/exported count is exact after release. | Replace batch processor with synchronous export or widen queue behavior. |
| Failure / ownership | failing exporter plus shutdown test | Export attempt is visible; trace/log/meter force flush precedes caller-owned shutdown; shutdown is observable. | Suppress exporter error or omit a provider shutdown. |
| Portable boundary | cargo metadata / dependency inspection | `tracing`, `opentelemetry*`, `tokio`, `tonic`, `http`, and `tls` do not enter portable core package dependency trees. | Add any such dependency to a portable crate. |
| Quality gate | nested fmt, test, pedantic clippy, doc | All succeed with no panic handling in adapter test reporters. | Any failing command or unreported source loss stops closure. |

## Decision table (pre-build)

| Question | Existing design | Alternative | Decision criterion |
|---|---|---|---|
| Export queue | SDK dedicated-thread bounded batch processors | Custom lock-free adapter queue | Retain SDK only if the tests establish capacity/drop/flush behavior; custom queue is forbidden without a measured missing law. |
| Semantic mapping | Static `Probe<Event>` impls and `tracing` callsites | Stringly generic event bridge | Retain typed mappings unless duplication leaves a current law unproved. |
| Metrics | Seven cached gauges, one scenario-end snapshot | Per-event `MetricsLayer` | Retain cached gauges; a hot metric event is rejected by the contract. |
| Lifecycle | Caller owns explicit flush/shutdown | Global installer/Drop lifecycle | Retain local dispatch and explicit ownership. |

## Cycle record

### Accepted evidence

| Law | Result |
|---|---|
| Lazy typed probe | `filtered_dispatch_does_not_build_root_probe_events` installs this adapter's `dispatch` with `nudox.root` disabled and observes no builder execution. |
| Correlation | The in-memory integration exports exactly three spans and fifteen logs. Every log has the exported request trace ID and its declared request/root/workflow span ID. |
| Values | Seven gauges are exact and unlabeled. Scenario fields use `Text` or `Count`; `tracing-opentelemetry` renders trace-event counts as decimal strings while the log bridge retains native OTLP integers. Both representations are asserted exactly. |
| Batching/overload | Independent span and log producers complete through a bounded channel before a blocked exporter is released; each then exports exactly two records and no batch exceeds one record with a one-record bound. |
| Lifecycle | The integration force-flushes trace, logs, and metrics before explicit provider shutdown; the in-memory trace and log exporters report shutdown. |
| Portable boundary | No portable crate manifest names `tracing`, `opentelemetry*`, `tokio`, `tonic`, `hyper`, `http`, or `rustls`; all adapter dependencies remain in this nested workspace. |

### Rejected scope

Caller-visible background-export failure accounting is not present. The SDK's background batch path may
contain an exporter error, but the current adapter offers no bounded caller-owned observation of it.
A correct private span/log wrapper plus a shared health handle was prototyped and deleted at 122 net
formatted production lines: it exceeded this cycle's 100-line ceiling and consumed its required
25-line reserve. This is the next separate adapter capability, not a passing claim in this cycle.

### Orchestration record

| Role | Turns | Result |
|---|---:|---|
| Read-only scout | 1 | Found the missing failure observation, missing log span-ID proof, absent log overload proof, and ambient-only laziness test. |
| Builder | 4 | Prototyped then deleted 122 production lines of health wrappers; retained 10 production lines for concrete dispatch interest and owning span scopes; strengthened tests. |
| Read-only breaker | 1 | Rejected the first candidate for synchronous producer evidence, non-adapter laziness, trace-ID-only correlation, stringly values, and absent health. |
| Builder repair | 2 | Added independent producer completion, actual adapter filtering, exact trace ID, and typed sink-aware values; deleted the misleading SDK-attempt test. |

### Observed process costs

- The initial health attempt was too large because it correctly had to forward both foreign exporter
  traits' resource, shutdown, and enablement behavior. A macro or omitted forwarding would have made
  the LOC target look better while weakening behavior, so the design was discarded.
- A sequential producer loop looked like a nonblocking test but could pass after exporter timeout.
  The breaker required a distinct producer completion channel before release.
- A shared string fixture hid a real SDK mapping difference. A closed semantic fixture now makes the
  trace-string/log-integer boundary visible rather than coercing both paths to text.
