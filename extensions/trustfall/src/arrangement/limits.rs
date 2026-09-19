//! Bounded graph arrangement maintenance budgets and refresh kinds.

use crate::Error;

/// Memory and work bounds for a retained graph overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArrangementLimits {
    /// Maximum changed logical rows retained in one overlay.
    pub max_changed_rows: usize,
    /// Maximum bytes retained by replacement rows.
    pub max_bytes: usize,
    /// Maximum rows admitted by one complete rebuild.
    pub max_rebuild_rows: usize,
}

impl Default for ArrangementLimits {
    fn default() -> Self {
        Self {
            max_changed_rows: 512,
            max_bytes: 4 * 1024 * 1024,
            max_rebuild_rows: 65_536,
        }
    }
}

impl ArrangementLimits {
    /// Validates bounds before they influence allocation or traversal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when any maintenance bound is zero.
    pub const fn validate(self) -> Result<Self, Error> {
        if self.max_changed_rows == 0 || self.max_bytes == 0 || self.max_rebuild_rows == 0 {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Selected graph refresh strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshKind {
    /// No effective relation rows changed.
    Reuse,
    /// Add the transition to the exact overlay.
    Overlay,
    /// Rebuild the immutable arrangement base.
    Rebuild,
}
