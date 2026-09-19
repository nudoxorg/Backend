//! Small progress receipts returned by transfer assembly.

use crate::SparseCoverage;

/// Progress receipt returned after staging a chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferReceipt {
    /// Current sparse coverage.
    pub coverage: SparseCoverage,
    /// Number of unique sequence numbers retained.
    pub chunks: usize,
}
