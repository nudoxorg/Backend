//! Proof bearing projections for client and subscription boundaries.
//!
//! A [`ViewRoot`] is an immutable value, but a root and a cursor are only a
//! useful client claim when they name the same view version, relation root,
//! source stream, and frontier.  Keeping that pair in one value gives the
//! callers a small, checked capability instead of making every frontend
//! repeat the same collection of equality checks.

use super::{Coverage, Freshness, ViewRoot, ViewSnapshot};
use crate::{Cursor, CursorEvent, ViewStateRoot};

/// Failure while admitting a root/cursor projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewProjectionError {
    /// The root failed its internal relation, version, or basis checks.
    Incoherent,
    /// The root uses a protocol/schema version this projection cannot read.
    UnsupportedSchema,
    /// The cursor names a different root, version, recipe, or source stream.
    CursorMismatch,
    /// The snapshot does not prove a freshness state accepted by its caller.
    FreshnessMismatch,
    /// The root is based on another source relation.
    BasisMismatch {
        /// Source relation expected by the caller.
        expected: ViewStateRoot,
        /// Source relation carried by the root.
        observed: ViewStateRoot,
    },
    /// A complete projection requires producer-admitted source coverage.
    MissingCoverage,
}

/// A root and cursor admitted as one coherent immutable projection.
///
/// The fields are private on purpose.  A caller can keep a digest-only
/// `Cursor` for diagnostics, but it cannot construct a new trusted pair by
/// changing one half of an already-admitted projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewProjection {
    root: ViewRoot,
    cursor: Cursor,
}

/// A projection whose visible rows and source scope were admitted complete by
/// a producer capability.  This is the type used for successful reset roots
/// and health/status claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompleteViewProjection(ViewProjection);

impl ViewProjection {
    /// Admits a root together with the cursor at its visible frontier.
    ///
    /// This is the canonical root-only admission used by health/status
    /// replies.  It deliberately derives the cursor from the already
    /// admitted frontier rather than allowing a caller to manufacture a
    /// second, slightly different set of identity checks.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the root or its derived cursor is
    /// incoherent.
    pub fn from_root(root: ViewRoot) -> Result<Self, ViewProjectionError> {
        let cursor = Cursor::for_view_root(&root);
        Self::admit(root, cursor)
    }

    /// Admits a root at its visible frontier against a caller-owned source
    /// relation.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the root is incoherent or based
    /// on another source relation.
    pub fn from_root_against(
        root: ViewRoot,
        expected_basis: ViewStateRoot,
    ) -> Result<Self, ViewProjectionError> {
        let cursor = Cursor::for_view_root(&root);
        Self::admit_against(root, cursor, expected_basis)
    }

    /// Admits a query snapshot and every cursor it exposes as one coherent
    /// projection.
    ///
    /// The returned projection is paired with `requested_cursor` when one is
    /// present, otherwise with the snapshot root's visible frontier.  A
    /// continuation cursor is checked independently because it may carry a
    /// query offset while retaining the same view identity.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the snapshot, requested cursor,
    /// continuation, basis, or freshness state is not admissible.
    pub fn from_snapshot(
        snapshot: &ViewSnapshot,
        expected_basis: Option<ViewStateRoot>,
        requested_cursor: Option<Cursor>,
    ) -> Result<Self, ViewProjectionError> {
        let projection = match requested_cursor {
            Some(cursor) => match expected_basis {
                Some(expected) => Self::admit_against(snapshot.root.clone(), cursor, expected)?,
                None => Self::admit(snapshot.root.clone(), cursor)?,
            },
            None => match expected_basis {
                Some(expected) => Self::from_root_against(snapshot.root.clone(), expected)?,
                None => Self::from_root(snapshot.root.clone())?,
            },
        };
        match snapshot.freshness {
            Freshness::Current => {}
            Freshness::Stale { observed } if expected_basis == Some(observed) => {}
            Freshness::Stale { .. } | Freshness::Unknown => {
                return Err(ViewProjectionError::FreshnessMismatch);
            }
        }
        if let Some(next) = snapshot.next {
            match expected_basis {
                Some(expected) => Self::admit_against(snapshot.root.clone(), next, expected)?,
                None => Self::admit(snapshot.root.clone(), next)?,
            };
        }
        Ok(projection)
    }

    /// Admits an immutable root and its exact stream cursor.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when either value is incoherent or the
    /// cursor does not identify the supplied root and frontier.
    pub fn admit(root: ViewRoot, cursor: Cursor) -> Result<Self, ViewProjectionError> {
        if !root.is_coherent() {
            return Err(ViewProjectionError::Incoherent);
        }
        if root.frontier().schema != crate::PROTOCOL_SCHEMA
            || cursor.schema() != crate::CURSOR_SCHEMA
        {
            return Err(ViewProjectionError::UnsupportedSchema);
        }
        if !cursor_names_root(cursor, &root) {
            return Err(ViewProjectionError::CursorMismatch);
        }
        Ok(Self { root, cursor })
    }

    /// Admits a root against a caller-owned source relation.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError::BasisMismatch`] when the root was
    /// produced from another source relation.
    pub fn admit_against(
        root: ViewRoot,
        cursor: Cursor,
        expected_basis: ViewStateRoot,
    ) -> Result<Self, ViewProjectionError> {
        if root.basis().root != expected_basis {
            return Err(ViewProjectionError::BasisMismatch {
                expected: expected_basis,
                observed: root.basis().root,
            });
        }
        Self::admit(root, cursor)
    }

    /// Converts a structurally coherent projection into a complete one.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError::MissingCoverage`] when the producer did
    /// not attach a capability or when any lane remains partial/unavailable.
    pub fn complete(self) -> Result<CompleteViewProjection, ViewProjectionError> {
        let root = &self.root;
        if root.capability().is_none()
            || !root.coverage().iter().copied().all(Coverage::is_complete)
        {
            return Err(ViewProjectionError::MissingCoverage);
        }
        Ok(CompleteViewProjection(self))
    }

    /// Returns the admitted immutable root.
    #[must_use]
    pub const fn root(&self) -> &ViewRoot {
        &self.root
    }

    /// Returns the exact cursor paired with the root.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Returns the source root named by the projection.
    #[must_use]
    pub const fn basis(&self) -> ViewStateRoot {
        self.root.basis().root
    }

    /// Consumes the projection and returns its immutable root.
    #[must_use]
    pub fn into_root(self) -> ViewRoot {
        self.root
    }

    /// Applies one bounded contiguous event suffix while retaining every
    /// explicit coverage state carried by the resulting root.
    ///
    /// # Errors
    ///
    /// Returns a cursor mismatch when the suffix does not chain or a checked
    /// delta cannot apply to its predecessor.
    pub fn apply_events(
        self,
        cursor: Cursor,
        events: &[CursorEvent],
    ) -> Result<Self, ViewProjectionError> {
        let mut root = self.root;
        let mut next = self.cursor;
        for event in events {
            let observed = next
                .advance_event(event)
                .map_err(|_| ViewProjectionError::CursorMismatch)?;
            match event {
                CursorEvent::Intent { .. } => {}
                CursorEvent::View { delta } => {
                    root = delta
                        .clone()
                        .apply_to(&root)
                        .map_err(|_| ViewProjectionError::CursorMismatch)?;
                }
            }
            next = observed;
        }
        if cursor != next {
            return Err(ViewProjectionError::CursorMismatch);
        }
        Self::admit(root, cursor)
    }
}

impl CompleteViewProjection {
    /// Admits a complete root together with the cursor at its visible
    /// frontier.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the root is incoherent or lacks
    /// producer-admitted complete coverage.
    pub fn from_root(root: ViewRoot) -> Result<Self, ViewProjectionError> {
        ViewProjection::from_root(root)?.complete()
    }

    /// Admits a complete root at its visible frontier against an exact
    /// source relation.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the root is incoherent, based on
    /// another source, or lacks complete producer coverage.
    pub fn from_root_against(
        root: ViewRoot,
        expected_basis: ViewStateRoot,
    ) -> Result<Self, ViewProjectionError> {
        ViewProjection::from_root_against(root, expected_basis)?.complete()
    }

    /// Admits a root/cursor pair and requires complete producer coverage.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when structural or complete-coverage
    /// admission fails.
    pub fn admit(root: ViewRoot, cursor: Cursor) -> Result<Self, ViewProjectionError> {
        ViewProjection::admit(root, cursor)?.complete()
    }

    /// Admits a complete root/cursor pair against an exact source basis.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError`] when the root is based on a different
    /// source or lacks producer-admitted complete coverage.
    pub fn admit_against(
        root: ViewRoot,
        cursor: Cursor,
        expected_basis: ViewStateRoot,
    ) -> Result<Self, ViewProjectionError> {
        ViewProjection::admit_against(root, cursor, expected_basis)?.complete()
    }

    /// Returns the admitted immutable root.
    #[must_use]
    pub const fn root(&self) -> &ViewRoot {
        self.0.root()
    }

    /// Returns the exact cursor paired with the root.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.0.cursor()
    }

    /// Returns the source root named by the projection.
    #[must_use]
    pub const fn basis(&self) -> ViewStateRoot {
        self.0.basis()
    }

    /// Consumes the complete projection and returns the underlying root.
    #[must_use]
    pub fn into_root(self) -> ViewRoot {
        self.0.into_root()
    }

    /// Applies a bounded, contiguous event suffix and returns the next
    /// proof-bearing projection.
    ///
    /// Event cursors are checked by the shared library kernel.  Consumers no
    /// longer need to duplicate root/version/branch/log/schema/sequence
    /// state-machine logic in each UI or protocol adapter.
    ///
    /// # Errors
    ///
    /// Returns [`ViewProjectionError::CursorMismatch`] when the suffix is not
    /// contiguous or a checked view delta cannot apply to the prior root.
    pub fn apply_events(
        self,
        cursor: Cursor,
        events: &[CursorEvent],
    ) -> Result<Self, ViewProjectionError> {
        self.0.apply_events(cursor, events)?.complete()
    }
}

fn cursor_names_root(cursor: Cursor, root: &ViewRoot) -> bool {
    cursor.recipe() == root.recipe()
        && cursor.version() == root.version()
        && cursor.root() == root.root()
        && cursor.branch() == root.frontier().branch
        && cursor.log() == root.frontier().log
        && cursor.schema() == root.frontier().schema
}
