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
        Self::capture_revision(revision, daemon)
    }

    pub(super) fn capture_revision(
        revision: PackageGraphRevision,
        daemon: &'a ProductDaemon,
    ) -> Result<Self, ProjectionError> {
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

/// The derived SQL writer is transferred separately from its held read
/// snapshot. A reader has no write accessor and never reopens the selector.
pub(super) struct ProjectionOwner {
    identity: std::sync::Arc<()>,
    role: ProjectionRole,
}

enum ProjectionRole {
    Writer(TursoProjection),
    Captured(backend_extension_turso::TursoProjectionReadSnapshot),
}

#[must_use = "return the original derived projection writer to its owner"]
pub(super) struct ProjectionWriter {
    identity: std::sync::Arc<()>,
    projection: TursoProjection,
}

/// Checked while the actor still serves the captured head. Holding the
/// mutable field borrow prevents another reservation until installation.
pub(super) struct ProjectionReturn<'a> {
    owner: &'a mut ProjectionOwner,
    writer: ProjectionWriter,
}

impl ProjectionOwner {
    pub(super) fn new(projection: TursoProjection) -> Self {
        Self {
            identity: std::sync::Arc::new(()),
            role: ProjectionRole::Writer(projection),
        }
    }

    pub(super) fn writer_mut(&mut self) -> Result<&mut TursoProjection, ProjectionError> {
        match &mut self.role {
            ProjectionRole::Writer(writer) => Ok(writer),
            ProjectionRole::Captured(_) => Err(ProjectionError::NamespaceBusy),
        }
    }

    pub(super) fn revision(&self) -> Result<PackageGraphRevision, ProjectionError> {
        match &self.role {
            ProjectionRole::Writer(writer) => {
                futures_executor::block_on(writer.package_graph_revision())
            }
            ProjectionRole::Captured(reader) => Ok(reader.package_graph_revision()),
        }
    }

    pub(super) async fn read_package_graph_page(
        &self,
        request: &backend_library::PackageGraphPageRequest,
    ) -> Result<backend_library::PackageGraphPage, backend_extension_turso::PackageGraphReadError>
    {
        match &self.role {
            ProjectionRole::Writer(writer) => writer.read_package_graph_page(request).await,
            ProjectionRole::Captured(reader) => reader.read_package_graph_page(request).await,
        }
    }

    pub(super) fn reserve_writer(&mut self) -> Result<ProjectionWriter, ProjectionError> {
        let ProjectionRole::Writer(writer) = &self.role else {
            return Err(ProjectionError::NamespaceBusy);
        };
        let reader = futures_executor::block_on(writer.capture_read_snapshot())?;
        let ProjectionRole::Writer(writer) =
            std::mem::replace(&mut self.role, ProjectionRole::Captured(reader))
        else {
            unreachable!("selected original projection writer");
        };
        Ok(ProjectionWriter {
            identity: std::sync::Arc::clone(&self.identity),
            projection: writer,
        })
    }

    pub(super) fn prepare_return(
        &mut self,
        writer: ProjectionWriter,
    ) -> Result<ProjectionReturn<'_>, (ProjectionWriter, ProjectionError)> {
        if !matches!(self.role, ProjectionRole::Captured(_))
            || !std::sync::Arc::ptr_eq(&self.identity, &writer.identity)
        {
            return Err((writer, ProjectionError::NamespaceIdentity));
        }
        Ok(ProjectionReturn {
            owner: self,
            writer,
        })
    }
}

impl ProjectionWriter {
    pub(super) fn projection_mut(&mut self) -> &mut TursoProjection {
        &mut self.projection
    }
}

impl ProjectionReturn<'_> {
    pub(super) fn into_writer(self) -> ProjectionWriter {
        self.writer
    }

    /// No fallible work remains after prepare_return. The old read snapshot
    /// is returned intact for retirement on the existing publication worker.
    pub(super) fn install(self) -> backend_extension_turso::TursoProjectionReadSnapshot {
        let old = std::mem::replace(
            &mut self.owner.role,
            ProjectionRole::Writer(self.writer.projection),
        );
        let ProjectionRole::Captured(reader) = old else {
            unreachable!("prepared return retains exclusive field borrow");
        };
        reader
    }
}
