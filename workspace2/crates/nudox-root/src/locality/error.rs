use thiserror::Error;

use super::SelectedCount;

/// Sparse selected-ordinal storage rejected before classification begins.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SelectedOrdinalBufferError {
    /// The selected closure exceeds the buffer's declared compact capacity.
    #[error("selected ordinal buffer has {available:?} entries but requires {required:?}")]
    TooSmall {
        /// Exact selected descriptor count for this invocation.
        required: SelectedCount,
        /// Declared compact ordinal capacity.
        available: SelectedCount,
    },
}
