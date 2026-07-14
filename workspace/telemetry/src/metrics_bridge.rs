//! Bridge the `metrics`-crate facade onto **both** the Prometheus pull exporter
//! (`/metrics`, scraped directly by VictoriaMetrics on the backend port) and the
//! OTLP `MeterProvider` (pushed to the Alloy collector). This is the concrete
//! "path (b)" from `OBSERVABILITY-PLAN.md`: every existing `metrics::counter!` /
//! `metrics::gauge!` / `metrics::histogram!` call site — the outbox/CAS/index
//! gauges *and* the HTTP RED metrics added in `server::http::router` — now fans
//! out to the two sinks through a single installed global recorder.
//!
//! # Why a hand-written fan-out (no new dependency)
//!
//! `metrics-util` (already vendored, label `metrics_util-0_19`) provides
//! [`FanoutBuilder`], and `metrics-exporter-prometheus` (label
//! `metrics_exporter_prometheus-0_16`) provides the pull recorder. The only
//! piece missing is a `metrics::Recorder` that forwards into OpenTelemetry
//! instruments — [`OtelRecorder`] below — so no crates.io-blocked crate is
//! needed here.
//!
//! # `_total` / `_bucket` suffixing (the naming contract with the dashboards)
//!
//! `metrics-exporter-prometheus` appends `_total` to counters in its exposition
//! (this is why the pre-existing `counter!("outbox_intents_consumed")` is queried
//! as `outbox_intents_consumed_total` in the Grafana dashboards). So the HTTP
//! counter is registered as `http_requests` and *renders* as `http_requests_total`.
//! Histograms render as `<name>_bucket`/`_sum`/`_count` — **but only when the
//! recorder is given explicit buckets**; the default is a summary (quantiles),
//! which `histogram_quantile(..., ..._bucket)` cannot read. [`install`] therefore
//! sets latency buckets for every `_seconds` histogram, which is what makes the
//! `http_request_duration_seconds_bucket` alert/panel queries resolve.
//!
//! # Version sensitivity
//!
//! The OpenTelemetry synchronous instrument API used here (`f64_gauge`,
//! `u64_counter`, `f64_histogram`, `.build()`, `Gauge::record`) tracks the same
//! opentelemetry version the rest of this crate targets. If a re-vendor lands a
//! version where the sync `Gauge` or the `.build()` instrument constructors
//! differ, this file and `tracing_otel.rs`/`metrics_otel.rs` move together.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use metrics::{
	Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder,
	SharedString, Unit,
};
use metrics_exporter_prometheus::{Matcher, PrometheusHandle};
use metrics_util::layers::FanoutBuilder;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

/// Latency histogram buckets (seconds) applied to every `_seconds` histogram so
/// the exposition carries `_bucket` series. Standard Prometheus web-latency
/// spread from 5ms to 10s.
const LATENCY_BUCKETS_SECONDS: &[f64] =
	&[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

/// The process-wide Prometheus handle `/metrics` renders from. Populated only
/// when [`install`] successfully installs its recorder as *the* global recorder;
/// left empty (so `/metrics` answers `503`) if some other recorder won the race
/// — the exact idempotent contract the previous `health::install_prometheus`
/// carried.
static PROM_HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Render the current Prometheus exposition, if the recorder is installed.
/// `server::http::handlers::health::metrics` calls this to serve `/metrics`.
pub fn render_prometheus() -> Option<String> {
	PROM_HANDLE.get().map(PrometheusHandle::render)
}

/// Install the global `metrics` recorder: Prometheus alone when `meter` is
/// `None` (SDK disabled, or the OTLP meter pipeline failed to build), or a
/// Prometheus+OTLP fan-out when a meter is available. Idempotent and fail-open —
/// a second call is a no-op, and losing the `set_global_recorder` race only
/// warns (never panics), leaving `/metrics` at `503`.
pub(crate) fn install(meter: Option<Meter>) {
	if PROM_HANDLE.get().is_some() {
		return;
	}

	let builder = match metrics_exporter_prometheus::PrometheusBuilder::new()
		.set_buckets_for_metric(Matcher::Suffix("_seconds".to_owned()), LATENCY_BUCKETS_SECONDS)
	{
		Ok(builder) => builder,
		Err(error) => {
			// A malformed bucket set is a programmer error, not a runtime one;
			// fall back to a bucket-less recorder rather than losing /metrics
			// entirely (latency histograms degrade to summaries, everything
			// else is unaffected).
			tracing::warn!(%error, "telemetry: latency buckets rejected; /metrics histograms will be summaries");
			metrics_exporter_prometheus::PrometheusBuilder::new()
		}
	};

	let prometheus = builder.build_recorder();
	let handle = prometheus.handle();

	// Two install shapes, kept monomorphic (no `Box<dyn Recorder>`) so the
	// `set_global_recorder` Send+Sync+'static bound is satisfied directly.
	let installed = match meter {
		Some(meter) => {
			let fanout = FanoutBuilder::default()
				.add_recorder(prometheus)
				.add_recorder(OtelRecorder::new(meter))
				.build();
			metrics::set_global_recorder(fanout).map_err(|error| error.to_string())
		}
		None => metrics::set_global_recorder(prometheus).map_err(|error| error.to_string()),
	};

	match installed {
		Ok(()) => {
			let _ = PROM_HANDLE.set(handle);
		}
		Err(error) => {
			tracing::warn!(%error, "telemetry: a metrics recorder was already installed; /metrics will answer 503");
		}
	}
}

/// A `metrics::Recorder` that forwards every counter/gauge/histogram operation
/// into OpenTelemetry instruments. Instruments are cached by metric *name*
/// (creating two OTel instruments with the same name triggers duplicate-
/// instrument warnings and split streams), and the `metrics` key's labels are
/// captured once per registration as OTel attributes.
struct OtelRecorder {
	meter: Meter,
	counters: Mutex<HashMap<String, opentelemetry::metrics::Counter<u64>>>,
	gauges: Mutex<HashMap<String, opentelemetry::metrics::Gauge<f64>>>,
	histograms: Mutex<HashMap<String, opentelemetry::metrics::Histogram<f64>>>,
}

impl OtelRecorder {
	fn new(meter: Meter) -> Self {
		Self {
			meter,
			counters: Mutex::new(HashMap::new()),
			gauges: Mutex::new(HashMap::new()),
			histograms: Mutex::new(HashMap::new()),
		}
	}

	fn counter(&self, name: &str) -> opentelemetry::metrics::Counter<u64> {
		self.counters
			.lock()
			.expect("otel counter cache poisoned")
			.entry(name.to_owned())
			.or_insert_with(|| self.meter.u64_counter(name.to_owned()).build())
			.clone()
	}

	fn gauge(&self, name: &str) -> opentelemetry::metrics::Gauge<f64> {
		self.gauges
			.lock()
			.expect("otel gauge cache poisoned")
			.entry(name.to_owned())
			.or_insert_with(|| self.meter.f64_gauge(name.to_owned()).build())
			.clone()
	}

	fn histogram(&self, name: &str) -> opentelemetry::metrics::Histogram<f64> {
		self.histograms
			.lock()
			.expect("otel histogram cache poisoned")
			.entry(name.to_owned())
			.or_insert_with(|| self.meter.f64_histogram(name.to_owned()).build())
			.clone()
	}
}

/// Snapshot a `metrics` key's labels as OTel attributes (done once at
/// registration; the returned handles carry the frozen set).
fn attributes(key: &Key) -> Vec<KeyValue> {
	key.labels()
		.map(|label| KeyValue::new(label.key().to_owned(), label.value().to_owned()))
		.collect()
}

impl Recorder for OtelRecorder {
	// Descriptions/units are Prometheus-owned in this deployment; the OTel side
	// takes the instrument name and attributes only. No-op the describe hooks.
	fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
	fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
	fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

	fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
		Counter::from_arc(Arc::new(OtelCounter {
			instrument: self.counter(key.name()),
			attributes: attributes(key),
			last_absolute: AtomicU64::new(0),
		}))
	}

	fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
		Gauge::from_arc(Arc::new(OtelGauge {
			instrument: self.gauge(key.name()),
			attributes: attributes(key),
			current_bits: AtomicU64::new(0.0f64.to_bits()),
		}))
	}

	fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
		Histogram::from_arc(Arc::new(OtelHistogram {
			instrument: self.histogram(key.name()),
			attributes: attributes(key),
		}))
	}
}

/// Counter handle. OTel counters are monotonic-`add` only, so `absolute` (rare
/// among the call sites — the outbox/CAS metrics use `increment`) is emulated by
/// tracking the last set value and adding the positive delta.
struct OtelCounter {
	instrument: opentelemetry::metrics::Counter<u64>,
	attributes: Vec<KeyValue>,
	last_absolute: AtomicU64,
}

impl CounterFn for OtelCounter {
	fn increment(&self, value: u64) {
		self.instrument.add(value, &self.attributes);
	}

	fn absolute(&self, value: u64) {
		let previous = self.last_absolute.swap(value, Ordering::Relaxed);
		if let Some(delta) = value.checked_sub(previous)
			&& delta > 0 {
				self.instrument.add(delta, &self.attributes);
			}
	}
}

/// Gauge handle. The `metrics` facade offers `set`/`increment`/`decrement` over
/// an absolute value; OTel's synchronous `Gauge` records the absolute, so the
/// current value is kept in an atomic (f64 bit pattern) to service inc/dec.
struct OtelGauge {
	instrument: opentelemetry::metrics::Gauge<f64>,
	attributes: Vec<KeyValue>,
	current_bits: AtomicU64,
}

impl OtelGauge {
	fn store_and_record(&self, value: f64) {
		self.current_bits.store(value.to_bits(), Ordering::Relaxed);
		self.instrument.record(value, &self.attributes);
	}
}

impl GaugeFn for OtelGauge {
	fn increment(&self, value: f64) {
		let current = f64::from_bits(self.current_bits.load(Ordering::Relaxed));
		self.store_and_record(current + value);
	}

	fn decrement(&self, value: f64) {
		let current = f64::from_bits(self.current_bits.load(Ordering::Relaxed));
		self.store_and_record(current - value);
	}

	fn set(&self, value: f64) {
		self.store_and_record(value);
	}
}

/// Histogram handle — a direct pass-through to the OTel histogram.
struct OtelHistogram {
	instrument: opentelemetry::metrics::Histogram<f64>,
	attributes: Vec<KeyValue>,
}

impl HistogramFn for OtelHistogram {
	fn record(&self, value: f64) {
		self.instrument.record(value, &self.attributes);
	}
}
