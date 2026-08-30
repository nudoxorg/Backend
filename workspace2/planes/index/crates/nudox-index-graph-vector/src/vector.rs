use crate::VectorAuthority;

/// Vector-specific terminal facts. This type cannot be substituted for graph terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorTerminal {
    /// Every selected vector partition completed.
    Complete {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
    },
    /// Cancellation won before publication.
    Cancelled {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
    },
}

impl VectorTerminal {
    /// Returns the complete vector authority retained by every terminal.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        match self {
            Self::Complete { authority } | Self::Cancelled { authority } => authority,
        }
    }
}
