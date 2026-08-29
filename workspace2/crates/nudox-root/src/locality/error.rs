use thiserror::Error;

use super::{LocalityReadError, SelectedCount};

/// Sparse selected-ordinal storage rejected before classification begins.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SelectedOrdinalBufferError {
    /// The selected closure exceeds the buffer's declared compact capacity.
    #[error("selected ordinal buffer has {available:?} entries but requires {required:?}")]
    TooSmall {
        /// Exact selected descriptor count for this invocation.
        required: SelectedCount,
        /// Declared compact ordinal capacity.
        available: SelectedCount,
    },
    /// A locality payload changed after its immutable validation boundary.
    #[error("selected locality read failed")]
    Read(#[from] LocalityReadError),
}
