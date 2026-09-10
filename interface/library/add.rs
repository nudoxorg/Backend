//! Defines add behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the add invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Adding and removing packages: the one mutation every surface performs identically.

use interface_core::{CorrelationId, PackageCompilePhase, PackageUrl, PackageUrlError};
use interface_identity::PackageCoordinate;

use crate::{PackageCard, ShelfFailure};

/// One ordered compile phase with its position, for progress rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilePhaseProgress {
    /// Phase just entered.
    pub phase: PackageCompilePhase,
    /// Zero-based ordinal of the phase.
    pub ordinal: u8,
    /// Total ordered phases.
    pub total: u8,
}

impl CompilePhaseProgress {
    /// Positions one phase in the fixed eight-step journey.
    #[must_use]
    pub const fn of(phase: PackageCompilePhase) -> Self {
        let ordinal = match phase {
            PackageCompilePhase::Locate => 0,
            PackageCompilePhase::EnterSource => 1,
            PackageCompilePhase::Authority => 2,
            PackageCompilePhase::Lower => 3,
            PackageCompilePhase::Publish => 4,
            PackageCompilePhase::Reopen => 5,
            PackageCompilePhase::Discover => 6,
            PackageCompilePhase::Render => 7,
        };
        Self {
            phase,
            ordinal,
            total: 8,
        }
    }

    /// Short label for progress rows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self.phase {
            PackageCompilePhase::Locate => "locate",
            PackageCompilePhase::EnterSource => "read",
            PackageCompilePhase::Authority => "analyze",
            PackageCompilePhase::Lower => "lower",
            PackageCompilePhase::Publish => "publish",
            PackageCompilePhase::Reopen => "verify",
            PackageCompilePhase::Discover => "index",
            PackageCompilePhase::Render => "render",
        }
    }
}

/// Progress delivered while an add runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddProgress {
    /// The shelf row was recorded and the compile lock acquired.
    Admitted {
        /// Correlation of the add.
        correlation: CorrelationId,
    },
    /// One ordered compile phase was entered.
    Phase(CompilePhaseProgress),
    /// Publication finished; the durable lexical projection is being built.
    Indexing,
}

/// Why an add was refused before any work started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddRejection {
    /// The package URL was malformed.
    PackageUrl {
        /// Exact structural cause.
        cause: PackageUrlError,
    },
    /// This library was opened without a compiler.
    CompilerDetached,
    /// Another process holds the compile lock for this coordinate or another.
    Busy {
        /// Coordinate the other process is compiling, when readable.
        active: Option<PackageCoordinate>,
    },
    /// The shelf already holds a ready card for this coordinate.
    AlreadyReady,
    /// The shelf is full.
    ShelfFull {
        /// Fixed maximum.
        maximum: usize,
    },
}

/// Why an admitted add did not finish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddFailure {
    /// Correlation of the add.
    pub correlation: CorrelationId,
    /// Exact cause, also recorded on the shelf row.
    pub cause: ShelfFailure,
}

/// One refused add, retaining the exact spelling that was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedAdd {
    /// Exact package URL the caller supplied, returned rather than dropped.
    pub url: PackageUrl,
    /// Exact cause.
    pub rejection: AddRejection,
}

/// Terminal of one add.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddOutcome {
    /// The package is on the shelf and readable everywhere.
    Ready {
        /// Publication facts.
        card: PackageCard,
    },
    /// Refused before starting; the shelf is unchanged and the operand is handed back.
    Rejected(RejectedAdd),
    /// Started and failed; the shelf row records the cause.
    Failed(AddFailure),
}

/// Terminal of one remove.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoveOutcome {
    /// The row and its derived projections were removed.
    Removed,
    /// No row existed; nothing changed.
    Absent,
    /// The row is being compiled by a live process and was left alone.
    Busy,
}
