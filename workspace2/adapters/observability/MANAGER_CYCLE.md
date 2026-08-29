# Observability adapter manager cycle

## Root-review checkpoint contract

| Item | Contract |
|---|---|
| Baseline | Git `699b7cca71c78de8afebc1cee9e2a26a3ca46fdc`; every writable baseline file is digested in the manager handoff. |
| Capability | Make the already-shipping adapter journey an actual public integration journey, then delete redundant interest work and strengthen exact diagnostics without changing adapter semantics. |
| Observable consumer | A server composes bounded SDK providers, `dispatch`, and `TracingProbe`; public tests invoke only this adapter boundary. |
| Allowed paths | `adapters/observability/**` only. |
| Preserve | Static typed probes; the nested adapter dependency boundary; three spans, fifteen logs, seven unlabeled gauges; exact trace/span/value assertions; bounded queues; caller-owned lifecycle; no health wrapper. |
| Must prove | Public journey is in top-level `tests/`; one outer `Targets` policy is behaviorally equivalent to the former three uses; three-span lookup has no allocation and rejects missing/duplicate names; overload cleanup retains every primary/release/join failure deterministically; missing and unsupported log fields are distinct errors. |
| Budget | No dependency, unsafe, allocator, SIMD, exporter-health, OTLP, core API, or new production abstraction. Deletion is preferred; no production growth without an eliminated redundant operation. |
| Negative space | No custom queue, global subscriber, changed telemetry protocol, runtime/store/core edit, retry policy, or collector integration. |
| Parent decision | Accept one path-topology/diagnostic cleanup commit only if exact public tests and nested gates pass. |

### Root-review falsifiers

| Law | Artifact | Expected evidence | Stop trigger |
|---|---|---|---|
| Public boundary | top-level integration test | `src/` has only private unit law modules; full observable journey is compiled as an external consumer. | A test needs a private item or changes production visibility. |
| One interest policy | controlled disabled-target test | Outer-only `Targets` retains log/span routing and lazy builder behavior. | Trace or log events differ, or the builder runs. |
| Span lookup | exact integration assertion | No `BTreeMap`; each of the three expected names appears once, and missing/duplicate names return distinct typed errors. | A lookup allocates or a duplicate overwrites. |
| Cleanup | injected receive/release/join failure | Exact deterministic aggregate/priority preserves all coexisting causes. | `let _ =` or string conversion discards one. |
| Log fields | missing and malformed fixture paths | Absence reports `MissingLogField`; wrong representation reports `UnsupportedLogValue`. | Both defects share one error. |

## Frozen contract

| Item | Contract |
|---|---|
| Capability | A server-only, local-dispatch `TracingProbe` maps public semantic signals to correlated spans, logs, and aggregate runtime gauges; trace/log export is bounded and nonblocking. |
| Observable consumer | A server composes `TracingProbe`, `dispatch`, bounded trace/log providers, and a periodic meter provider without coupling the portable producers to the adapter. |
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
| Metrics | Seven cached gauges, one explicit aggregate snapshot | Per-event `MetricsLayer` | Retain cached gauges; a hot metric event is rejected by the contract. |
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

## Root-review cycle record

The Sol implementation worker committed
`eb3726898ac4c266263340a2cf6474f288cf641d` from contract commit
`3edfdc0c`. Its adapter-only churn is 176 additions and 58 deletions: production is two additions and
seven deletions (net -5), while relocated and strengthened tests are 174 additions and 51 deletions
(net +123). No manifest, lockfile, dependency, unsafe, allocator, core API, exporter-health, or OTLP
surface changed.

The outer-only `Targets` experiment passed. The exact public journey retained three spans, fifteen
logs, and seven unlabeled gauges after removing both per-layer filters and their clones; the same
external test binary proved a disabled `nudox.root` target does not execute its typed builder. Fixed
`Option<&SpanData>` slots now reject every missing expected span and every duplicate expected span
without a lookup allocation. Cleanup coordination retains receive, release, and join outcomes before
applying the documented receive → release → join → producer-send source priority. Missing log fields
and present fields with unsupported representations have distinct typed errors.

All closure commands passed from `adapters/observability`: `cargo fmt --all -- --check`, Nix-clang
`cargo test` (seven integration tests plus empty unit/doc suites), Nix-clang
`cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`,
`cargo doc --no-deps`, and `git diff --check`.

### Baseline file digests

SHA-256 digests at frozen baseline `699b7cca71c78de8afebc1cee9e2a26a3ca46fdc`
(Git adapter tree `2f9792d79b2d3c5f6002e2d2b886c5d3edb13c4b`):

| File | SHA-256 |
|---|---|
| `Cargo.lock` | `9b33daedcd90146fb609836097a9eae90ec4a4f382fc6d1b2e22e00f0530c4bf` |
| `Cargo.toml` | `c22651b3d1e6271a2ed244b3fdb2531fa2d90c6707d375f605da784b297da119` |
| `MANAGER_CYCLE.md` | `50a4eb8bddbfdacf944269ab872489f5524ecc4df885fce519ed72d2e564b4f8` |
| `src/batch.rs` | `ce182123eaab591f2e92da77cdd7ded0e59c8de26fd33b5485c323740188fc82` |
| `src/dispatch.rs` | `8e653e91c38581b8a6721d4737bcbb403fce9bf73fbbe69cc8ae7d443528747e` |
| `src/lib.rs` | `9b652daa296a99deaf54f61c9218e91c9e916a4bf81a15b6aba3bff6e91cdde3` |
| `src/metrics.rs` | `5bfd6db5d206d415332cc163633c917bbeb87814293cdeeaaf7d19383572630f` |
| `src/names.rs` | `d3c14b84f392866a8ac06d754d65dfaf97dc2f353f441513a084af7e106e7a5e` |
| `src/probe.rs` | `24e32f4e307862f23a41b63c54ddc4419dfa094f12ee6ef2759ce03bdfc829c8` |
| `src/tests.rs` | `dc204d32c3e7d96583a502c5254cb371ec65ce82dfe725bc8757fda63a57320c` |
| `src/tests/overload.rs` | `af6377def21104c3292fdfa9bcda182cedef1eb3c0108ca29d046131f5a54cff` |
| `src/tests/signal_contract.rs` | `41af972fb9fac7d04cb2c42a4f25c042221ab338e8038581fd8c304e773707a5` |
| `src/tests/support.rs` | `a671e89b87fc3a0f24fc59ecaf437992b445b8158ac356e1f9f7bb0bb8743b2e` |

### Implementation-candidate file digests

SHA-256 digests at accepted implementation `eb3726898ac4c266263340a2cf6474f288cf641d`
before this cycle-record-only commit (Git adapter tree
`cc1a435f9c762198a22571582807072cd016499b`):

| File | SHA-256 |
|---|---|
| `Cargo.lock` | `9b33daedcd90146fb609836097a9eae90ec4a4f382fc6d1b2e22e00f0530c4bf` |
| `Cargo.toml` | `c22651b3d1e6271a2ed244b3fdb2531fa2d90c6707d375f605da784b297da119` |
| `MANAGER_CYCLE.md` | `8e1105e4b635e9410fdd78aa4818d441341cf603307d6a00be8613ffbdc59df3` |
| `src/batch.rs` | `ce182123eaab591f2e92da77cdd7ded0e59c8de26fd33b5485c323740188fc82` |
| `src/dispatch.rs` | `b49d3262a56c859628c5d86df611fd038680593bf42e008623d5eaed8bef5d71` |
| `src/lib.rs` | `ceec5d312b671f4bfec34ab5cc30f394ecc8d1931acd75effad8bd7e90845ea9` |
| `src/metrics.rs` | `5bfd6db5d206d415332cc163633c917bbeb87814293cdeeaaf7d19383572630f` |
| `src/names.rs` | `d3c14b84f392866a8ac06d754d65dfaf97dc2f353f441513a084af7e106e7a5e` |
| `src/probe.rs` | `24e32f4e307862f23a41b63c54ddc4419dfa094f12ee6ef2759ce03bdfc829c8` |
| `tests/adapter.rs` | `fde8675d88b294854981df72c8d84e5492b93799dc3294b8cabad93599429f81` |
| `tests/adapter/overload.rs` | `189b55433e7de072e809d04e39ad9edb6649e04c0aa5d01b9608f8f38847a26a` |
| `tests/adapter/signal_contract.rs` | `1a9404dd19e36b64292567f894b06dbf4721adce96e348e27b5f80feb35cb0ce` |
| `tests/adapter/support.rs` | `840088599fdd4d3a90c9abe6fc516e73ae9009bb8e6b9d27acea484334b05841` |
