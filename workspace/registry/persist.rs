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

use index::engine::VersioningEngine;
use index::enums::ParseState;
use index::store::lifecycle;

use crate::{error::RegistryError, index::GlobalStore, queue::Queue};

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
/// Reads the catalog for all `in_progress` versions, transitions each back to
/// `Unindexed { needed: false }` (a clean, re-enqueueable state), drops every
/// stale scratch lease, and enqueues them. Idempotent: safe to run on every boot.
#[tracing::instrument(skip_all)]
pub async fn reconcile_on_start<Engine: VersioningEngine + Send + Sync>(
	index: &GlobalStore<Engine>,
	queue: &Queue,
) -> Result<Recovered, RegistryError> {
	// 1. Which packages were stranded mid-sync?
	let reset = lifecycle::versions_in_state(index.writer().engine(), ParseState::InProgress)
		.map_err(crate::error::IndexError::Catalog)?;

	// 2. Reset every transient row, then drop every stale lease. Each reset is
	//    an idempotent single-row write; a crash mid-loop just re-runs on the
	//    next boot (the survivors are still `in_progress`).
	for package in &reset {
		index
			.set_state(*package, &ResolutionState::Unindexed { needed: false })
			.await?;
	}
	queue.clear_all_leases().await?;

	// 3. Re-enqueue each recovered package (idempotent per package).
	let mut requeued = Vec::with_capacity(reset.len());
	for package in &reset {
		queue.enqueue(*package).await?;
		requeued.push(*package);
	}

	if !reset.is_empty() {
		tracing::info!(count = reset.len(), "recovered packages stranded mid-sync");
	}
	Ok(Recovered { reset, requeued })
}

/// Reset a single package's transient in-flight state to re-enqueueable. The
/// unit [`reconcile_on_start`] applies in bulk; exposed for targeted recovery.
pub fn reset_transient(state: &ResolutionState) -> ResolutionState {
	match state {
		// Mid-flight progress is meaningless across a restart: nobody is
		// working it, so it goes back to the clean, re-enqueueable start.
		ResolutionState::Progressing(_) => ResolutionState::Unindexed { needed: false },
		// Everything else — never-started, terminal, or failure-recorded — is
		// durable truth and passes through untouched.
		other => other.clone(),
	}
}
