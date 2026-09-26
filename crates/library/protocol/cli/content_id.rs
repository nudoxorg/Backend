//! Fixed `content:` text encoding of a typed content identity.
//!
//! The adapter boundary is string-only. Width, prefix, and lower-hex nibbles
//! are rejected before the digest is admitted as a domain content id.

use std::error::Error;
use std::fmt;

use crate::interface::ContentId;
use backend_version::{ContentIdDecodeError, Domain, HASH_BYTES};

/// Exact text width of one formatted content identity (`content:` plus 32 lower-hex bytes).
pub const CANONICAL_CONTENT_ID_TEXT_BYTES: usize = 8 + HASH_BYTES * 2;

/// Exact rejection while parsing the string-only canonical content-identity encoding.
#[derive(Debug, Eq, PartialEq)]
pub enum CanonicalContentIdDecodeError {
    /// The complete formatted identity width was not retained exactly.
    Width {
        /// Observed UTF-8 byte width.
        actual: usize,
        /// Required formatted width.
        expected: usize,
    },
    /// The fixed textual authority prefix was not present.
    Prefix {
        /// Observed prefix bytes.
        observed: [u8; 8],
    },
    /// One formatted digest nibble was not lower hexadecimal.
    Hex {
        /// Byte offset in the complete formatted value.
        offset: usize,
        /// Observed non-hex byte.
        observed: u8,
    },
    /// The decoded fixed-width identity failed typed authority validation.
    ContentId(ContentIdDecodeError),
}

impl fmt::Display for CanonicalContentIdDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Width { actual, expected } => write!(
                formatter,
                "canonical content identity has {actual} text bytes, expected {expected}"
            ),
            Self::Prefix { observed } => {
                write!(
                    formatter,
                    "canonical content identity prefix is not `content:`: "
                )?;
                for byte in observed {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
            Self::Hex { offset, observed } => write!(
                formatter,
                "canonical content identity byte {offset} is not lower hexadecimal: {observed:02x}"
            ),
            Self::ContentId(source) => source.fmt(formatter),
        }
    }
}

impl Error for CanonicalContentIdDecodeError {}

/// One typed content identity carried through a fixed string-only adapter boundary.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalContentId<DomainTag>(ContentId<DomainTag>);

impl<DomainTag> From<ContentId<DomainTag>> for CanonicalContentId<DomainTag> {
    fn from(value: ContentId<DomainTag>) -> Self {
        Self(value)
    }
}

impl<DomainTag> From<CanonicalContentId<DomainTag>> for ContentId<DomainTag> {
    fn from(value: CanonicalContentId<DomainTag>) -> Self {
        value.0
    }
}

impl<DomainTag> fmt::Display for CanonicalContentId<DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<DomainTag: Domain> TryFrom<&str> for CanonicalContentId<DomainTag> {
    type Error = CanonicalContentIdDecodeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        if bytes.len() != CANONICAL_CONTENT_ID_TEXT_BYTES {
            return Err(CanonicalContentIdDecodeError::Width {
                actual: bytes.len(),
                expected: CANONICAL_CONTENT_ID_TEXT_BYTES,
            });
        }
        if &bytes[..8] != b"content:" {
            let mut observed = [0; 8];
            observed.copy_from_slice(&bytes[..8]);
            return Err(CanonicalContentIdDecodeError::Prefix { observed });
        }

        let mut raw = [0; HASH_BYTES];
        for (index, byte) in raw.iter_mut().enumerate() {
            let high_offset = 8 + index * 2;
            let high = hex_nibble(bytes[high_offset], high_offset)?;
            let low_offset = high_offset + 1;
            let low = hex_nibble(bytes[low_offset], low_offset)?;
            *byte = (high << 4) | low;
        }
        ContentId::<DomainTag>::try_from(raw)
            .map(Self)
            .map_err(CanonicalContentIdDecodeError::ContentId)
    }
}

fn hex_nibble(byte: u8, offset: usize) -> Result<u8, CanonicalContentIdDecodeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(CanonicalContentIdDecodeError::Hex {
            offset,
            observed: byte,
        }),
    }
}
