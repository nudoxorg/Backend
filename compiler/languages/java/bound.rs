//! Validates the source-bound envelope around a javac semantic image.
//! Retains raw source identity beside the existing typed javac planes.
//! Delegates all declaration, type, symbol, and reference validation to `JavaImage`.

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ImageError, JavaImage};

const MAGIC: [u8; 4] = *b"NJAB";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 80;
const DIGEST_DOMAIN: &[u8] = b"nudox.java.bound.authority.image.sha256.v1\0";

/// A source-bound javac authority image borrowing its caller-owned bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JavaAuthorityImage<'image> {
    /// Validated typed javac semantic planes within the outer source-bound envelope.
    pub image: JavaImage<'image>,
    source_digest: [u8; 32],
}

impl<'image> JavaAuthorityImage<'image> {
    /// Opens a checksummed source-bound image and every embedded javac plane.
    pub fn open(bytes: &'image [u8]) -> Result<Self, BoundImageError> {
        if bytes.len() < HEADER_BYTES {
            return Err(BoundImageError::Header(BoundHeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let found_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if found_magic != MAGIC {
            return Err(BoundImageError::Header(BoundHeaderError::Magic {
                found: found_magic,
            }));
        }
        let version = u16_at(bytes, 4);
        if version != VERSION {
            return Err(BoundImageError::Header(BoundHeaderError::Version {
                found: version,
            }));
        }
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(BoundImageError::Header(BoundHeaderError::Length {
                found: header_bytes,
            }));
        }
        let image_bytes = usize::try_from(u32_at(bytes, 8)).map_err(|_| {
            BoundImageError::Header(BoundHeaderError::ImageLength {
                declared: usize::MAX,
                actual: bytes.len() - HEADER_BYTES,
            })
        })?;
        if bytes.len() != HEADER_BYTES + image_bytes {
            return Err(BoundImageError::Header(BoundHeaderError::ImageLength {
                declared: image_bytes,
                actual: bytes.len() - HEADER_BYTES,
            }));
        }
        if bytes[76..HEADER_BYTES] != [0; 4] {
            return Err(BoundImageError::Header(BoundHeaderError::Reserved));
        }
        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[12..44]);
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&bytes[..44]);
        digest.update(&bytes[76..HEADER_BYTES]);
        digest.update(&bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice() != &bytes[44..76] {
            return Err(BoundImageError::Digest);
        }
        let image = JavaImage::open(&bytes[HEADER_BYTES..]).map_err(BoundImageError::Image)?;
        Ok(Self {
            image,
            source_digest,
        })
    }

    /// Returns the SHA-256 digest of the exact configured Java source bytes.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }
}

/// Exact failure while opening a source-bound javac authority image.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BoundImageError {
    /// The outer fixed envelope is malformed.
    #[error("invalid Java authority binding header: {0}")]
    Header(#[from] BoundHeaderError),
    /// The outer source-bound envelope checksum differs from its bytes.
    #[error("Java authority binding checksum does not match")]
    Digest,
    /// The embedded typed javac authority image is invalid.
    #[error("embedded Java authority image is invalid: {0}")]
    Image(#[from] ImageError),
}

/// Exact fixed-envelope rejection returned before opening typed javac planes.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BoundHeaderError {
    /// The complete fixed outer header is unavailable.
    #[error("truncated header: found {actual} bytes, need at least {HEADER_BYTES}")]
    Truncated {
        /// Number of envelope bytes available to the decoder.
        actual: usize,
    },
    /// The outer magic differs from `NJAB`.
    #[error("unexpected magic {found:?}")]
    Magic {
        /// Raw four-byte envelope magic found in the supplied bytes.
        found: [u8; 4],
    },
    /// The outer image version is not supported by this reader.
    #[error("unsupported image version {found}")]
    Version {
        /// Unsupported raw outer image version.
        found: u16,
    },
    /// The fixed outer header size differs from this version's size.
    #[error("unexpected fixed header length {found}")]
    Length {
        /// Header byte count encoded by the envelope.
        found: usize,
    },
    /// The embedded image length disagrees with the supplied envelope body.
    #[error("declared embedded image length {declared} differs from actual {actual}")]
    ImageLength {
        /// Embedded image byte count declared in the outer header.
        declared: usize,
        /// Embedded image bytes available after the outer header.
        actual: usize,
    },
    /// Reserved fixed header bytes are not zero.
    #[error("reserved header bytes are non-zero")]
    Reserved,
}

const fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

const fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
