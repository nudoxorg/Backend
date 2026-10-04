//! SQL cache reconciliation belongs to the serialized product source owner.
//!
//! Keep the projection-side compare-and-swap capture before reading the
//! owner's current view. Borrowing the daemon prevents this caller from
//! advancing its authoritative view while that exact snapshot is projected.
//! It does not certify the newest observation of an external registry or peer.

use super::ProductDaemon;
use backend_extension_turso::{
    PackageGraphRevision, ProjectionError, ProjectionGraphSeed, ProjectionSeed, ProjectionUpdate,
    TursoProjection,
};
use backend_library::{CheckedPackageGraphFacts, ViewRoot};
use std::path::Path;

const MAX_OPEN_ATTEMPTS: usize = 8;

/// Opens a selected generation, or seeds an empty namespace from this owner.
/// Unsupported selected schemas and ambiguous publication remain refusals.
pub(super) fn open_current(
    path: &Path,
    daemon: &ProductDaemon,
) -> Result<TursoProjection, ProjectionError> {
    for _ in 0..MAX_OPEN_ATTEMPTS {
        match futures_executor::block_on(TursoProjection::open(path)) {
            Ok(mut projection) => {
                synchronize_current(&mut projection, daemon)?;
                return Ok(projection);
            }
            Err(ProjectionError::NeedsSeed) => {
                // A first-seed CAS loser must discard its proposed seed. The
                // next iteration opens the winner and reads the owner again
                // after capturing that selected generation's SQL revision.
                let seed = ProjectionSeed::new(
                    daemon.engine().daemon().library().view(),
                    ProjectionGraphSeed::Unavailable,
                );
                match futures_executor::block_on(TursoProjection::seed_if_empty(path, seed)) {
                    Ok(projection) => return Ok(projection),
                    Err(ProjectionError::AlreadySeeded | ProjectionError::SeedConflict) => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(ProjectionError::NamespaceBusy)
}

/// Reconciles only the view currently held by the borrowed product owner.
/// No retained caller-supplied view can enter this full-root replacement path.
pub(super) fn synchronize_current(
    projection: &mut TursoProjection,
    daemon: &ProductDaemon,
) -> Result<ProjectionUpdate, ProjectionError> {
    let expected = futures_executor::block_on(projection.revision())?;
    let view = daemon.engine().daemon().library().view();
    futures_executor::block_on(projection.synchronize_from(expected, view))
}

/// A graph's SQL base paired with a borrow of the current product view.
/// Capture this before obtaining registry or manifest observations. The borrow
/// keeps that view fixed while source owners build and validate their snapshot.
pub(super) struct GraphBase<'a> {
    revision: PackageGraphRevision,
    view: &'a ViewRoot,
}

impl<'a> GraphBase<'a> {
    pub(super) fn capture(
        projection: &TursoProjection,
        daemon: &'a ProductDaemon,
    ) -> Result<Self, ProjectionError> {
        let revision = futures_executor::block_on(projection.package_graph_revision())?;
        let view = daemon.engine().daemon().library().view();
        if revision.view_root() != *view.root().as_bytes()
            || revision.view_version() != *view.version().as_bytes()
        {
            return Err(ProjectionError::StaleTransition);
        }
        Ok(Self { revision, view })
    }

    pub(super) fn view(&self) -> &'a ViewRoot {
        self.view
    }

    /// The caller must retain or revalidate its source-owner observation before
    /// consuming this base. Checked facts certify shape/content, not freshness.
    pub(super) fn synchronize(
        self,
        projection: &mut TursoProjection,
        facts: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        futures_executor::block_on(projection.synchronize_checked_package_graph_from(
            self.revision,
            self.view.root(),
            facts,
        ))
    }
}
