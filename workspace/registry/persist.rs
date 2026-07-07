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
#[tracing::instrument(skip_all)]
pub async fn reconcile_on_start(
	index: &GlobalStore,
	queue: &Queue,
) -> Result<Recovered, RegistryError> {
	use sqlx::Row;

	use crate::error::IndexError;

	let pool = index.pool();

	// 1. Which packages were stranded mid-sync?
	let (select_sql, select_vals) = queries::index::select_progressing();
	let rows = sqlx::query_with(&select_sql, select_vals)
		.fetch_all(pool)
		.await
		.map_err(|e| IndexError::Database(e))?;
	let reset: Vec<PackageId> = rows
		.iter()
		.map(|row| row.try_get::<uuid::Uuid, _>(0).map(codec::package_id_from_uuid))
		.collect::<Result<_, _>>()
		.map_err(|e| IndexError::Database(e))?;

	// 2. Reset every transient row and drop every stale lease, atomically —
	//    a half-applied recovery would be worse than none.
	let mut tx = pool.begin().await.map_err(|e| IndexError::Database(e))?;
	let (reset_sql, reset_vals) = queries::persist::reset_transient_parse_status();
	sqlx::query_with(&reset_sql, reset_vals)
		.execute(&mut *tx)
		.await
		.map_err(|e| IndexError::Database(e))?;
	let (lease_sql, lease_vals) = queries::persist::clear_all_leases();
	sqlx::query_with(&lease_sql, lease_vals)
		.execute(&mut *tx)
		.await
		.map_err(|e| IndexError::Database(e))?;
	tx.commit().await.map_err(|e| IndexError::Database(e))?;

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
