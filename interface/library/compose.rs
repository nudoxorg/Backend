//! Defines compose behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the compose invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Commands composed from other commands: several pages in one call, and a diff between two versions.

use interface_documents::{Count, Page, ProjectionLimits, Signature, Symbol};
use interface_identity::PackageCoordinate;

use crate::{PageError, PageLocator};

/// Most locators one `read` accepts.
pub const MAX_READ_LOCATORS: usize = 16;
/// Most declarations one `diff` compares before it truncates.
pub const MAX_DIFF_SYMBOLS: usize = 4096;

/// One batched page request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadRequest {
    /// Pages wanted, in the order they will be answered; at most [`MAX_READ_LOCATORS`].
    pub locators: Box<[PageLocator]>,
    /// Projection budgets shared by every page.
    pub limits: ProjectionLimits,
}

/// One page of a batched read, answered or refused on its own.
#[derive(Debug)]
pub struct ReadPage {
    /// The locator as asked.
    pub locator: PageLocator,
    /// The page or its exact failure.
    pub page: Result<Page, PageError>,
}

/// The batched read; every locator is answered, so the whole cannot fail.
#[derive(Debug)]
pub struct ReadTerminal {
    /// One entry per locator, in request order.
    pub pages: Box<[ReadPage]>,
    /// Locators beyond [`MAX_READ_LOCATORS`] that were not attempted.
    pub dropped: Count,
}

/// One diff request between two pinned versions of one package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffRequest {
    /// The older version.
    pub from: PackageCoordinate,
    /// The newer version.
    pub to: PackageCoordinate,
}

/// One declaration-level change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffChange {
    /// Present only in `to`.
    Added(Symbol),
    /// Present only in `from`.
    Removed(Symbol),
    /// Present in both under the same path and kind with a different signature.
    Changed {
        /// As `from` spells it.
        before: Symbol,
        /// As `to` spells it.
        after: Symbol,
        /// Signature in `from`.
        signature_before: Signature,
        /// Signature in `to`.
        signature_after: Signature,
    },
}

/// The diff between two versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDiff {
    /// The older version.
    pub from: PackageCoordinate,
    /// The newer version.
    pub to: PackageCoordinate,
    /// Changes in canonical outline order: removed, added, then changed.
    pub changes: Box<[DiffChange]>,
    /// Declarations present in both with the same signature.
    pub unchanged: Count,
    /// Whether the comparison stopped at [`MAX_DIFF_SYMBOLS`].
    pub truncated: bool,
}
