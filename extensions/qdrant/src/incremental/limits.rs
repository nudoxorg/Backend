//! Bounded vector maintenance budgets and refresh strategies.

use crate::Error;

/// Bounded maintenance budget for a fresh exact vector overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayLimits {
    /// Maximum changed candidate identities retained in the overlay.
    pub max_changed_points: usize,
    /// Maximum encoded overlay bytes.
    pub max_bytes: usize,
    /// Maximum points accepted by one complete ANN base rebuild.
    pub max_rebuild_points: usize,
}

impl Default for OverlayLimits {
    fn default() -> Self {
        Self {
            max_changed_points: 512,
            max_bytes: 8 * 1024 * 1024,
            max_rebuild_points: 65_536,
        }
    }
}

impl OverlayLimits {
    /// Validates the maintenance envelope.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when any maintenance bound is zero.
    pub const fn validate(self) -> Result<Self, Error> {
        if self.max_changed_points == 0 || self.max_bytes == 0 || self.max_rebuild_points == 0 {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Exact vector refresh strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshKind {
    /// No changed relation rows.
    Reuse,
    /// Advance the exact overlay.
    Overlay,
    /// Rebuild/replace the immutable ANN base.
    Rebuild,
}
