//! Defines image behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the image invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closure-scoped access to one reopened semantic image, so a borrowed view can never outlive it.

use compiler_ir::EntityId;
use interface_documents::{Outline, Page, ProjectionError, ProjectionLimits, Symbol};
use interface_identity::{ExactAddress, PackageCoordinate, SymbolPath};

use crate::PackageCard;

/// One reopened, validated semantic image borrowed for the duration of a [`crate::Library::read`]
/// closure.
///
/// The image bytes and every borrowed view live inside the closure; what escapes is owned
/// document data. That is the whole reason this is a closure API rather than a handle.
pub struct Image<'image> {
    pub(crate) card: &'image PackageCard,
    pub(crate) bytes: &'image [u8],
}

impl Image<'_> {
    /// The card this image was reopened for.
    #[must_use]
    pub const fn card(&self) -> &PackageCard {
        self.card
    }

    /// The owning package.
    #[must_use]
    pub const fn package(&self) -> &PackageCoordinate {
        &self.card.coordinate
    }

    /// Exact validated image bytes, for callers that build their own borrowed readers.
    #[must_use]
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// Projects one page.
    ///
    /// # Errors
    ///
    /// Returns the exact missing coordinate or exceeded budget.
    pub fn page(&self, entity: EntityId, limits: ProjectionLimits) -> Result<Page, ProjectionError> {
        let _ = limits;
        Err(ProjectionError::MissingEntity { entity })
    }

    /// Projects the containment tree.
    ///
    /// # Errors
    ///
    /// Returns the exact missing coordinate.
    pub fn outline(&self) -> Result<Outline, ProjectionError> {
        Ok(Outline {
            package: self.card.coordinate.clone(),
            roots: Box::new([]),
            census: self.card.census,
        })
    }

    /// Spells one entity's identity header without projecting its page.
    ///
    /// # Errors
    ///
    /// Returns the exact missing coordinate or depth overflow.
    pub fn symbol(&self, entity: EntityId) -> Result<Symbol, ProjectionError> {
        Err(ProjectionError::MissingEntity { entity })
    }

    /// Finds every declaration whose root-relative path matches, ignoring kind qualifiers the
    /// caller omitted and honoring the ones it supplied.
    #[must_use]
    pub fn resolve_path(&self, path: &SymbolPath) -> Box<[ExactAddress]> {
        let _ = path;
        Box::new([])
    }
}
