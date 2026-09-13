use crate::NativeProtocolError;

/// Preparation cache failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparationError {
    /// The session key did not agree with the authority or manifest version.
    KeyMismatch,
    /// Canonical request validation or encoding failed.
    Protocol(NativeProtocolError),
    /// The request or cache configuration exceeded its bound.
    Capacity,
    /// A request was encoded concurrently with an explicit invalidation.
    Invalidated,
    /// A bounded generation or byte accounting counter overflowed.
    Overflow,
}

impl std::fmt::Display for PreparationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyMismatch => formatter.write_str("preparation key is not authority-bound"),
            Self::Protocol(error) => write!(formatter, "native preparation failed: {error}"),
            Self::Capacity => formatter.write_str("native preparation cache bound exceeded"),
            Self::Invalidated => formatter.write_str("native preparation was invalidated"),
            Self::Overflow => formatter.write_str("native preparation accounting overflowed"),
        }
    }
}

impl std::error::Error for PreparationError {}

impl From<NativeProtocolError> for PreparationError {
    fn from(error: NativeProtocolError) -> Self {
        Self::Protocol(error)
    }
}
