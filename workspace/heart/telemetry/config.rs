//! [`TelemetryConfig`]: resolves the OTel-standard env vars plus the small set
//! of `NUDOX_*` extensions, per the shared contract with the platform side
//! (see `OBSERVABILITY-PLAN.md` §1-2).
//!
//! Kept as plain `std::env::var` reads (mirroring the style already used for
//! `NUDOX_LOG`/`RUST_LOG` in `server::main` and the `NUDOX_*` folding in
//! `server::config::environment_fragment`) rather than threading through
//! `figment` — telemetry must be resolvable *before* the rest of config so a
//! failure in the main config layer can still be logged/traced.

use std::time::Duration;

/// Structured log output format (`NUDOX_LOG_FORMAT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// `tracing_subscriber`'s JSON formatter, event fields flattened to the
    /// top level (`flatten_event(true)`) — the journal/OTLP-fallback path.
    Json,
    /// Human-readable pretty formatting — local development.
    Pretty,
}

impl LogFormat {
    fn from_raw(raw: Option<&str>) -> Self {
        match raw {
            Some(raw) if raw.eq_ignore_ascii_case("pretty") => LogFormat::Pretty,
            // Default is Json, matching the platform's journal/Loki fallback path.
            _ => LogFormat::Json,
        }
    }
}

/// The OTel traces sampler (`OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG`).
///
/// Only the subset the shared contract names is modeled; anything else falls
/// back to the parent-based ratio sampler at the given (or default) ratio,
/// which is the documented default in the plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sampler {
    /// `parentbased_traceidratio` — respect an existing parent's sampling
    /// decision, otherwise sample by ratio.
    ParentBasedTraceIdRatio(f64),
    /// `always_on` — sample every trace.
    AlwaysOn,
    /// `always_off` — sample no trace.
    AlwaysOff,
}

impl Sampler {
    fn from_raw(sampler: Option<&str>, sampler_arg: Option<&str>) -> Self {
        let ratio = sampler_arg
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        match sampler {
            Some(raw) if raw.eq_ignore_ascii_case("always_on") => Sampler::AlwaysOn,
            Some(raw) if raw.eq_ignore_ascii_case("always_off") => Sampler::AlwaysOff,
            // Default (including unset): the shared contract's default sampler.
            _ => Sampler::ParentBasedTraceIdRatio(ratio),
        }
    }
}

/// The fully-resolved telemetry configuration, read once at boot from the
/// OTel-standard env vars plus the small `NUDOX_*`/env extensions the shared
/// contract defines. See `OBSERVABILITY-PLAN.md` §1 "SHARED CONTRACT".
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// `OTEL_SDK_DISABLED`. When `true`, [`crate::init`] installs plain
    /// `tracing_subscriber` logging only — no exporter, no pyroscope agent.
    pub disabled: bool,

    /// `OTEL_EXPORTER_OTLP_ENDPOINT`, default `http://127.0.0.1:4318`. All
    /// three signals (traces/metrics/logs) share this one http/protobuf
    /// endpoint — the shared contract fixes the transport, not just the
    /// default address.
    pub otlp_endpoint: String,

    /// `OTEL_SERVICE_NAME`, default `nudox-backend`.
    pub service_name: String,

    /// The build's version, attached as the `service.version` resource
    /// attribute. Defaults to `CARGO_PKG_VERSION` of the *server* crate,
    /// passed in by the caller (Buck2 has no direct `CARGO_PKG_VERSION`
    /// equivalent for a non-cargo build, so this is a constructor argument,
    /// not an env lookup here).
    pub service_version: String,

    /// `deployment.environment` resource attribute — folded in from
    /// `OTEL_RESOURCE_ATTRIBUTES` if present there, otherwise from
    /// `NUDOX_DEPLOYMENT` (`development`/`production`), defaulting to
    /// `development`.
    pub environment: String,

    /// Extra resource attributes parsed from `OTEL_RESOURCE_ATTRIBUTES`
    /// (`key=value,key=value` — the OTel spec's baggage-style encoding).
    /// Already includes `service.version`/`deployment.environment`/
    /// `host.name`/`nudox.build_system` if the caller set them there; entries
    /// here are layered on top of (never replacing) the typed fields above.
    pub resource_attributes: Vec<(String, String)>,

    /// The sampler for the traces pipeline.
    pub sampler: Sampler,

    /// `NUDOX_LOG` / `RUST_LOG` (checked in that order) as a level name,
    /// default `info` — the existing behavior, unchanged.
    pub log_level: tracing::Level,

    /// `NUDOX_LOG_FORMAT`, default `Json`.
    pub log_format: LogFormat,

    /// `NUDOX_PYROSCOPE_ENDPOINT`, default `http://127.0.0.1:4040`. Pyroscope
    /// is started whenever this is set (which it always is, by default) *and*
    /// the SDK is not disabled — matching the plan's "guarded by
    /// OTEL_SDK_DISABLED" instruction.
    pub pyroscope_endpoint: Option<String>,

    /// Export/shutdown timeout applied to every OTLP pipeline's batch
    /// exporter and to the guard's flush-on-drop. Not part of the shared env
    /// contract; a conservative internal default keeps a slow/unreachable
    /// collector from blocking shutdown indefinitely.
    pub export_timeout: Duration,
}

impl TelemetryConfig {
    /// Resolve from the environment. `service_version` and `build_system` are
    /// passed in explicitly (compile-time / build-system values, not env
    /// vars) so the caller controls how they're sourced; everything else
    /// follows the shared OTel/`NUDOX_*` contract.
    pub fn resolve(service_version: impl Into<String>, build_system: &str) -> Self {
        Self::resolve_with(service_version, build_system, |name| {
            std::env::var(name).ok()
        })
    }

    /// Resolve using an injected environment lookup. Keeping the environment
    /// boundary here makes the parsing and precedence rules deterministic to
    /// test without mutating process-global environment variables.
    fn resolve_with(
        service_version: impl Into<String>,
        build_system: &str,
        env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let disabled = env("OTEL_SDK_DISABLED")
            .is_some_and(|raw| raw.eq_ignore_ascii_case("true") || raw == "1");

        let otlp_endpoint = env("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|| "http://127.0.0.1:4318".to_owned());

        let service_name = env("OTEL_SERVICE_NAME").unwrap_or_else(|| "nudox-backend".to_owned());

        let mut resource_attributes =
            parse_resource_attributes(env("OTEL_RESOURCE_ATTRIBUTES").as_deref());

        let environment = resource_attributes
            .iter()
            .find(|(k, _)| k == "deployment.environment")
            .map(|(_, v)| v.clone())
            .or_else(|| env("NUDOX_DEPLOYMENT"))
            .unwrap_or_else(|| "development".to_owned());

        if !resource_attributes.iter().any(|(k, _)| k == "host.name")
            && let Some(host) = hostname_best_effort()
        {
            resource_attributes.push(("host.name".to_owned(), host));
        }

        if !resource_attributes
            .iter()
            .any(|(k, _)| k == "nudox.build_system")
        {
            resource_attributes.push(("nudox.build_system".to_owned(), build_system.to_owned()));
        }

        let pyroscope_endpoint = env("NUDOX_PYROSCOPE_ENDPOINT")
            .filter(|s| !s.is_empty())
            .or_else(|| Some("http://127.0.0.1:4040".to_owned()));

        Self {
            disabled,
            otlp_endpoint,
            service_name,
            service_version: service_version.into(),
            environment,
            resource_attributes,
            sampler: Sampler::from_raw(
                env("OTEL_TRACES_SAMPLER").as_deref(),
                env("OTEL_TRACES_SAMPLER_ARG").as_deref(),
            ),
            log_level: log_level_from(env("NUDOX_LOG").as_deref(), env("RUST_LOG").as_deref()),
            log_format: LogFormat::from_raw(env("NUDOX_LOG_FORMAT").as_deref()),
            pyroscope_endpoint,
            export_timeout: Duration::from_secs(5),
        }
    }
}

/// The maximum tracing level: `NUDOX_LOG` (then `RUST_LOG`) as a level name,
/// defaulting to `info`. Mirrors the pre-existing `server::main::log_level`
/// so behavior is unchanged for callers that only set one of the two vars.
fn log_level_from(nudox_log: Option<&str>, rust_log: Option<&str>) -> tracing::Level {
    [nudox_log, rust_log]
        .into_iter()
        .flatten()
        .find_map(|raw| raw.parse().ok())
        .unwrap_or(tracing::Level::INFO)
}

/// Parse `OTEL_RESOURCE_ATTRIBUTES` (`key=value,key=value`, percent-decoding
/// intentionally not applied — the values we set are all plain tokens).
fn parse_resource_attributes(raw: Option<&str>) -> Vec<(String, String)> {
    raw.map(|raw| {
        raw.split(',')
            .filter_map(|pair| {
                let (k, v) = pair.split_once('=')?;
                let (k, v) = (k.trim(), v.trim());
                (!k.is_empty()).then(|| (k.to_owned(), v.to_owned()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Best-effort local hostname for the `host.name` resource attribute. `None`
/// (attribute simply omitted) rather than an error — telemetry must never
/// block boot on something this soft.
fn hostname_best_effort() -> Option<String> {
    #[cfg(unix)]
    {
        // Avoid pulling in a `hostname` crate for one syscall: shell out to the
        // env var systemd/most shells export, falling back to `uname -n` only
        // if that's unset (kept dependency-free per the plan's "dependency
        // light" placement note).
        if let Ok(host) = std::env::var("HOSTNAME")
            && !host.is_empty()
        {
            return Some(host);
        }
        std::process::Command::new("uname")
            .arg("-n")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::{LogFormat, Sampler, TelemetryConfig, parse_resource_attributes};

    #[test]
    fn resource_attributes_parser_trims_and_skips_malformed_pairs() {
        assert_eq!(
            parse_resource_attributes(Some(
                " service.name = raw-service, deployment.environment=prod, malformed, =missing-key, empty="
            )),
            vec![
                ("service.name".to_owned(), "raw-service".to_owned()),
                ("deployment.environment".to_owned(), "prod".to_owned()),
                ("empty".to_owned(), "".to_owned()),
            ]
        );
    }

    #[test]
    fn resolve_uses_typed_fields_and_parses_all_telemetry_values() {
        let values = [
			("OTEL_SDK_DISABLED", "true"),
			("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318/"),
			("OTEL_SERVICE_NAME", "typed-service"),
			(
				"OTEL_RESOURCE_ATTRIBUTES",
				"service.name=raw-service,service.version=raw-version,deployment.environment=production,host.name=test-host,team=platform",
			),
			("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
			("OTEL_TRACES_SAMPLER_ARG", "2.0"),
			("NUDOX_LOG", "debug"),
			("NUDOX_LOG_FORMAT", "pretty"),
			("NUDOX_PYROSCOPE_ENDPOINT", ""),
		]
		.into_iter()
		.collect::<std::collections::HashMap<_, _>>();
        let config = TelemetryConfig::resolve_with("typed-version", "cargo", |name| {
            values.get(name).map(|value| (*value).to_owned())
        });

        assert!(config.disabled);
        assert_eq!(config.otlp_endpoint, "http://collector:4318/");
        assert_eq!(config.service_name, "typed-service");
        assert_eq!(config.service_version, "typed-version");
        assert_eq!(config.environment, "production");
        assert_eq!(config.log_level, tracing::Level::DEBUG);
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert_eq!(
            config.pyroscope_endpoint,
            Some("http://127.0.0.1:4040".to_owned())
        );
        assert_eq!(config.sampler, Sampler::ParentBasedTraceIdRatio(1.0));
        assert!(
            config
                .resource_attributes
                .contains(&("host.name".to_owned(), "test-host".to_owned()))
        );
        assert!(
            config
                .resource_attributes
                .contains(&("nudox.build_system".to_owned(), "cargo".to_owned()))
        );
    }
}
