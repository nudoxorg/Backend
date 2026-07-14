//! The `fmt` layer (journal / OTLP-fallback path) plus the level filter.
//!
//! `main.rs` previously noted the vendored `tracing-subscriber` build might
//! lack the `env-filter` feature. Checked against `build/third-party/registry.bzl`
//! while building this crate: the vendored `tracing-subscriber` (0.3.23,
//! label `tracing_subscriber-0_3`) *does* list `env-filter` among its
//! enabled features. So this module uses `tracing_subscriber::EnvFilter`
//! directly rather than the plain max-level fallback the plan sketches as
//! the degraded option — see the crate doc comment on `lib.rs` for the note
//! to carry forward if that ever regresses.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

use super::config::{LogFormat, TelemetryConfig};

/// Build the level filter from `NUDOX_LOG`/`RUST_LOG`. Uses
/// [`TelemetryConfig::log_level`] (already resolved from those two vars,
/// checked in that order) as the filter directive so behavior matches the
/// pre-existing `main.rs` `log_level()` helper exactly — this is a rename of
/// that call site, not a behavior change.
pub(crate) fn env_filter(config: &TelemetryConfig) -> EnvFilter {
	// `EnvFilter::new` takes a directive string; a bare level name is a valid
	// global directive. If `RUST_LOG`/`NUDOX_LOG` carried a full directive
	// string (e.g. "info,hyper=warn") `Level::from_str` above would have
	// failed to parse it and `log_level()` would have fallen back to `info`,
	// silently dropping the finer-grained directive. Prefer `EnvFilter`'s own
	// parser directly over the two raw env vars so multi-directive strings
	// keep working now that the feature is confirmed available; fall back to
	// the coarse level only if neither var is set or neither parses.
	let raw = ["NUDOX_LOG", "RUST_LOG"].iter().find_map(|name| std::env::var(name).ok());
	match raw {
		Some(directive) => EnvFilter::try_new(&directive)
			.unwrap_or_else(|_| EnvFilter::new(config.log_level.to_string())),
		None => EnvFilter::new(config.log_level.to_string()),
	}
}

/// The `fmt` layer: JSON (flattened event fields) when `NUDOX_LOG_FORMAT=json`
/// (the default), pretty-printed otherwise. This is the layer that keeps
/// writing to stdout → journal → (existing) Alloy `loki.source.journal`
/// fallback path regardless of whether the OTLP pipeline is up.
///
/// Returns a boxed layer so `init` can pick one of two concrete formatter
/// types without the caller needing to name either.
pub(crate) fn fmt_layer<S>(
	config: &TelemetryConfig,
) -> Box<dyn tracing_subscriber::Layer<S> + Send + Sync + 'static>
where
	S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
	match config.log_format {
		LogFormat::Json => Box::new(
			tracing_subscriber::fmt::layer()
				.json()
				.flatten_event(true)
				.with_span_events(FmtSpan::NONE)
				.with_target(true),
		),
		LogFormat::Pretty => Box::new(tracing_subscriber::fmt::layer().pretty().with_target(true)),
	}
}
