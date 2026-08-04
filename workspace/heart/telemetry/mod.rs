//! Unified telemetry: one `tracing_subscriber::Registry` wired to a JSON/pretty
//! `fmt` layer (journal fallback), an OTLP trace pipeline
//! (`tracing-opentelemetry`), an OTLP log pipeline
//! (`opentelemetry-appender-tracing`), an OTLP metrics `MeterProvider`
//! (alongside — not replacing — the existing prometheus `/metrics` fallback),
//! and an optional continuous-profiling Pyroscope agent.
//!
//! See `OBSERVABILITY-PLAN.md` (repo root) for the full design; this crate
//! implements Phase 1 (the crate itself) and the Rust half of Phase 5
//! (Pyroscope). Phase 6 (HTTP span coverage) lives in `workspace/server`,
//! consuming nothing from here beyond the propagator installed by [`init`].
//!
//! # Fail-open contract
//!
//! [`init`] never returns `Err` because a *collector* is unreachable — the
//! OTLP exporters are constructed once (which can fail on a malformed
//! endpoint/timeout config, a genuine programmer error) and then push
//! asynchronously in the background for the life of the process; a network
//! blip or a collector that never comes up shows up as background export
//! failures (logged by the SDK's own internal error channel, wired to
//! `tracing` below), never as a boot failure or a panic. `OTEL_SDK_DISABLED`
//! bypasses every exporter entirely — only the `fmt` layer is installed.
//!
//! # `env-filter` feature note
//!
//! `server::main` previously carried a comment that the vendored
//! `tracing-subscriber` build "carries no env-filter feature". Checked while
//! building this crate: `build/third-party/registry.bzl`'s `tracing-subscriber`
//! entry (0.3.23, label `tracing_subscriber-0_3`) already lists `env-filter`
//! among its vendored features. So `logging::env_filter` uses
//! [`tracing_subscriber::EnvFilter`] directly (parsing full directive strings,
//! not just a bare level name) rather than the level-name-only fallback the
//! plan sketches as the degraded option. If a future re-vendor ever drops
//! that feature, `logging::env_filter` is the one place to revert to
//! `tracing_subscriber::filter::LevelFilter::from_level(config.log_level)`.

mod config;
mod logging;
mod logs_otel;
mod metrics_bridge;
mod metrics_otel;
mod pyroscope;
mod resource;
mod tracing_otel;

pub use config::{LogFormat, Sampler, TelemetryConfig};
pub use metrics_bridge::render_prometheus;

use opentelemetry::global;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The live telemetry state. Holding this for the lifetime of `main` keeps
/// every provider (and the pyroscope agent) alive; dropping it flushes and
/// shuts each one down in turn (see [`Drop for TelemetryGuard`]).
///
/// Deliberately has no public fields/methods beyond `Drop` — callers are not
/// meant to reach into the providers; `tracing`/`metrics`/pyroscope call
/// sites are the only intended API surface once `init` has run.
pub struct TelemetryGuard {
	tracer_provider: Option<SdkTracerProvider>,
	meter_provider: Option<SdkMeterProvider>,
	logger_provider: Option<SdkLoggerProvider>,
	pyroscope: Option<pyroscope::PyroscopeHandle>,
}

impl Drop for TelemetryGuard {
	fn drop(&mut self) {
		// Best-effort, in reverse dependency order (profiling first — it does
		// not depend on the OTel SDK state at all — then logs/metrics/traces).
		// Every provider's `shutdown()` is fallible (network/collector down);
		// none of those failures propagate as a panic during unwind/shutdown.
		if let Some(pyroscope) = self.pyroscope.take() {
			pyroscope.stop();
		}
		if let Some(provider) = self.logger_provider.take()
			&& let Err(error) = provider.shutdown() {
				eprintln!("telemetry: logger provider shutdown failed: {error}");
			}
		if let Some(provider) = self.meter_provider.take()
			&& let Err(error) = provider.shutdown() {
				eprintln!("telemetry: meter provider shutdown failed: {error}");
			}
		if let Some(provider) = self.tracer_provider.take()
			&& let Err(error) = provider.shutdown() {
				eprintln!("telemetry: tracer provider shutdown failed: {error}");
			}
	}
}

/// Initialize the process-wide `tracing` subscriber and OTel pipelines.
///
/// Call once, immediately after configuration resolves (so `NUDOX_CONFIG`
/// overrides are available to feed into `TelemetryConfig` if a caller chooses
/// to route them there), and hold the returned guard for the life of `main`.
///
/// # `OTEL_SDK_DISABLED`
///
/// When `config.disabled` is set, this installs *only* the `fmt` layer
/// (level-filtered) — no exporters are constructed, no propagator is
/// installed beyond the no-op default, and pyroscope is skipped. This is the
/// exact pre-existing behavior of the bare
/// `tracing_subscriber::fmt().with_max_level(...).init()` call it replaces.
///
/// # Errors
///
/// Returns `Err` only for a genuine local misconfiguration at construction
/// time (e.g. an unparseable OTLP endpoint) — never because a collector is
/// unreachable over the network. See the module-level fail-open contract.
pub fn init(config: &TelemetryConfig) -> anyhow::Result<TelemetryGuard> {
	let filter = logging::env_filter(config);
	let fmt = logging::fmt_layer(config);

	if config.disabled {
		tracing_subscriber::registry().with(filter).with(fmt).init();
		// Even with the OTLP SDK bypassed, the `metrics` facade still needs a
		// recorder or every `counter!`/`gauge!` call site becomes a silent
		// no-op and `/metrics` answers 503. Install the Prometheus pull
		// recorder alone (no OTLP fan-out) so the scrape path keeps working.
		metrics_bridge::install(None);
		tracing::info!("telemetry: OTEL_SDK_DISABLED set; plain logging + prometheus /metrics only");
		return Ok(TelemetryGuard {
			tracer_provider: None,
			meter_provider: None,
			logger_provider: None,
			pyroscope: None,
		});
	}

	// W3C TraceContext propagation across the HTTP boundary (Traefik →
	// backend and any outbound calls this process makes) — installed
	// regardless of whether any individual OTLP pipeline below succeeds,
	// since propagation itself has no network dependency.
	global::set_text_map_propagator(TraceContextPropagator::new());

	let otel_resource = resource::build(config);

	// Each pipeline is built independently and degrades independently: a
	// failure building the trace exporter must not prevent metrics/logs from
	// coming up, and vice versa. This is the concrete shape of "fail open".
	let tracer_provider = match tracing_otel::build_provider(config, otel_resource.clone()) {
		Ok(provider) => Some(provider),
		Err(error) => {
			tracing::warn!(%error, "telemetry: OTLP trace pipeline unavailable; continuing without it");
			None
		}
	};
	let meter_provider = match metrics_otel::build_provider(config, otel_resource.clone()) {
		Ok(provider) => Some(provider),
		Err(error) => {
			tracing::warn!(%error, "telemetry: OTLP metrics pipeline unavailable; continuing without it");
			None
		}
	};
	let logger_provider = match logs_otel::build_provider(config, otel_resource) {
		Ok(provider) => Some(provider),
		Err(error) => {
			tracing::warn!(%error, "telemetry: OTLP log pipeline unavailable; continuing without it");
			None
		}
	};

	if let Some(provider) = &meter_provider {
		global::set_meter_provider(provider.clone());
	}

	// Install the process-wide `metrics`-crate recorder: a Prometheus+OTLP
	// fan-out when the OTLP meter pipeline is up (so `metrics::counter!` &c.
	// reach both `/metrics` and the collector), Prometheus-only otherwise. This
	// subsumes the old `server::http::handlers::health::install_prometheus` —
	// the recorder is now installed here, once, before `serve()` runs.
	let meter = meter_provider.as_ref().map(|provider| provider.meter("nudox-backend"));
	metrics_bridge::install(meter);

	// Assemble the registry. `tracing_subscriber::Layer` composition requires
	// static dispatch per layer type, but the OTel layers are optional
	// (Option<Layer> implements Layer as "no-op when None"), so this compiles
	// regardless of which pipelines came up.
	let otel_trace_layer =
		tracer_provider.as_ref().map(|provider| tracing_otel::layer(provider, &config.service_name));
	let otel_log_layer = logger_provider.as_ref().map(logs_otel::layer);

	tracing_subscriber::registry()
		.with(filter)
		.with(fmt)
		.with(otel_trace_layer)
		.with(otel_log_layer)
		.init();

	if tracer_provider.is_none() && meter_provider.is_none() && logger_provider.is_none() {
		tracing::warn!(
			endpoint = %config.otlp_endpoint,
			"telemetry: no OTLP pipeline could be constructed; running with fmt logging only"
		);
	} else {
		tracing::info!(
			endpoint = %config.otlp_endpoint,
			service = %config.service_name,
			environment = %config.environment,
			"telemetry initialized"
		);
	}

	let pyroscope_handle = pyroscope::PyroscopeHandle::start(config);

	Ok(TelemetryGuard {
		tracer_provider,
		meter_provider,
		logger_provider,
		pyroscope: Some(pyroscope_handle),
	})
}
