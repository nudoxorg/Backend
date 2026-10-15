//! Universe-shared variance-cache data and accessors.

use super::TypeInterner;
use crate::def::DefId;
use crate::types::Variance;
use std::sync::Arc;

/// A variance result plus its unresolved-definition fingerprint.
pub type SharedDefVariance = (Arc<[Variance]>, Arc<[DefId]>);

impl TypeInterner {
    /// Read a universe-shared variance mask. `mode` selects the variance
    /// computation that produced it (`0` is the declared mask).
    #[inline]
    pub fn shared_def_variance(&self, def_id: DefId, mode: u8) -> Option<SharedDefVariance> {
        self.def_variance_masks
            .get(&(def_id, mode))
            .map(|entry| entry.value().clone())
    }

    /// Store a canonical universe-shared variance mask for `mode`.
    #[inline]
    pub fn insert_shared_def_variance(
        &self,
        def_id: DefId,
        mode: u8,
        mask: Arc<[Variance]>,
        gaps: Arc<[DefId]>,
    ) {
        self.def_variance_masks
            .entry((def_id, mode))
            .or_insert((mask, gaps));
    }
}
