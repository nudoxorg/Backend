use nudox_id::GenerationId;
use nudox_root::{EntryRange, GenerationView};
use thiserror::Error;

/// Requested immutable generation projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Projection {
    /// Full closure eligible for verified readiness.
    CompleteGeneration,
    /// Range and ancestor closure; intentionally not a whole-generation witness.
    Range(
        /// Inclusive semantic-key range.
        EntryRange,
    ),
}

/// Closed canonical dependency-set discriminant for a projection shape.
#[repr(u8)]
#[derive(Clone, Copy)]
pub(crate) enum ProjectionTag {
    CompleteGeneration = 0,
    Range = 1,
}

impl From<ProjectionTag> for u8 {
    #[allow(
        clippy::as_conversions,
        reason = "ProjectionTag is a repr(u8) closed protocol discriminant"
    )]
    fn from(tag: ProjectionTag) -> Self {
        tag as Self
    }
}

impl Projection {
    pub(crate) const fn range(self) -> Option<EntryRange> {
        match self {
            Self::CompleteGeneration => None,
            Self::Range(range) => Some(range),
        }
    }
    pub(crate) const fn is_complete(self) -> bool {
        matches!(self, Self::CompleteGeneration)
    }

    pub(crate) const fn tag(self) -> ProjectionTag {
        match self {
            Self::CompleteGeneration => ProjectionTag::CompleteGeneration,
            Self::Range(_) => ProjectionTag::Range,
        }
    }
}

/// A root-pinned logical demand independent of a store or transport adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Need {
    /// Immutable generation identity this demand is pinned to.
    pub pinned_root: GenerationId,
    /// Exact requested logical projection.
    pub projection: Projection,
}
impl Need {
    /// Pins a required projection to one immutable generation identity.
    #[must_use]
    pub const fn new(pinned_root: GenerationId, projection: Projection) -> Self {
        Self {
            pinned_root,
            projection,
        }
    }

    /// Checks this wire/request demand against one coherent generation view.
    ///
    /// # Errors
    ///
    /// Returns [`DemandBindError::GenerationMismatch`] when the request pins a
    /// different immutable generation than `view`.
    pub fn bind<'view, 'root, 'locality, DomainTag>(
        self,
        view: &'view GenerationView<'root, 'locality, DomainTag>,
    ) -> Result<BoundNeed<'view, 'root, 'locality, DomainTag>, DemandBindError> {
        if self.pinned_root != view.id {
            return Err(DemandBindError::GenerationMismatch {
                requested: self.pinned_root,
                actual: view.id,
            });
        }
        Ok(BoundNeed {
            view,
            projection: self.projection,
        })
    }
}

/// Demand proven coherent with one borrowed generation/locality composition.
pub struct BoundNeed<'view, 'root, 'locality, DomainTag> {
    pub(crate) view: &'view GenerationView<'root, 'locality, DomainTag>,
    /// Exact projection now bound to the view's generation proof.
    pub projection: Projection,
}

impl<DomainTag> Copy for BoundNeed<'_, '_, '_, DomainTag> {}
impl<DomainTag> Clone for BoundNeed<'_, '_, '_, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Creates a locally initiated demand already bound to its generation view.
#[must_use]
pub const fn demand<'view, 'root, 'locality, DomainTag>(
    view: &'view GenerationView<'root, 'locality, DomainTag>,
    projection: Projection,
) -> BoundNeed<'view, 'root, 'locality, DomainTag> {
    BoundNeed { view, projection }
}

/// Rejection while binding untrusted request identity to a generation view.
#[derive(Debug, Error)]
pub enum DemandBindError {
    /// The request named a different immutable generation.
    #[error("demand pinned {requested:?}, supplied generation was {actual:?}")]
    GenerationMismatch {
        /// Request identity.
        requested: GenerationId,
        /// Coherent view identity.
        actual: GenerationId,
    },
}
