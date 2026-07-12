Observability — wire the backend end-to-end into the NixOS telemetry stack (OTLP everywhere)
==========================================================================================

Status: **PROPOSED** (2026-07-12). No code has been written. This document spans
two repositories:

- `Backend/` — the Rust workspace (Buck2-built `nudox` binary).
- `nixos/`   — the fleet config that runs the platform (`nudox.services.*`).

---

## 0. Diagnosis — why "is it even wired?" answers *no*

The platform side is nearly complete; the application side barely touches it, and
**OpenTelemetry is not used at all** (verified: no `opentelemetry*`,
`tracing-opentelemetry`, `tempo`, or `pyroscope` crate appears in any `BUCK`,
`Cargo.*`, or `.rs` file in `workspace/`).

What the platform can ingest (`nixos/profiles/observability.nix`):

| Signal    | Service                              | Receiver              | Today                         |
| --------- | ------------------------------------ | --------------------- | ----------------------------- |
| Metrics   | VictoriaMetrics + exporters          | scrape                | running; **backend unscraped**|
| Logs      | Loki                                 | push (via Alloy)      | running; journal only         |
| Traces    | Tempo                                | OTLP :4317 / :4318    | running; **receives nothing** |
| Profiling | Pyroscope                            | :4040                 | running; **receives nothing** |
| Alerting  | vmalert                              | —                     | running; no backend rules     |
| Dashboards| Grafana                              | —                     | only a Tempo dashboard        |

What the backend actually emits:

- **Logs**: `tracing_subscriber::fmt().with_max_level(...)` — plain text to stdout
  (`workspace/server/main.rs:24`). Reaches Loki only because Alloy scrapes the
  systemd journal (`nixos/system/services/alloy.nix`). Unstructured; no trace
  correlation.
- **Metrics**: a handful via the `metrics` crate (`outbox_intents_consumed`,
  `outbox_rows_reclaimed`, `cas_blobs_stored`, `text_index_watermark_lag`,
  `packages_ensure_initialized`, `indexing_progress_overall`), rendered at
  `GET /metrics` on :3001 (`workspace/server/http/handlers/health.rs`). **Nothing
  scrapes port 3001** — VictoriaMetrics has jobs for node/smartctl/systemd/
  postgres/blackbox/traefik/zitadel and no `backend` job
  (`nixos/system/services/victoriametrics.nix`). Emitted into the void.
- **Traces**: ~20 files carry `#[tracing::instrument]` spans, but they stay
  in-process. No exporter ⇒ Tempo never sees a span.
- **Profiling**: nothing wired to Pyroscope.
- **Backend systemd unit** (`nixos/system/services/backend.nix`): sets `NUDOX_*`
  env, **zero `OTEL_*`**.
- **Alloy** only runs `loki.source.journal`; no `otelcol` receiver.

Root cause: the app is instrumented for `tracing` + `metrics`, but the two ends
were never connected, and the trace/profile planes are dead weight on the
platform.

---

## 1. Target architecture — OTLP everywhere, Alloy as the collector

Decisions locked (2026-07-12):

1. **Full OTLP** in the Rust workspace: traces **and** metrics **and** logs
   exported over OTLP.
2. **Logs go direct over OTLP, with JSON-to-journal as the fallback** (see §5).
3. Deliverable now is this plan only.

```
                          ┌──────────────────────────── host (systemd) ───────────────────────────┐
  nudox backend (:3001)   │                                                                        │
  ┌───────────────────┐   │   ┌───────────────── Grafana Alloy (otelcol collector) ────────────┐   │
  │ tracing + otel    │   │   │  otelcol.receiver.otlp  (grpc :4317 / http :4318, 127.0.0.1)    │   │
  │ opentelemetry SDK ├───OTLP┤    → batch/resource/memory_limiter processors                    │   │
  │ metrics → otel    │   │   │      ├─ otelcol.exporter.otlp        → Tempo :4317   (traces)     │   │
  │ log bridge → otel │   │   │      ├─ otelcol.exporter.prometheus  → VictoriaMetrics remote-write│   │
  │ pyroscope agent   ├───────┼──────┴─ otelcol.exporter.loki        → Loki :3100    (logs)        │   │
  └─────────┬─────────┘   │   │  loki.source.journal (existing) ────→ Loki  (fallback / non-OTLP)  │   │
            │ pprof       │   └────────────────────────────────────────────────────────────────────┘   │
            └─────────────┼────────────────────────────────→ Pyroscope :4040 (profiles)               │
  /metrics (:3001)  ······│······ VictoriaMetrics scrape (belt-and-suspenders fallback, §4)            │
                          └────────────────────────────────────────────────────────────────────────────┘
```

Design principles:

- **Alloy is the single local collection point.** The backend never talks to
  Tempo/VM/Loki directly; it speaks OTLP to `127.0.0.1:4318` (http) only. This
  keeps the app's config to one endpoint and lets the platform re-route signals
  without a backend redeploy. Alloy already runs on every host via the profile.
- **One `Resource` describes the process.** `service.name=nudox-backend`,
  `service.version` (from the Buck2/cargo build), `service.instance.id`,
  `deployment.environment` (dev/prod), host + build-system attributes. Set once,
  attached to every span/metric/log so all three signals join in Grafana.
- **Config is env-driven and standard.** Honor the OTel spec env vars
  (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`,
  `OTEL_TRACES_SAMPLER`, `OTEL_SDK_DISABLED`) so behavior is tunable from the Nix
  unit with no recompile. `NUDOX_LOG`/`RUST_LOG` keep controlling verbosity.
- **Fail open, never fail the server.** If the collector is unreachable at boot
  or at runtime, telemetry init logs a warning and the server serves anyway —
  same contract the current prometheus recorder already honors
  (`install_prometheus` → 503, no panic). `OTEL_SDK_DISABLED=true` fully bypasses
  the SDK for tests and local runs.
- **Fallbacks stay live.** Journal→Loki and `/metrics` scrape remain wired as
  degraded-mode paths so a collector outage never blinds us.

---

## 2. Phase 1 — Rust: a `telemetry` module/crate (Backend/)

Goal: one `telemetry::init(&TelemetryConfig) -> TelemetryGuard` call that replaces
the bare `fmt()` line and installs traces + metrics + logs + a resource, returning
a guard whose `Drop` flushes and shuts down the providers.

Placement: a new workspace member `workspace/telemetry/` (peer of `util`,
`heart`). It must be dependency-light and usable by `server` and any future
binary. Wire it as a Buck2 `rust_crate` in `workspace/telemetry/BUCK` and add
`member("telemetry")` to `workspace/server/BUCK`.

New third-party crates (add via the repo's cargo-for-buck2 tooling —
`buck2 run //:add <crate>`, per `CARGO-BUCK-PLAN.md`; these must be vendored/pinned
into `build/third-party`):

- `opentelemetry` (API) + `opentelemetry_sdk` (with `rt-tokio`)
- `opentelemetry-otlp` (features: `grpc-tonic` **or** `http-proto`; pick HTTP to
  match the `:4318` endpoint and avoid a second transport stack)
- `opentelemetry-semantic-conventions`
- `tracing-opentelemetry` (bridges `tracing` spans → OTel spans)
- `opentelemetry-appender-tracing` (bridges `tracing` events → OTel logs)
- For metrics, choose one (decide in this phase, don't do both):
  - **(a)** keep `metrics` + `metrics_exporter_prometheus`, add the scrape job
    (§4) — smallest diff, but metrics travel a different path than traces/logs; or
  - **(b)** migrate call sites to `opentelemetry` metrics (or add
    `metrics-exporter-opentelemetry`) so all three signals share the OTLP pipeline
    and the same `Resource`. **Recommended (b)** for true "otel throughout"; (a)
    is the acceptable interim if call-site churn is a concern.

`TelemetryConfig` (resolved in `ServerConfiguration::resolve`, alongside the
existing `NUDOX_*` resolution in `workspace/server/config.rs`):

- `otlp_endpoint: Option<String>` (`OTEL_EXPORTER_OTLP_ENDPOINT`, default
  `http://127.0.0.1:4318`)
- `service_name` (default `nudox-backend`), `service_version` (compile-time
  `env!("CARGO_PKG_VERSION")` or a Buck-injected value), `environment`
- `sampler` (`OTEL_TRACES_SAMPLER`, default `parentbased_traceidratio`, ratio
  from `OTEL_TRACES_SAMPLER_ARG`; dev = 1.0, prod = e.g. 0.1)
- `log_format: Json | Pretty` (`NUDOX_LOG_FORMAT`, default `Json` in prod)
- `disabled: bool` (`OTEL_SDK_DISABLED`)

`telemetry::init` builds a single `tracing_subscriber::Registry` with layers:

1. `EnvFilter`-equivalent level from `NUDOX_LOG`/`RUST_LOG` (note: `main.rs:24`
   comment says the vendored `tracing-subscriber` carries no `env-filter` feature
   — either enable that feature when vendoring, or keep the existing level-name
   parse in `log_level()` and apply it as the max level).
2. `fmt` layer — **JSON** (`.json().flatten_event(true)`) when `log_format=Json`,
   pretty otherwise. This is the journal path and the OTLP-logs fallback.
3. `tracing_opentelemetry::layer()` with the OTLP-backed `TracerProvider`.
4. `OpenTelemetryTracingBridge` (log appender) with the OTLP-backed
   `LoggerProvider`.

`main.rs` changes: replace line 24 with
`let _guard = telemetry::init(&config.telemetry)?;` **after** `config` resolves
(so env is available), and hold `_guard` for the lifetime of `main`. Keep
`install_prometheus()` in `lib.rs:382` iff metrics path (a) is chosen; delete it
under path (b).

Acceptance for Phase 1: `cargo run`/`buck2 run` with `OTEL_SDK_DISABLED=true`
behaves exactly as today; with the SDK enabled and no collector, the server still
boots and serves (warning logged); with a local collector, a manual request
produces a span visible in Tempo.

---

## 3. Phase 2 — Alloy becomes the OTLP collector (nixos/)

Extend `nixos/system/services/alloy.nix`. It currently only ships the journal.
Add an `otelcol` pipeline (keep the journal block as the fallback log path).

New options on `nudox.services.alloy`:

- `otlpGrpcListen` (default `127.0.0.1:4317`), `otlpHttpListen`
  (default `127.0.0.1:4318`)
- `tempoEndpoint` (default from the tempo module's `otlpGrpcPort`, i.e.
  `127.0.0.1:4317` — mind the port collision with Alloy's own receiver; give
  Alloy `:4319`/`:4320` **or** point the receiver at distinct ports and export to
  Tempo's `:4317`. Resolve the exact port map in this phase.)
- `victoriametricsRemoteWrite` (default derived from the VM module)
- `pyroscopeEndpoint` (default `http://127.0.0.1:4040`)

River config to append (sketch — finalize against the Alloy version pinned in
nixpkgs):

```river
otelcol.receiver.otlp "backend" {
  grpc { endpoint = "127.0.0.1:4317" }
  http { endpoint = "127.0.0.1:4318" }
  output {
    traces  = [otelcol.processor.batch.default.input]
    metrics = [otelcol.processor.batch.default.input]
    logs    = [otelcol.processor.batch.default.input]
  }
}
otelcol.processor.memory_limiter "default" { ... check_interval = "1s" ... }
otelcol.processor.batch "default" {
  output {
    traces  = [otelcol.exporter.otlp.tempo.input]
    metrics = [otelcol.exporter.prometheus.vm.input]
    logs    = [otelcol.exporter.loki.local.input]
  }
}
otelcol.exporter.otlp "tempo" { client { endpoint = "127.0.0.1:4317" tls { insecure = true } } }
otelcol.exporter.prometheus "vm" { forward_to = [prometheus.remote_write.vm.receiver] }
prometheus.remote_write "vm" { endpoint { url = "http://127.0.0.1:8428/api/v1/write" } }
otelcol.exporter.loki "local" { forward_to = [loki.write.local.receiver] }
```

Add a `nudox.testing.checks` entry asserting the OTLP receiver port is open
(mirror the existing `alloy.service` unit check). Firewall stays closed —
everything is loopback.

---

## 4. Phase 3 — NixOS backend service env + metrics scrape fallback

Edit `nixos/system/services/backend.nix`:

- Add `OTEL_*` to `backendEnv`:
  - `OTEL_EXPORTER_OTLP_ENDPOINT = "http://127.0.0.1:4318"`
  - `OTEL_EXPORTER_OTLP_PROTOCOL = "http/protobuf"`
  - `OTEL_SERVICE_NAME = "nudox-backend"`
  - `OTEL_RESOURCE_ATTRIBUTES =
    "deployment.environment=${profile},service.version=${backend.version},host.name=${config.networking.hostName},nudox.build_system=${cfg.buildSystem}"`
  - `OTEL_TRACES_SAMPLER = "parentbased_traceidratio"`,
    `OTEL_TRACES_SAMPLER_ARG` per profile (dev `1.0`, prod `0.1`)
  - `NUDOX_LOG_FORMAT = "json"`
- New options: `nudox.services.backend.telemetry.{enable, samplingRatio,
  otlpEndpoint, logFormat}` so profiles can override without touching the unit.
- Ordering: add `after = [ "alloy.service" ]` and `wants` so the collector is up
  first (but the backend must still tolerate it being down — §1 fail-open).

Metrics scrape as belt-and-suspenders (independent of the OTLP metrics path):
in `nixos/system/services/victoriametrics.nix`, add a `scrapeBackend` option
(default `config.nudox.services.backend.enable`) and a `backend` scrape job:

```nix
{ job_name = "backend";
  static_configs = [{ targets = [ "127.0.0.1:${toString config.nudox.services.backend.port}" ]; }];
  metrics_path = "/metrics"; }
```

This guarantees the existing `metrics!` counters/gauges are collected even before
the OTLP metrics migration (path b) lands, and remains a fallback afterward.

---

## 5. Phase 4 — Logs: direct OTLP, JSON journal as fallback

Primary: the OTel log appender (Phase 1, layer 4) ships structured logs over OTLP
to Alloy → Loki. Each record carries the active `trace_id`/`span_id`, so Grafana's
Loki↔Tempo correlation ("logs for this trace") works.

Fallback: the `fmt().json()` layer keeps writing to stdout → journal. Alloy's
existing `loki.source.journal` still forwards it. Enhance that block to parse the
JSON payload and lift `level`, `target`, and `trace_id` into Loki labels via
`loki.process` stages, so journal-path logs remain queryable if OTLP is down.
Add a `service_name="nudox-backend"` label so both paths land under one stream
selector without duplication ambiguity (dedupe/label strategy to be confirmed so
the two paths don't double-count during normal operation — likely gate the
journal-forward of backend lines off when OTLP is healthy, or accept dev-only
duplication).

---

## 6. Phase 5 — Profiling to Pyroscope (nixos + Backend)

Continuous profiling of the `nudox` process:

- Add `pyroscope` + `pyroscope_pprofrs` crates to the Rust workspace; start the
  agent in `telemetry::init` when `PYROSCOPE_ADHOC_SERVER_ADDRESS` /
  `NUDOX_PYROSCOPE_ENDPOINT` is set (default `http://127.0.0.1:4040`), tagged with
  the same `service.name`/`environment`. Guard behind the `disabled` flag.
- Alternative if the crate's pinned-nixpkgs/build story is painful: Alloy's
  `pyroscope.scrape` against a pprof endpoint — but the Rust process would need to
  expose `/debug/pprof`, which it does not today. Prefer the in-process agent.
- Add the `NUDOX_PYROSCOPE_ENDPOINT` env to `backend.nix` and a
  `nudox.testing.checks` note. CPU profiling only at first; add alloc profiling
  later (interacts with the `mimalloc` global allocator in `main.rs` — validate).

---

## 7. Phase 6 — Span & metric coverage (semantic conventions)

Instrumentation exists but is thin and inconsistent. Once export works, raise
coverage where it pays:

- **HTTP layer**: add `tower-http`'s `TraceLayer` (or an OTel http middleware) in
  `workspace/server/http/router.rs` so every route gets a server span with
  `http.route`, `http.method`, `http.status_code`, and latency — automatically,
  instead of the current per-handler `#[instrument]` scattering.
- **Ingestion/compile pipeline**: ensure spans propagate across the async task
  boundaries in `poll.rs`, `coordination/indexing.rs`, and the compiler
  producers, so a package ingest is one connected trace, not orphaned spans.
- **Metrics naming**: align existing metric names to OTel/Prometheus conventions
  (units suffixes, `_total` for counters). Convert the ad-hoc names in `poll.rs`
  etc. Add RED metrics for the HTTP plane (rate/errors/duration) — mostly free
  from the TraceLayer.
- **Context propagation**: install the W3C `TraceContext` propagator and extract
  incoming `traceparent` at the proxy boundary so traces stitch across services
  (Traefik → backend).

---

## 8. Phase 7 — Dashboards & alerts (nixos/)

- **Grafana**: add backend dashboards under
  `nixos/system/services/grafana/dashboards/` (peer of the existing `tempo.nix`):
  a service-overview (RED for HTTP, ingest throughput, queue depth from
  `outbox_*`, CAS/index gauges) and a trace-explorer landing. Register in
  `dashboards/default.nix`.
- **Datasource correlation**: confirm the Tempo↔Loki↔VM datasources have the
  derived-field / trace-to-logs links configured so a span jumps to its logs.
- **vmalert**: add backend alert rules in `nixos/system/services/vmalert.nix` —
  backend down (scrape `up==0` or blackbox on `/readyz`), error-rate SLO,
  `text_index_watermark_lag` too high, outbox backlog growth, p99 latency.

---

## 9. Phase 8 — Verification & rollout

- **Unit/integration**: telemetry init must be a no-op under
  `OTEL_SDK_DISABLED=true` (assert in a `workspace/server/tests` case that the
  server boots and `/readyz` is 200 with the SDK disabled and enabled-but-no-
  collector).
- **NixOS VM test**: extend the `nudox.testing.checks` harness — assert Alloy's
  OTLP port is open, then drive one backend request and assert a trace lands in
  Tempo and a metric sample in VM (query their APIs from the test).
- **Rollout order** (each independently shippable, lowest blast radius first):
  1. §4 backend scrape job + §5 JSON journal logs (pure Nix + one Rust line;
     collects today's dead metrics immediately).
  2. §2 Alloy otelcol receiver (platform-only; inert until the app exports).
  3. §2/§3 Rust telemetry crate + `OTEL_*` env (traces + logs go live).
  4. §6 metrics-over-OTLP migration; retire the scrape job to fallback-only.
  5. §6 Pyroscope, §7 coverage, §8 dashboards/alerts.
- Keep `buildSystem = "cargo"` default throughout; verify parity under
  `buck2` separately (the OTel crates must vendor cleanly into `build/third-party`
  — this is the main Buck2 risk and should be de-risked first in Phase 1).

---

## Open decisions to close during implementation

1. OTLP transport: **http/protobuf** (single `:4318`) vs gRPC (`tonic`). Plan
   assumes HTTP.
2. Metrics: migrate call sites to OTel metrics (path b) vs keep prometheus +
   scrape (path a). Plan recommends (b), interim (a).
3. Port map for Alloy's OTLP receiver vs Tempo's OTLP receiver (both default
   4317/4318) — must not collide.
4. Log double-counting between the OTLP path and the journal fallback — dedupe
   strategy.
5. `tracing-subscriber` `env-filter` feature availability in the vendored build
   (`main.rs:24` says it's absent today).
