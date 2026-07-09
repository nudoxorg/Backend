//! Readiness probes shared across registry and runtime stores.
//!
//! Each backing store implements [`Probeable`] so the server can
//! `tokio::join!` probes uniformly without hand-rolled closures.

use std::time::Instant;

use crate::error::BackendKind;

/// The outcome of probing one backend.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Probe {
	/// Which backend was probed.
	pub backend: BackendKind,

	/// Whether it responded healthy.
	pub healthy: bool,

	/// Round-trip latency of the probe, if it completed.
	pub latency_ms: Option<u32>,

	/// A short human-readable detail when unhealthy.
	pub detail: Option<String>,
}

/// Something that can report its own liveness — implemented by each backing
/// store so the aggregator probes them uniformly.
#[allow(
	async_fn_in_trait,
	reason = "native RPITIT is the crate-wide convention; not object-safe by design"
)]
#[diagnostic::on_unimplemented(
	message = "`{Self}` cannot be health-probed",
	note = "implement `Probeable` so the health aggregator can include it"
)]
pub trait Probeable {
	/// The backend kind this store reports as.
	fn backend(&self) -> BackendKind;

	/// Run a cheap liveness probe (a ping / HEAD / `SELECT 1`).
	async fn probe(&self) -> Probe;
}

/// Time a probe body and shape its verdict. `None` detail means healthy.
pub async fn timed(backend: BackendKind, body: impl Future<Output = Option<String>>) -> Probe {
	let started = Instant::now();
	let detail = body.await;
	Probe {
		backend,
		healthy: detail.is_none(),
		latency_ms: u32::try_from(started.elapsed().as_millis()).ok(),
		detail,
	}
}
