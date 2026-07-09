//! Health + parse-status coordination for the registry's backing stores.
//!
//! Aggregates per-backend readiness ([`heart::Probe`]) into one [`Health`]
//! verdict the server's `/readyz` surface reports. A single degraded backend is
//! distinguished from a full outage so a load balancer can shed the right
//! amount of traffic.
//!
//! Per-store probe implementations live on the store types via
//! [`heart::Probeable`]; this module owns only the roll-up policy.

use heart::BackendKind;

// Re-export so existing `registry::health::{Probe, Probeable}` paths keep working.
pub use heart::{Probe, Probeable};

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
	pub const fn is_serving(&self) -> bool {
		!matches!(self, Health::Down)
	}
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
	/// The backends without which the registry cannot serve *anything*: the
	/// relational spine (identity, lifecycle, queue, outbox) and the blob store
	/// (the durable root every read plane derives from). The derived stores
	/// (qdrant / terminus / tantivy) only degrade their own surfaces.
	const REQUIRED: [BackendKind; 2] = [BackendKind::Postgres, BackendKind::ObjectStore];

	let impaired: Vec<BackendKind> = probes
		.iter()
		.filter(|probe| !probe.healthy)
		.map(|probe| probe.backend)
		.collect();

	match &impaired[..] {
		[] => Health::Ready,
		down if down.iter().any(|backend| REQUIRED.contains(backend)) => {
			tracing::warn!(?down, "a required backend is down; registry not serving");
			Health::Down
		}
		_ => {
			tracing::warn!(backends = ?impaired, "registry degraded");
			Health::Degraded(impaired)
		}
	}
}
