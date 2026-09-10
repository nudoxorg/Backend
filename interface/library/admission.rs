//! Defines admission behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the admission invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The move-only proof that one process may compile one package right now.

use interface_core::{CorrelationId, PackageUrl};
use interface_identity::PackageCoordinate;

use crate::{AddOutcome, AddProgress, Library};

/// Exclusive permission to compile one package, held for exactly one run.
///
/// An `Admission` is minted only by [`Library::admit`], which records the shelf row and takes the
/// cross-process compile lock. It borrows the library so it cannot outlive it, it is neither
/// `Clone` nor `Copy` so the permission cannot be duplicated, and it can only be consumed by
/// [`Admission::run`]. Dropping it unrun releases the lock and records the row as cancelled, so a
/// surface that admits and then abandons a package never leaves a phantom "compiling" row behind.
#[must_use = "an unrun admission releases its lock and records a cancelled shelf row"]
pub struct Admission<'library> {
    pub(crate) library: &'library Library,
    pub(crate) correlation: CorrelationId,
    pub(crate) coordinate: PackageCoordinate,
    pub(crate) url: PackageUrl,
    pub(crate) ran: bool,
}

impl Admission<'_> {
    /// Correlation recorded on the shelf row.
    #[must_use]
    pub const fn correlation(&self) -> CorrelationId {
        self.correlation
    }

    /// Coordinate this admission may compile.
    #[must_use]
    pub const fn coordinate(&self) -> &PackageCoordinate {
        &self.coordinate
    }

    /// Runs the admitted compile to its terminal, consuming the permission.
    ///
    /// Blocks the calling thread. `progress` receives ordered phases; a phase never implies
    /// success, and only [`AddOutcome::Ready`] proves the package is readable.
    pub fn run(mut self, progress: &mut dyn FnMut(AddProgress)) -> AddOutcome {
        self.ran = true;
        self.library.run_admitted(&self, progress)
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if !self.ran {
            self.library.abandon_admission(self);
        }
    }
}
