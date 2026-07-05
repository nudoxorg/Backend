//! Durable persistence of the tracked-package registry across restarts.
//!
//! A process that dies mid-sync leaves packages stuck in
//! [`ResolutionState::Progressing`] with nobody working them — the state is
//! transient and meaningless across a restart. On load, this module resets every
//! such in-flight package back to a re-enqueueable state and hands them to the
//! queue, so a crash never strands work. Terminal states
//! ([`Stored`](ResolutionState::Stored), [`DeadLettered`](ResolutionState::DeadLettered))
//! are left untouched.

use heart::{PackageId, ResolutionState};

use crate::{error::RegistryError, index::GlobalStore, queue::Queue};
// The reconciliation bodies (still `todo!()`) build their statements from these.
#[allow(unused_imports)]
use crate::schema::{codec, queries};

/// The outcome of a restart reconciliation: what was reset and re-enqueued.
#[derive(Debug, Clone, Default)]
pub struct Recovered {
	/// Packages that were `Progressing` and got reset to re-enqueueable.
	pub reset: Vec<PackageId>,

	/// Packages re-enqueued for a fresh attempt.
	pub requeued: Vec<PackageId>,
}

/// On startup, find every package stuck mid-sync, reset its transient state, and
/// re-enqueue it.
///
/// Reads the global index for all [`ResolutionState::Progressing`] packages,
/// transitions each back to `Unindexed { needed: false }` (a clean,
/// re-enqueueable state) in one transaction, and enqueues them. Idempotent: safe
/// to run on every boot.
pub async fn reconcile_on_start(
	index: &GlobalStore,
	queue: &Queue,
) -> Result<Recovered, RegistryError> {
	let _ = (index, queue);
	todo!("SELECT Progressing packages, reset to Unindexed in a txn, enqueue each")
}

/// Reset a single package's transient in-flight state to re-enqueueable. The
/// unit [`reconcile_on_start`] applies in bulk; exposed for targeted recovery.
pub fn reset_transient(state: &ResolutionState) -> ResolutionState {
	let _ = state;
	todo!("map Progressing back to Unindexed(needed:false); pass terminal states through unchanged")
}
