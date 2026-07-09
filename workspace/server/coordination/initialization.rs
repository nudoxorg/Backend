//! The initialization flow: ensure a package is present and fresh before a
//! caller serves against it.

use heart::{Edition, Freshness, Language, PackageId, ResolutionState, Toolchain};
use registry::identity::PackageCoordinates;
use registry::{GlobalPackage, Package, RegistryError, error::IndexError};
use serde::{Deserialize, Serialize};

use runtime::vector::EmbeddingModel;
use crate::Server;
use crate::error::ServerResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Initialized {
	pub package: PackageId,
	pub state: ResolutionState,
	pub enqueued: bool,
}

/// What the initialization policy says to do for a package in a given state —
/// the pure heart of [`Server::ensure_initialized`], separated so the decision
/// table is testable without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializationDecision {
	/// Absent, unindexed, stale, or retriably failed: (re-)enqueue indexing.
	Enqueue,
	/// A worker already owns it; return its state without re-enqueueing.
	AlreadyInFlight,
	/// Stored and fresh: serve directly.
	Serve,
	/// Dead-lettered: a human owns it now; never auto-requeue.
	Hold,
}

/// The decision table: current lifecycle state (`None` = never seen) plus an
/// optional freshness verdict for stored packages.
pub fn initialization_decision(
	state: Option<&ResolutionState>,
	freshness: Option<Freshness>,
) -> InitializationDecision {
	match state {
		None | Some(ResolutionState::Unindexed { .. }) => InitializationDecision::Enqueue,
		Some(ResolutionState::Progressing(_)) => InitializationDecision::AlreadyInFlight,
		Some(ResolutionState::Stored { .. }) => match freshness {
			Some(Freshness::Stale) => InitializationDecision::Enqueue,
			Some(Freshness::Fresh) | None => InitializationDecision::Serve,
		},
		Some(ResolutionState::Failed(_)) => InitializationDecision::Enqueue,
		Some(ResolutionState::DeadLettered(_)) => InitializationDecision::Hold,
	}
}

/// The global record published when a package is first requested: identity from
/// its coordinates, lifecycle `Unindexed { needed: true }`, and a *provisional*
/// toolchain (provenance, never identity — the compile phase records the real
/// one).
pub fn provisional_global_package(coordinates: &PackageCoordinates) -> GlobalPackage {
	GlobalPackage {
		id: coordinates.id(),
		package: Package {
			coordinates: coordinates.clone(),
			toolchain: provisional_toolchain(coordinates.ecosystem()),
		},
		state: ResolutionState::Unindexed { needed: true },
		facets: None,
	}
}

/// The stand-in toolchain recorded before a package has ever been compiled.
fn provisional_toolchain(ecosystem: Language) -> Toolchain {
	match ecosystem {
		Language::Rust => Toolchain::Rust {
			compiler: semver::Version::new(1, 88, 0),
			edition: Edition::E2024,
		},
		Language::Typescript => Toolchain::Typescript { compiler: semver::Version::new(5, 8, 0) },
		Language::Python => Toolchain::Python { interpreter: semver::Version::new(3, 13, 0) },
		Language::Go => Toolchain::Go { compiler: semver::Version::new(1, 23, 0) },
		Language::Java => Toolchain::Java { compiler: semver::Version::new(23, 0, 0) },
		Language::Nix => Toolchain::Nix { evaluator: semver::Version::new(0, 1, 0) },
	}
}

impl<M: EmbeddingModel> Server<M> {
	pub async fn ensure_initialized(
		&self,
		coordinates: &PackageCoordinates,
	) -> ServerResult<Initialized> {
		self.authorize("packages.ensure_initialized")?;
		let stores = self.base();
		let package = coordinates.id();

		let state = match stores.global_store.get_state(package).await {
			Ok(state) => Some(state),
			Err(IndexError::NotFound { .. }) => None,
			Err(error) => return Err(RegistryError::from(error).into()),
		};

		// The ensure path treats a stored record as fresh — recomputing freshness
		// means re-acquiring the source, which is the sync surface's job.
		let decision = initialization_decision(state.as_ref(), None);
		tracing::debug!(%package, ?decision, "initialization decision");
		metrics::counter!("packages_ensure_initialized", "decision" => format!("{decision:?}"))
			.increment(1);

		let enqueued = matches!(decision, InitializationDecision::Enqueue);
		if enqueued {
			// Publishing the record and enqueueing the job are each idempotent
			// (identity upsert; unique live job per package), so this pair
			// converges even if the process dies between the two writes.
			let record = provisional_global_package(coordinates);
			stores.global_store.upsert(&record).await.map_err(RegistryError::from)?;
			stores.queue.enqueue(package).await.map_err(RegistryError::from)?;
		}

		let state = match (enqueued, state) {
			(false, Some(state)) => state,
			_ => ResolutionState::Unindexed { needed: true },
		};
		Ok(Initialized { package, state, enqueued })
	}
}
