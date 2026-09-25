//! The initialization flow: ensure a package is present and fresh before a
//! caller serves against it.

#[allow(unused_imports)]
use crate::server::registry;
use crate::server::registry::{
    GlobalPackage, Package, RegistryError, error::IndexError, identity::PackageCoordinates,
};
use heart::{Edition, Freshness, Language, PackageId, ResolutionState, Toolchain};
use serde::{Deserialize, Serialize};

use crate::server::{
    Server,
    authz::WriteCap,
    error::{ServerError, ServerResult},
};
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

/// How a dependency-list revision meets the catalog.
///
/// The first enqueue publishes the whole record. Any later revision replaces
/// feed edges and leaves lifecycle state where it is, including a package that
/// is already stored. An unchanged payload touches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogTouch {
    /// First publish: identity, facets, edges, and lifecycle.
    Upsert,
    /// A changed dependency list on a package that already has a catalog row.
    ReplaceEdges,
    /// The versioned payload matches the tip, or this call is not publishing.
    Leave,
}

/// `first_enqueue` is the call that must create the catalog row.
/// `revised` is a Turso payload whose hash differs from the tip.
pub fn catalog_touch(first_enqueue: bool, revised: bool) -> CatalogTouch {
    if first_enqueue {
        CatalogTouch::Upsert
    } else if revised {
        CatalogTouch::ReplaceEdges
    } else {
        CatalogTouch::Leave
    }
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

/// Facets that carry feed-supplied dependency names, or `None` when there are
/// none. Names are trimmed, emptied names dropped, then sorted and deduped.
pub fn facets_from_dependency_names(names: &[String]) -> Option<crate::metadata::SearchFacets> {
    use smol_str::SmolStr;
    let record = crate::record::PackageRecord::from_parts(
        Language::Rust,
        "provisional",
        "0.0.0",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        crate::record::runtime_edges_from_names(names),
    );
    let mut dependencies: Vec<SmolStr> = record
        .runtime_names()
        .into_iter()
        .map(SmolStr::new)
        .collect();
    if dependencies.is_empty() {
        return None;
    }
    dependencies.sort();
    dependencies.dedup();
    Some(crate::metadata::SearchFacets {
        dependencies,
        ..crate::metadata::SearchFacets::default()
    })
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
        cap: &WriteCap,
        coordinates: &PackageCoordinates,
    ) -> ServerResult<Initialized> {
        self.ensure_initialized_with(cap, coordinates, &[], None).await
    }

    /// Like [`Self::ensure_initialized`], and when `dependencies` is non-empty
    /// the first upsert stores those names on [`SearchFacets::dependencies`].
    pub async fn ensure_initialized_with(
        &self,
        _cap: &WriteCap,
        coordinates: &PackageCoordinates,
        dependencies: &[String],
        checksum: Option<&str>,
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

        let mut published = crate::record::PackageRecord::published(
            coordinates.ecosystem(),
            coordinates.name.canonical(),
            coordinates.version.canonical(),
            dependencies,
        );
        if let Some(digest) = checksum.and_then(crate::pid::sha256) {
            published = published.with_content(digest);
        }
        let fact_write = {
            let mut facts = self
                .package_facts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if dependencies.is_empty()
                && facts
                    .get(
                        coordinates.ecosystem().as_token(),
                        coordinates.name.canonical().as_ref(),
                        coordinates.version.canonical().as_ref(),
                    )
                    .is_some()
            {
                match published.content {
                    Some(digest) => facts
                        .bind_content(
                            coordinates.ecosystem().as_token(),
                            coordinates.name.canonical().as_ref(),
                            coordinates.version.canonical().as_ref(),
                            digest,
                        )
                        .map_err(|error| {
                            ServerError::Runtime(
                                crate::server::registry::runtime::error::TextError::Io(
                                    std::io::Error::other(error.to_string()),
                                )
                                .into(),
                            )
                        })?,
                    None => crate::engine::turso_vc::FactWrite::Unchanged,
                }
            } else {
                facts.put_record(&published).map_err(|error| {
                    ServerError::Runtime(
                        crate::server::registry::runtime::error::TextError::Io(
                            std::io::Error::other(error.to_string()),
                        )
                        .into(),
                    )
                })?
            }
        };
        let touch = catalog_touch(
            enqueued,
            matches!(fact_write, crate::engine::turso_vc::FactWrite::Revised(_)),
        );
        if touch != CatalogTouch::Leave {
            // Publishing the record and enqueueing the job are each idempotent
            // (identity upsert; unique live job per package), so this pair
            // converges even if the process dies between the two writes.
            // A later publish with a new dependency list revises the versioned
            // row and replaces feed edges without rewriting lifecycle state,
            // including a package that is already stored.
            let mut record = provisional_global_package(coordinates);
            record.facets = facets_from_dependency_names(dependencies);
            match touch {
                CatalogTouch::Upsert => {
                    stores
                        .global_store
                        .upsert(&record)
                        .await
                        .map_err(RegistryError::from)?;
                }
                CatalogTouch::ReplaceEdges => {
                    stores
                        .global_store
                        .replace_feed_edges(&record)
                        .await
                        .map_err(RegistryError::from)?;
                }
                CatalogTouch::Leave => {}
            }
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

#[cfg(test)]
mod tests {
    use super::{CatalogTouch, catalog_touch, facets_from_dependency_names};

    #[test]
    fn a_revised_payload_replaces_edges_after_the_first_publish() {
        assert_eq!(catalog_touch(true, true), CatalogTouch::Upsert);
        assert_eq!(catalog_touch(true, false), CatalogTouch::Upsert);
        assert_eq!(catalog_touch(false, true), CatalogTouch::ReplaceEdges);
        assert_eq!(catalog_touch(false, false), CatalogTouch::Leave);
    }

    #[test]
    fn dependency_names_are_trimmed_sorted_and_deduped() {
        let facets = facets_from_dependency_names(&[
            " ms ".into(),
            "debug".into(),
            "debug".into(),
            "  ".into(),
        ])
        .expect("names");
        let names: Vec<_> = facets
            .dependencies
            .iter()
            .map(|name| name.as_str())
            .collect();
        assert_eq!(names, vec!["debug", "ms"]);
        assert!(facets_from_dependency_names(&[" ".into()]).is_none());
    }
}
