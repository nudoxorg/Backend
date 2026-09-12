//! Defines shelf behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the shelf invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The shelf: every package a person asked for, what happened to it, and what it became.

use std::path::Path;

use compiler_vocabulary::LanguageProfile;
use heart_identity::GenerationId;
use interface_core::{CorrelationId, PackageCompilePhase, SemanticImageAuthority};
use interface_documents::Census;
use interface_identity::PackageCoordinate;

use crate::LibraryEpoch;

/// Most shelf entries one library retains.
pub const MAX_SHELF_ENTRIES: usize = 4096;

/// Seconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(pub u64);

/// Facts about one successfully published package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageCard {
    /// Pinned coordinate.
    pub coordinate: PackageCoordinate,
    /// Language profile the compiler proved.
    pub profile: LanguageProfile,
    /// Compiler generation that owns the publication.
    pub generation: GenerationId,
    /// Complete semantic image identity and extent.
    pub image: SemanticImageAuthority,
    /// Declaration counts from the reopened image.
    pub census: Census,
    /// When the publication became visible.
    pub published_at: Timestamp,
    /// Where the package sources the compiler read live on this machine, when they still do.
    pub source_root: Option<Box<Path>>,
}

/// Why an add did not produce a card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShelfFailure {
    /// The package directory is not present beneath the configured ecosystem root.
    PackageNotFound,
    /// No root is configured for the ecosystem.
    EcosystemRootUnavailable,
    /// The compiler rejected the source with a retained diagnostic summary.
    Compiler {
        /// First diagnostic line, bounded.
        summary: Box<str>,
    },
    /// Durable publication failed.
    Publication,
    /// The compile was cancelled.
    Cancelled,
    /// The owning process exited before recording a terminal.
    Orphaned,
}

/// Lifecycle of one shelf entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShelfStatus {
    /// Recorded, not yet started.
    Requested,
    /// A process is compiling it and last reported this phase.
    Compiling {
        /// Most recent ordered phase.
        phase: PackageCompilePhase,
    },
    /// Published and readable.
    Ready {
        /// Publication facts.
        card: PackageCard,
    },
    /// The add reached a terminal failure.
    Failed {
        /// Exact cause.
        cause: ShelfFailure,
    },
}

/// One shelf row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShelfEntry {
    /// Pinned coordinate.
    pub coordinate: PackageCoordinate,
    /// Current status.
    pub status: ShelfStatus,
    /// When the package was requested.
    pub requested_at: Timestamp,
    /// Correlation of the request that created the row.
    pub correlation: CorrelationId,
}

/// A consistent read of the whole shelf at one epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shelf {
    /// Rows in request order.
    pub entries: Box<[ShelfEntry]>,
    /// Epoch at which the rows were read.
    pub epoch: LibraryEpoch,
}

impl Shelf {
    /// Finds one entry by coordinate.
    #[must_use]
    pub fn entry(&self, coordinate: &PackageCoordinate) -> Option<&ShelfEntry> {
        self.entries.iter().find(|entry| &entry.coordinate == coordinate)
    }

    /// Ready cards in request order.
    pub fn ready(&self) -> impl Iterator<Item = &PackageCard> {
        self.entries.iter().filter_map(|entry| match &entry.status {
            ShelfStatus::Ready { card } => Some(card),
            ShelfStatus::Requested | ShelfStatus::Compiling { .. } | ShelfStatus::Failed { .. } => {
                None
            }
        })
    }
}

/// Exact shelf storage failure.
#[derive(Debug)]
pub enum ShelfError {
    /// The shelf store could not be opened or read.
    Store {
        /// Underlying description, bounded.
        detail: Box<str>,
    },
    /// The shelf is full.
    Capacity {
        /// Fixed maximum.
        maximum: usize,
    },
    /// The epoch file failed.
    Epoch(crate::EpochError),
}
