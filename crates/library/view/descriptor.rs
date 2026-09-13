//! Constant size identity descriptors for immutable view roots.

use super::{Basis, Coverage, CoverageCapability, Row, ViewError, ViewRoot};
use crate::canonical::{Frontier, ViewRecipeId, ViewStateRoot, ViewVersion};

/// Constant-size identity of one immutable view root.
///
/// The descriptor deliberately carries no rows.  It is cheap to retain in a
/// subscription lease and can be sent with every page so a reconnect never
/// has to repeat a complete visible snapshot.  A descriptor becomes a
/// [`ViewRoot`] only after all authenticated pages have been assembled and
/// checked against its row count, root, version, basis, and coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewRootDescriptor {
    pub(super) recipe: ViewRecipeId,
    pub(super) version: ViewVersion,
    pub(super) root: ViewStateRoot,
    pub(super) basis: Basis,
    pub(super) frontier: Frontier,
    pub(super) coverage: Box<[Coverage]>,
    pub(super) capability: Option<CoverageCapability>,
    pub(super) row_count: u64,
}

/// A producer-admitted snapshot descriptor whose visible root is intentionally
/// deferred until page hydration completes.
///
/// A fixed-width root digest is retained as bytes here instead of being
/// promoted to [`ViewStateRoot`].  The latter can only be constructed by
/// [`ViewRoot::new_checked`] after the complete row relation has been
/// rebuilt.  This distinction keeps a page certificate useful for bounded
/// transport while preventing a digest-only reset from becoming an accepted
/// root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewRootDescriptorClaim {
    recipe: ViewRecipeId,
    version: ViewVersion,
    root: [u8; backend_version::ID_BYTES],
    basis: Basis,
    frontier: Frontier,
    coverage: Box<[Coverage]>,
    capability: Option<CoverageCapability>,
    row_count: u64,
}

pub(crate) struct DescriptorParts {
    pub(crate) recipe: ViewRecipeId,
    pub(crate) version: ViewVersion,
    pub(crate) root: [u8; backend_version::ID_BYTES],
    pub(crate) basis: Basis,
    pub(crate) frontier: Frontier,
    pub(crate) coverage: Box<[Coverage]>,
    pub(crate) capability: Option<CoverageCapability>,
    pub(crate) row_count: u64,
}

impl ViewRootDescriptorClaim {
    pub(crate) fn from_parts(parts: DescriptorParts) -> Self {
        Self {
            recipe: parts.recipe,
            version: parts.version,
            root: parts.root,
            basis: parts.basis,
            frontier: parts.frontier,
            coverage: parts.coverage,
            capability: parts.capability,
            row_count: parts.row_count,
        }
    }

    /// Returns the stable view recipe identity.
    #[must_use]
    pub const fn recipe(&self) -> ViewRecipeId {
        self.recipe
    }

    /// Returns the admitted immutable view version.
    #[must_use]
    pub const fn version(&self) -> ViewVersion {
        self.version
    }

    /// Returns the deferred visible root bytes.
    #[must_use]
    pub const fn root_bytes(&self) -> &[u8; backend_version::ID_BYTES] {
        &self.root
    }

    /// Returns the source basis bound to all hydrated rows.
    #[must_use]
    pub const fn basis(&self) -> Basis {
        self.basis
    }

    /// Returns the source branch/log/schema/sequence frontier.
    #[must_use]
    pub const fn frontier(&self) -> Frontier {
        self.frontier
    }

    /// Returns the producer coverage admitted for this descriptor.
    #[must_use]
    pub fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Returns the complete source capability, when one was admitted.
    #[must_use]
    pub fn capability(&self) -> Option<CoverageCapability> {
        self.capability.clone()
    }

    /// Returns the complete row count promised by the producer.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Rebuilds and admits the complete visible relation from authenticated
    /// pages.  Only this operation promotes the deferred root bytes into the
    /// typed [`ViewRoot`] identity used by reducers and queries.
    /// # Errors
    ///
    /// Returns an error when the encoded identity or checked state is invalid.
    pub fn admit_rows(self, rows: Vec<Row>) -> Result<ViewRoot, ViewError> {
        let observed = match self.capability {
            Some(capability) => ViewRoot::new_checked(
                self.recipe,
                self.basis,
                self.frontier,
                rows,
                self.coverage.to_vec(),
                capability,
            )?,
            None => ViewRoot::new_incomplete(
                self.recipe,
                self.basis,
                self.frontier,
                rows,
                self.coverage.to_vec(),
            )?,
        };
        if observed.recipe != self.recipe
            || observed.version != self.version
            || observed.root.as_bytes() != &self.root
            || observed.basis != self.basis
            || observed.frontier != self.frontier
            || observed.coverage.as_ref() != self.coverage.as_ref()
            || observed.row_count() != self.row_count
        {
            return Err(ViewError::WrongTarget);
        }
        Ok(observed)
    }
}

impl ViewRootDescriptor {
    /// Returns the stable view recipe identity.
    #[must_use]
    pub const fn recipe(&self) -> ViewRecipeId {
        self.recipe
    }

    /// Returns the immutable visible-view version.
    #[must_use]
    pub const fn version(&self) -> ViewVersion {
        self.version
    }

    /// Returns the canonical visible relation root.
    #[must_use]
    pub const fn root(&self) -> ViewStateRoot {
        self.root
    }

    /// Returns the source basis bound to every row.
    #[must_use]
    pub const fn basis(&self) -> Basis {
        self.basis
    }

    /// Returns the source branch/log/schema/sequence frontier.
    #[must_use]
    pub const fn frontier(&self) -> Frontier {
        self.frontier
    }

    /// Returns honest lane coverage for the complete root.
    #[must_use]
    pub fn coverage(&self) -> &[Coverage] {
        &self.coverage
    }

    /// Returns the producer coverage capability, when complete coverage was
    /// admitted at the owner boundary.
    #[must_use]
    pub fn capability(&self) -> Option<CoverageCapability> {
        self.capability.clone()
    }

    /// Returns the number of visible rows in the canonical relation.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Rebuilds a root from its descriptor after page hydration.
    ///
    /// The resulting canonical relation root and derived version are compared
    /// with the descriptor.  A page set that omits, duplicates, mutates, or
    /// reorders rows therefore cannot silently publish a different root.
    ///
    /// # Errors
    ///
    /// Returns [`ViewError`] when coverage, row basis, canonical root, version,
    /// or row count differs from the checked descriptor.
    pub fn admit_rows(self, rows: Vec<Row>) -> Result<ViewRoot, ViewError> {
        let observed = match self.capability {
            Some(capability) => ViewRoot::new_checked(
                self.recipe,
                self.basis,
                self.frontier,
                rows,
                self.coverage.to_vec(),
                capability,
            )?,
            None => ViewRoot::new_incomplete(
                self.recipe,
                self.basis,
                self.frontier,
                rows,
                self.coverage.to_vec(),
            )?,
        };
        if observed.recipe != self.recipe
            || observed.version != self.version
            || observed.root != self.root
            || observed.basis != self.basis
            || observed.frontier != self.frontier
            || observed.coverage.as_ref() != self.coverage.as_ref()
            || observed.row_count() != self.row_count
        {
            return Err(ViewError::WrongTarget);
        }
        Ok(observed)
    }
}
