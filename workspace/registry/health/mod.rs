//! Readiness / liveness for the registry's backing stores.
//!
//! The registry fronts several backends ([`heart::BackendKind`]); this module
//! probes each and rolls their status into one [`Health`] verdict the server's
//! `/readyz` surface reports. A single degraded backend is distinguished from a
//! full outage so a load balancer can shed the right amount of traffic.

use heart::BackendKind;

use crate::error::RegistryError;

/// The rolled-up health verdict across every registry backend.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Health {
	/// Every backend probed healthy.
	Ready,

	/// Some backends are down but the registry can still serve a reduced surface;
	/// carries which backends are impaired.
	Degraded(Vec<BackendKind>),

	/// A backend required for any operation is down.
	Down,
}

impl Health {
	/// Whether the registry should accept traffic at all.
	pub const fn is_serving(&self) -> bool { !matches!(self, Health::Down) }
}

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
#[allow(async_fn_in_trait, reason = "native RPITIT is the crate-wide convention; not object-safe by design")]
#[diagnostic::on_unimplemented(
	message = "`{Self}` cannot be health-probed",
	note = "implement `Probeable` so the registry health aggregator can include it"
)]
pub trait Probeable {
	/// The backend kind this store reports as.
	fn backend(&self) -> BackendKind;

	/// Run a cheap liveness probe (a ping / HEAD / `SELECT 1`).
	async fn probe(&self) -> Probe;
}

/// Aggregate per-backend probe results into one [`Health`] verdict.
///
/// This is the whole readiness policy, over already-collected [`Probe`] values —
/// which backends are *required* to serve vs. merely degrading lives here, in
/// one place. It takes concrete data, not futures, so no boxing is involved:
/// the caller (the server, which owns the concrete store handles) runs the
/// probes with a plain `tokio::join!` and passes the results here. That keeps
/// `Probeable::probe` a native `async fn` (RPITIT) with no `Box<dyn Future>`
/// anywhere.
pub fn aggregate(probes: &[Probe]) -> Health {
	let _ = probes;
	todo!("classify probes: all-healthy => Ready, required-down => Down, else Degraded(list)")
}

/// Assert a store's probe future is `Send` (so the aggregator can `join!` it on a
/// multi-threaded runtime) without boxing — via `return_type_notation`, matching
/// the store connect/query guards elsewhere in the workspace.
pub fn assert_probe_future_send<P>()
where
	P: Probeable<probe(..): Send>,
{
	let _ = std::marker::PhantomData::<fn() -> RegistryError>;
}
