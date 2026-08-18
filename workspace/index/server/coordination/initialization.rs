//! The initialization flow: ensure a package is present and fresh before a
//! caller serves against it.

#[allow(unused_imports)]
use crate::server::registry;
use crate::server::registry::identity::PackageCoordinates;
use crate::server::registry::{GlobalPackage, Package, RegistryError, error::IndexError};
use heart::{Edition, Freshness, Language, PackageId, ResolutionState, Toolchain};
use serde::{Deserialize, Serialize};

use crate::server::Server;
use crate::server::authz::WriteCap;
use crate::server::error::ServerResult;
use registry::vector::EmbeddingModel;

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
///
/// # Also the fleet-wide content-hash identity anchor (W9)
///
/// This is a **pure function of `Language` alone** — no host/OS/environment
/// input — so it returns byte-identical [`Toolchain`] values on every node in
/// the fleet. `indexing::execute_extract_phase` calls this (not whatever is
/// stored on the package record) to pick the `Toolchain` folded into
/// [`crate::blob::BlobManifest::identity_bytes`] for the snapshot's content
/// hash. That keeps snapshot identity a function of source bytes +
/// coordinates only, so a macOS node (which never runs a real toolchain
/// detection) and a Linux forge node (which, once per-node toolchain
/// detection lands, could otherwise record a real compiler version here)
/// compute the *same* hash for the *same* source — see `indexing.rs`'s
/// `identity_toolchain` for the call site and full rationale.
pub(super) fn provisional_toolchain(ecosystem: Language) -> Toolchain {
    match ecosystem {
        Language::Rust => Toolchain::Rust {
            compiler: semver::Version::new(1, 88, 0),
            edition: Edition::E2024,
        },
        Language::Typescript => Toolchain::Typescript {
            compiler: semver::Version::new(5, 8, 0),
        },
        Language::Python => Toolchain::Python {
            interpreter: semver::Version::new(3, 13, 0),
        },
        Language::Go => Toolchain::Go {
            compiler: semver::Version::new(1, 23, 0),
        },
        Language::Java => Toolchain::Java {
            compiler: semver::Version::new(23, 0, 0),
        },
        Language::CSharp => Toolchain::CSharp {
            sdk: semver::Version::new(10, 0, 0),
        },
        // C/C++ analyzed via a clang/libclang oracle (IR plane, RL-15).
        Language::Cpp => Toolchain::Cpp {
            compiler: semver::Version::new(18, 0, 0),
        },
    }
}

impl<M: EmbeddingModel> Server<M> {
    /// Ensure a package is initialized and enqueue it if needed.
    ///
    /// The caller must hold a [`WriteCap`] proving authorization has occurred.
    pub async fn ensure_initialized(
        &self,
        _cap: &WriteCap,
        coordinates: &PackageCoordinates,
    ) -> ServerResult<Initialized> {
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

        // `initialization_decision` stays pure and stateless — `Unindexed`
        // always *decides* `Enqueue`, deliberately, so it can't (and doesn't)
        // know whether that package is genuinely unseen or already has a live
        // job outstanding (see `initialization_decision`'s doc + the
        // `first_use_triggers_indexing` test, which pins exactly this: a
        // known-but-unindexed package still *decides* Enqueue). This call site
        // is the one place that also has `state`, so it is where "does this
        // decision need to be *acted on*" is decided: a package already
        // sitting at `Unindexed { needed: true }` was written by a *previous*
        // `ensure_initialized` call's Enqueue branch — a live job already
        // exists for it (the queue's `job_key` is the package uuid, so
        // `queue.enqueue` would silently no-op), so acting again would only
        // re-run `global_store.upsert`, which is not a no-op: it re-emits a
        // Text outbox intent for a record that has not actually changed.
        // Without this guard, every repeat "add"/"ensure" of an already-tracked-
        // but-not-yet-started package both mis-reports `enqueued: true` (see
        // `tests/api_add_package.rs::duplicate_add_returns_existing`,
        // `tests/initialization_flow.rs::ensure_init_returns_or_requests` /
        // `usage_is_tracked_for_tiering`) and piles up redundant outbox rows.
        let already_pending = matches!(state, Some(ResolutionState::Unindexed { needed: true }));
        let enqueued = matches!(decision, InitializationDecision::Enqueue) && !already_pending;
        let wants_work = matches!(decision, InitializationDecision::Enqueue);

        if enqueued {
            // Publishing the record and enqueueing the job are each idempotent
            // (identity upsert; unique live job per package), so this pair
            // converges even if the process dies between the two writes.
            let record = provisional_global_package(coordinates);
            stores
                .global_store
                .upsert(&record)
                .await
                .map_err(RegistryError::from)?;
        }

        // Enqueue on EVERY call that wants work, not only the first.
        //
        // # Why the `already_pending` guard must not cover this
        //
        // The guard above infers "a live job already exists" from the catalog
        // saying `Unindexed { needed: true }`. That inference is not sound: the
        // two writes are separate, and the comment above says so itself — the
        // pair "converges even if the process dies between them" only if a
        // *later* call re-enqueues. Guarding the enqueue is what stopped it.
        //
        // So any way of losing the job while keeping the catalog row — a crash
        // between the two writes, a scratch queue that did not survive, a job
        // dropped by an operator — left the package pinned at
        // `Unindexed { needed: true }` with nothing to run it, and every retry
        // answered `enqueued: false` and did nothing. There was no API path
        // back: the package could never be indexed again.
        //
        // Observed 2026-08-16 on macOS against a live `nudox-serve`:
        // `rust/ryu@1.0.18` sat at `Unindexed { needed: true }` across repeated
        // `POST /packages` *and a full server restart*, never compiling.
        //
        // Doing this unconditionally is safe by the queue's own contract:
        // `Queue::enqueue` keys on the package uuid, so a second enqueue hits
        // the primary key and is "a silent no-op that preserves the original
        // job (and thus its priority)". It cannot double-enqueue, cannot
        // reprioritize, and cannot disturb a job a worker has already claimed.
        // The only case it changes is the one that was previously terminal.
        if wants_work {
            stores
                .queue
                .enqueue(package)
                .await
                .map_err(RegistryError::from)?;
            if already_pending {
                // Not the same event as a first enqueue: this one either found
                // a live job (no-op) or repaired a lost one. Logged so the
                // repair is visible rather than silent — a package that needed
                // it was, by definition, stuck until now.
                tracing::info!(
                    %package,
                    "re-enqueued an already-pending package; a lost job would otherwise never run"
                );
            }
        }

        let state = match (enqueued, state) {
            (false, Some(state)) => state,
            _ => ResolutionState::Unindexed { needed: true },
        };
        Ok(Initialized {
            package,
            state,
            enqueued,
        })
    }
}
