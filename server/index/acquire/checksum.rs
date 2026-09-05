use crate::transport::TransportFault;
use thiserror::Error;

/// A SHA-256 checksum advertised by the registry for exact source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveChecksum([u8; 32]);

impl ArchiveChecksum {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Decodes exactly 64 lowercase or uppercase hexadecimal SHA-256 characters.
    pub fn parse_hex(input: &str) -> Result<Self, RegistryError> {
        if input.len() != 64 {
            return Err(RegistryError::MalformedChecksum);
        }
        let mut bytes = [0_u8; 32];
        for (index, cell) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            let high = hex(input.as_bytes()[offset]).ok_or(RegistryError::MalformedChecksum)?;
            let low = hex(input.as_bytes()[offset + 1]).ok_or(RegistryError::MalformedChecksum)?;
            *cell = high << 4 | low;
        }
        Ok(Self(bytes))
    }

    /// Returns the canonical digest bytes.
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Exact acquisition and source protocol failures.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The injected transport failed before a source response could be admitted.
    #[error(transparent)]
    Transport(#[from] TransportFault),
    /// A source replied with a status outside this protocol's accepted set.
    #[error("registry protocol status {status}")]
    Status {
        /// Exact HTTP status received from the source.
        status: u16,
    },
    /// The cursor stored for this source was not a positive bounded page number.
    #[error("registry cursor is malformed")]
    Cursor,
    /// A bounded source document was not valid protocol JSON.
    #[error("registry protocol document is malformed")]
    Protocol,
    /// The registry named an invalid package coordinate.
    #[error("registry package coordinate is invalid")]
    Coordinate,
    /// A checksum was missing or violated the SHA-256 representation.
    #[error("registry checksum is malformed")]
    MalformedChecksum,
    /// Source bytes disagreed with the registry's content address.
    #[error("registry archive checksum mismatch")]
    ChecksumMismatch {
        /// Content address advertised by the sparse index.
        expected: ArchiveChecksum,
        /// Content address calculated from the admitted archive bytes.
        actual: ArchiveChecksum,
    },
}
