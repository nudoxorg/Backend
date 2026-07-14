//! Continuous CPU profiling via Pyroscope (OBSERVABILITY-PLAN.md §6 / Phase 5).
//!
//! CPU profiling only — `pyroscope_pprofrs`'s `PprofConfig` is left at its
//! default (no allocation-space sampling). `main.rs` sets `mimalloc` as the
//! `#[global_allocator]`; alloc-space profiling would need an allocator-aware
//! sampling hook mimalloc doesn't provide the same way jemalloc does, so it is
//! explicitly out of scope here (per the plan's instruction not to enable it).

use pyroscope::PyroscopeAgent;
use pyroscope::pyroscope::PyroscopeAgentRunning;
use pyroscope_pprofrs::{PprofConfig, pprof_backend};

use crate::config::TelemetryConfig;

/// A running Pyroscope agent handle. Stopped (best-effort) when dropped by
/// the owning [`crate::TelemetryGuard`].
pub(crate) struct PyroscopeHandle {
	agent: Option<PyroscopeAgent<PyroscopeAgentRunning>>,
}

impl PyroscopeHandle {
	/// Start the agent against `config.pyroscope_endpoint`, tagged with the
	/// same `service_name`/`environment` the rest of the pipeline uses. Fails
	/// open: any construction/start error is logged and yields a handle that
	/// is simply inert (`agent: None`) — profiling being unavailable must
	/// never be fatal to boot, mirroring every other signal in this crate.
	pub(crate) fn start(config: &TelemetryConfig) -> Self {
		let Some(endpoint) = config.pyroscope_endpoint.as_deref() else {
			return Self { agent: None };
		};

		let backend = pprof_backend(PprofConfig::new().sample_rate(100));

		let build = PyroscopeAgent::builder(endpoint, config.service_name.as_str())
			.backend(backend)
			.tags(vec![
				("environment", config.environment.as_str()),
				("service_version", config.service_version.as_str()),
			])
			.build();

		let agent = match build {
			Ok(agent) => agent,
			Err(error) => {
				tracing::warn!(%error, endpoint, "pyroscope agent construction failed; continuing without profiling");
				return Self { agent: None };
			}
		};

		match agent.start() {
			Ok(running) => {
				tracing::info!(endpoint, "pyroscope continuous profiling started");
				Self { agent: Some(running) }
			}
			Err(error) => {
				tracing::warn!(%error, endpoint, "pyroscope agent failed to start; continuing without profiling");
				Self { agent: None }
			}
		}
	}

	/// Stop the agent, if running. Best-effort: logs and swallows a stop
	/// error rather than panicking during shutdown/drop.
	pub(crate) fn stop(self) {
		if let Some(agent) = self.agent
			&& let Err(error) = agent.stop() {
				tracing::warn!(%error, "pyroscope agent stop failed");
			}
	}
}
