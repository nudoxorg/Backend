//! Defines page behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the page invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Locating and resolving pages: turning anything a person types into one exact declaration.

use compiler_ir::EntityId;
use interface_documents::{ProjectionError, Symbol};
use interface_identity::{Address, AddressParseError, ContentKey, ExactAddress, PackageCoordinate};

/// How a caller names the page it wants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageLocator {
    /// A full or partial address.
    Address(Address),
    /// A bare key across every loaded package.
    Key(ContentKey),
    /// An entity coordinate inside one loaded package.
    Entity {
        /// Owning package.
        package: PackageCoordinate,
        /// Coordinate inside its image.
        entity: EntityId,
    },
}

impl From<ExactAddress> for PageLocator {
    fn from(address: ExactAddress) -> Self {
        Self::Address(address.into_address())
    }
}

/// What free text resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    /// Exactly one declaration.
    Exact(Symbol),
    /// Several declarations share the spelling; the caller must choose.
    Ambiguous(Box<[Symbol]>),
    /// The package is loaded but nothing matched.
    Unknown {
        /// Package that was searched.
        package: PackageCoordinate,
    },
}

/// Exact resolution failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// The text was not an address.
    Parse {
        /// Exact cause.
        cause: AddressParseError,
    },
    /// The named package is not on the shelf.
    PackageUnknown {
        /// Named package.
        package: PackageCoordinate,
    },
    /// The package is on the shelf but not ready.
    PackageNotReady {
        /// Named package.
        package: PackageCoordinate,
    },
}

/// Exact page failure.
#[derive(Debug)]
pub enum PageError {
    /// The locator did not resolve.
    Resolve(ResolveError),
    /// The locator resolved to several declarations.
    Ambiguous(Box<[Symbol]>),
    /// The image could not be reopened from its durable publication.
    Reopen(ReopenError),
    /// The projector rejected the page.
    Projection(ProjectionError),
    /// The key is not present in any loaded package.
    KeyUnknown {
        /// Requested key.
        key: ContentKey,
    },
}

/// Exact reopen failure for one ready card.
#[derive(Debug)]
pub struct ReopenError {
    /// Package whose image failed to reopen.
    pub package: PackageCoordinate,
    /// Which step failed.
    pub phase: ReopenPhase,
    /// Bounded description from the durable store.
    pub detail: Box<str>,
}

/// Which reopen step failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReopenPhase {
    /// Loading the generation binding.
    Binding,
    /// Loading the manifest.
    Manifest,
    /// Reading the image bytes.
    Bytes,
    /// Validating the image grammar.
    Validate,
    /// The bytes did not match the card's authority.
    Authority,
}
