//! Versioned library catalog façade.
//!
//! `Library` is a read-only projection over one accepted immutable view.
//! Query indexes are retained by [`ProjectionArrangement`], while transitions
//! are admitted by `ViewRoot` and committed through the backend flow seam.

mod certificate;
mod command;
mod projection;
mod subscription;

use crate::arrangement::{ProjectionArrangement, WorkCounters};
use crate::{
    Basis, CommittedViewDelta, CoverageCapability, Cursor, Frontier, Lane, LibraryError,
    PreparedViewDelta, QueryWork, Reason, RowId, ViewError, ViewProjection, ViewProjectionError,
    ViewRoot, ViewSnapshot, ViewStateRoot, object_version, view_key, view_state_root,
};
use std::sync::Arc;

/// A search page whose explicit row order cannot be confused with the
/// canonical identity order of its immutable view relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedSearchSnapshot {
    snapshot: ViewSnapshot,
    order: Box<[RowId]>,
}

impl RankedSearchSnapshot {
    /// Returns the canonical snapshot used for proof and cursor admission.
    #[must_use]
    pub const fn snapshot(&self) -> &ViewSnapshot {
        &self.snapshot
    }

    /// Returns result identities in relevance order.
    #[must_use]
    pub fn order(&self) -> &[RowId] {
        &self.order
    }

    /// Drops presentation order for compatibility command transports.
    #[must_use]
    pub fn into_snapshot(self) -> ViewSnapshot {
        self.snapshot
    }
}

/// Read-only library projection over one engine-published immutable view.
///
/// The catalog owns no package/document/name database and no durable intent
/// log. An engine supplies a checked [`ViewRoot`] and commits user intents
/// through its workspace owner; this façade performs bounded projections over
/// accepted rows and carries their exact basis/frontier.
#[derive(Clone, Debug)]
pub struct Library {
    pub(super) cursor: Cursor,
    pub(super) view: Arc<ViewRoot>,
    pub(super) arrangement: Arc<ProjectionArrangement>,
    pub(super) work: Arc<WorkCounters>,
}

impl Default for Library {
    fn default() -> Self {
        Self::new()
    }
}

impl Library {
    /// Creates an empty library with a coherent default source/view basis.
    #[must_use]
    pub fn new() -> Self {
        let source_root = view_state_root(&[]);
        let source_object = object_version(b"library-source-v1");
        let basis = Basis::new(source_root, source_object);
        let frontier = Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0);
        let view = ViewRoot::unavailable(
            view_key(b"library-view-v1"),
            basis,
            frontier,
            Lane::Exact,
            Reason::NoIndex,
        );
        let cursor = Cursor::for_view_root(&view);
        let work = Arc::new(WorkCounters::default());
        let arrangement = Arc::new(ProjectionArrangement::empty());
        Self {
            cursor,
            view: Arc::new(view),
            arrangement,
            work,
        }
    }

    /// Creates a projection from an engine-owned coherent view and cursor.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::View`] when the supplied root is incoherent or
    /// [`LibraryError::WrongBasis`] when the cursor does not identify it.
    pub fn from_view(view: ViewRoot, cursor: Cursor) -> Result<Self, LibraryError> {
        let expected = view.root();
        let observed = cursor.root();
        let projection = ViewProjection::admit(view, cursor).map_err(|error| match error {
            ViewProjectionError::Incoherent | ViewProjectionError::UnsupportedSchema => {
                LibraryError::View(ViewError::IncoherentBase)
            }
            ViewProjectionError::CursorMismatch => LibraryError::WrongBasis {
                expected,
                observed: observed.into(),
            },
            ViewProjectionError::FreshnessMismatch
            | ViewProjectionError::BasisMismatch { .. }
            | ViewProjectionError::MissingCoverage => LibraryError::View(ViewError::IncoherentBase),
        })?;
        Self::from_projection(projection)
    }

    /// Creates a projection from one already admitted root/cursor capability.
    ///
    /// The capability keeps the immutable view and its subscription cursor
    /// together while the arrangement is built. This is the preferred engine
    /// boundary; [`Self::from_view`] remains as a convenience for callers that
    /// have two values at a legacy API seam.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::View`] when the retained root cannot build its
    /// bounded query arrangement.
    pub fn from_projection(projection: ViewProjection) -> Result<Self, LibraryError> {
        let cursor = projection.cursor();
        let view = projection.into_root();
        let work = Arc::new(WorkCounters::default());
        let arrangement = Arc::new(
            ProjectionArrangement::build(&view, &work)
                .map_err(|_| LibraryError::View(ViewError::InvalidRelationDelta))?,
        );
        Ok(Self {
            cursor,
            view: Arc::new(view),
            arrangement,
            work,
        })
    }

    /// Creates an empty library after an engine has admitted complete source
    /// coverage. The capability is supplied by the producer boundary; this
    /// constructor never infers it from the source root.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::View`] when the supplied capability does not
    /// cover this library's source object.
    pub fn with_coverage(capability: CoverageCapability) -> Result<Self, LibraryError> {
        let library = Self::new();
        let basis = library.view.basis;
        let frontier = library.view.frontier;
        let view =
            ViewRoot::empty_checked(view_key(b"library-view-v1"), basis, frontier, capability)?;
        let cursor = Cursor::for_view_root(&view);
        Self::from_view(view, cursor)
    }

    /// Returns the exact immutable materialized revision used for new query
    /// snapshots. The producer basis identifies the shared source stream;
    /// this root advances only when visible content changes.
    #[must_use]
    pub fn revision_root(&self) -> ViewStateRoot {
        self.view.root
    }

    /// Returns the current immutable view root.
    #[must_use]
    pub fn view(&self) -> &ViewRoot {
        &self.view
    }

    /// Returns structural work paid by arrangement construction and queries.
    #[must_use]
    pub fn work_counters(&self) -> QueryWork {
        self.work.snapshot()
    }

    /// Returns the current branch/log/schema cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Commits one checked view preparation and advances this projection to
    /// the derived immutable target version.
    ///
    /// The preparation is consumed by the same exact-base check used by
    /// [`ViewRoot::commit`]. The retained query arrangements are replaced as
    /// one immutable snapshot and their structural work accounting charges
    /// only the rows named by the committed change (coverage-only changes do
    /// not re-index rows).
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::View`] when the preparation is forged or was
    /// made against another view root.
    pub fn commit(
        self,
        prepared: PreparedViewDelta,
    ) -> Result<(Self, CommittedViewDelta), LibraryError> {
        let old_view = self.view.clone();
        let (next_view, committed) = prepared
            .commit(old_view.as_ref())
            .map_err(LibraryError::View)?;
        let cursor = Cursor::for_view_root(&next_view);
        let arrangement = Arc::new(
            ProjectionArrangement::update(
                self.arrangement.as_ref(),
                &next_view,
                &committed,
                &self.work,
            )
            .map_err(|_| LibraryError::View(ViewError::InvalidRelationDelta))?,
        );
        Ok((
            Self {
                cursor,
                view: Arc::new(next_view),
                arrangement,
                work: self.work,
            },
            committed,
        ))
    }
}
