//! Validates the fixed Go semantic-authority image emitted by `go/packages`.
//! Keeps declaration facts borrowed from a checksummed binary plane.
//! Rejects malformed or source-mismatched images before compiler admission.

use core::str;

use sha2::{Digest, Sha256};
use thiserror::Error;

const MAGIC: [u8; 4] = *b"NGAI";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 88;
const DECLARATION_BYTES: usize = 12;
const DIGEST_DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v1\0";

/// A closed declaration classification supplied by the Go type authority.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A defined named type from the package scope.
    Type = 1,
    /// A true type alias from the package scope.
    Alias = 2,
    /// A package-level function.
    Function = 3,
    /// A package-level constant.
    Constant = 4,
    /// A package-level variable.
    Static = 5,
}

impl DeclarationKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Type),
            2 => Some(Self::Alias),
            3 => Some(Self::Function),
            4 => Some(Self::Constant),
            5 => Some(Self::Static),
            _ => None,
        }
    }
}

/// One borrowed Go declaration from the checked authority image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Declaration<'image> {
    /// The declaration's closed Go kind.
    pub kind: DeclarationKind,
    /// Whether the package scope marks this identifier exported.
    pub exported: bool,
    /// Exact UTF-8 identifier bytes lent from the image atom plane.
    pub name: &'image [u8],
}

/// A validated immutable Go authority image borrowing caller-owned bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoImage<'image> {
    bytes: &'image [u8],
    declaration_count: usize,
    declaration_offset: usize,
    atom_offset: usize,
    atom_bytes: usize,
    source_digest: [u8; 32],
}

impl<'image> GoImage<'image> {
    /// Opens one complete fixed-layout Go authority image.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let found_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if found_magic != MAGIC {
            return Err(ImageError::Header(HeaderError::Magic {
                found: found_magic,
            }));
        }
        let version = u16_at(bytes, 4);
        if version != VERSION {
            return Err(ImageError::Header(HeaderError::Version { found: version }));
        }
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        if bytes[84..HEADER_BYTES] != [0; 4] {
            return Err(ImageError::Header(HeaderError::Reserved));
        }
        let declaration_count = usize::try_from(u32_at(bytes, 8)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len() - HEADER_BYTES,
            })
        })?;
        let atom_bytes = usize::try_from(u32_at(bytes, 12)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len() - HEADER_BYTES,
            })
        })?;
        let body_bytes = usize::try_from(u32_at(bytes, 16)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len() - HEADER_BYTES,
            })
        })?;
        let declaration_bytes =
            declaration_count
                .checked_mul(DECLARATION_BYTES)
                .ok_or(ImageError::Header(HeaderError::BodyLength {
                    declared: body_bytes,
                    actual: bytes.len() - HEADER_BYTES,
                }))?;
        let expected_body = declaration_bytes
            .checked_add(atom_bytes)
            .ok_or(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - HEADER_BYTES,
            }))?;
        if body_bytes != expected_body || bytes.len() != HEADER_BYTES + body_bytes {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - HEADER_BYTES,
            }));
        }
        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[20..52]);
        let image = Self {
            bytes,
            declaration_count,
            declaration_offset: HEADER_BYTES,
            atom_offset: HEADER_BYTES + declaration_bytes,
            atom_bytes,
            source_digest,
        };
        image.validate_digest()?;
        image.validate_declarations()?;
        Ok(image)
    }

    /// Returns the SHA-256 digest of the exact configured Go source file.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Iterates all package-scope declarations in producer order.
    #[must_use]
    pub const fn declarations(self) -> DeclarationIter<'image> {
        DeclarationIter {
            image: self,
            next: 0,
        }
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&self.bytes[..52]);
        digest.update(&self.bytes[84..HEADER_BYTES]);
        digest.update(&self.bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice() != &self.bytes[52..84] {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.declaration_count {
            self.declaration(index)?;
        }
        Ok(())
    }

    fn declaration(self, index: usize) -> Result<Declaration<'image>, ImageError> {
        let row = self.row(index);
        let kind = DeclarationKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
        })?;
        let exported = match row[1] {
            0 => false,
            1 => true,
            found => return Err(ImageError::ExportedFlag { index, found }),
        };
        if row[2..4] != [0; 2] {
            return Err(ImageError::DeclarationReserved { index });
        }
        let offset = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::NameRange {
            index,
            offset: usize::MAX,
            length: 0,
            atom_bytes: self.atom_bytes,
        })?;
        let length = usize::try_from(u32_at(row, 8)).map_err(|_| ImageError::NameRange {
            index,
            offset,
            length: usize::MAX,
            atom_bytes: self.atom_bytes,
        })?;
        if length == 0
            || offset
                .checked_add(length)
                .is_none_or(|end| end > self.atom_bytes)
        {
            return Err(ImageError::NameRange {
                index,
                offset,
                length,
                atom_bytes: self.atom_bytes,
            });
        }
        let name = &self.bytes[self.atom_offset + offset..self.atom_offset + offset + length];
        if str::from_utf8(name).is_err() {
            return Err(ImageError::NameUtf8 { index });
        }
        Ok(Declaration {
            kind,
            exported,
            name,
        })
    }

    fn row(self, index: usize) -> &'image [u8] {
        let start = self.declaration_offset + index * DECLARATION_BYTES;
        &self.bytes[start..start + DECLARATION_BYTES]
    }
}

/// Exact image rejection returned before a Go fact is admitted.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ImageError {
    /// The fixed binary envelope is invalid.
    #[error("invalid Go authority image header: {0}")]
    Header(#[from] HeaderError),
    /// The image checksum differs from its fixed header and body.
    #[error("Go authority image checksum does not match")]
    Digest,
    /// A declaration row has an unrecognized kind tag.
    #[error("Go authority declaration {index} has unknown kind tag {found}")]
    DeclarationKind { index: usize, found: u8 },
    /// A declaration row has an invalid closed exported flag.
    #[error("Go authority declaration {index} has invalid exported flag {found}")]
    ExportedFlag { index: usize, found: u8 },
    /// A declaration row claims non-zero reserved bits.
    #[error("Go authority declaration {index} has non-zero reserved bits")]
    DeclarationReserved { index: usize },
    /// A declaration name coordinate lies outside the atom plane.
    #[error(
        "Go authority declaration {index} name range {offset}..{length} exceeds atom bytes {atom_bytes}"
    )]
    NameRange {
        index: usize,
        offset: usize,
        length: usize,
        atom_bytes: usize,
    },
    /// A declaration name cannot be decoded as UTF-8.
    #[error("Go authority declaration {index} name is not UTF-8")]
    NameUtf8 { index: usize },
}

/// Exact fixed-header violation returned by [`GoImage::open`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HeaderError {
    /// Fewer than the complete fixed header bytes were supplied.
    #[error("truncated header: found {actual} bytes, need at least {HEADER_BYTES}")]
    Truncated { actual: usize },
    /// The four-byte image magic differs from `NGAI`.
    #[error("unexpected magic {found:?}")]
    Magic { found: [u8; 4] },
    /// The image version is not supported by this reader.
    #[error("unsupported image version {found}")]
    Version { found: u16 },
    /// The fixed header length does not match this image version.
    #[error("unexpected fixed header length {found}")]
    Length { found: usize },
    /// The body dimensions do not equal the supplied image body.
    #[error("declared body length {declared} differs from actual {actual}")]
    BodyLength { declared: usize, actual: usize },
    /// Reserved header bytes are not all zero.
    #[error("reserved header bytes are non-zero")]
    Reserved,
}

/// Exact-size iterator over validated borrowed Go declaration facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationIter<'image> {
    image: GoImage<'image>,
    next: usize,
}

impl<'image> Iterator for DeclarationIter<'image> {
    type Item = Result<Declaration<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.declaration_count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(self.image.declaration(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.image.declaration_count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DeclarationIter<'_> {
    fn len(&self) -> usize {
        self.image.declaration_count - self.next
    }
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
