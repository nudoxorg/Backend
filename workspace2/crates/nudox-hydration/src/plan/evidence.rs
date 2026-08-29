use alloc::collections::TryReserveError;
use core::{mem::size_of, ops::Deref};

use nudox_root::{
    ClosureError, LocalityReadError, MetadataBytes, SelectedCount, SelectedOrdinalBuffer,
};
use thiserror::Error;

/// Semantic count measured in selected entries that are absent locally.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct AbsentCount(u32);

impl From<u32> for AbsentCount {
    fn from(count: u32) -> Self {
        Self(count)
    }
}

impl From<AbsentCount> for u32 {
    fn from(count: AbsentCount) -> Self {
        count.0
    }
}

impl Deref for AbsentCount {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AbsentCount {
    pub(crate) const ZERO: Self = Self(0);

    /// Checked addition for independently supplied absence counts.
    #[must_use]
    pub const fn checked_combined(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(count) => Some(Self(count)),
            None => None,
        }
    }
}

const _: [(); size_of::<AbsentCount>()] = [(); size_of::<u32>()];

/// Aggregate, closed coverage counts for one completed plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanCoverage {
    /// Descriptors selected for this plan.
    pub required: SelectedCount,
    /// Selected descriptors already present under the planner predicate.
    pub present: SelectedCount,
    /// Selected absent descriptors backed by an eligible provider set.
    pub promised: AbsentCount,
    /// Selected absent descriptors with no provider promise.
    pub missing: AbsentCount,
}

/// Closed rejection class for a planning operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanRejection {
    /// Caller closure scratch could not cover the selected root.
    ClosureScratchTooSmall,
    /// Caller plan scratch could not retain every selected descriptor.
    PlanScratchTooSmall,
    /// Validated locality could not reconstruct one selected entry.
    LocalityRead,
}

/// Immutable sparse-plan allocation evidence exposed by dereferencing scratch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanScratchFacts {
    /// Root-proven maximum selected count reserved before planning starts.
    pub capacity: SelectedCount,
    /// Actual reusable absent-ordinal allocation bytes, excluding allocator bookkeeping.
    pub retained_absence_bytes: MetadataBytes,
    /// Largest number of absent selected ordinals retained by one plan.
    pub high_water_absent: AbsentCount,
}

/// Caller-owned reusable sparse planning memory.
pub struct PlanScratch {
    absent: SelectedOrdinalBuffer,
    facts: PlanScratchFacts,
}

impl Deref for PlanScratch {
    type Target = PlanScratchFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl PlanScratch {
    /// Reserves a fixed maximum absent-ordinal count before planning begins.
    ///
    /// # Errors
    ///
    /// Returns the allocator's exact reservation cause before retaining
    /// partially initialized scratch.
    pub fn new(capacity: SelectedCount) -> Result<Self, TryReserveError> {
        let absent = SelectedOrdinalBuffer::new(capacity)?;
        Ok(Self {
            facts: PlanScratchFacts {
                capacity,
                retained_absence_bytes: absent.retained_bytes().into(),
                high_water_absent: AbsentCount::ZERO,
            },
            absent,
        })
    }

    pub(super) const fn begin_plan(
        &mut self,
    ) -> (&mut SelectedOrdinalBuffer, &mut PlanScratchFacts) {
        (&mut self.absent, &mut self.facts)
    }
}

/// Pure plan derivation failure.
#[derive(Debug, Error)]
pub enum PlanError {
    /// Caller-owned root-selection scratch was insufficient.
    #[error("closure scratch failure")]
    Closure(#[from] ClosureError),
    /// Validated locality could not reconstruct one selected entry.
    #[error("could not read selected locality")]
    LocalityRead(#[from] LocalityReadError),
    /// Caller-owned plan scratch cannot retain every selected descriptor.
    #[error("plan scratch has {available:?} entries but requires {required:?}")]
    ScratchTooSmall {
        /// Required root-proven selected count.
        required: SelectedCount,
        /// Available compact ordinal slots.
        available: SelectedCount,
    },
}
