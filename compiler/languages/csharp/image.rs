//! Validates the fixed Roslyn authority image used by direct compiler admission.
//! Lends source-bound declaration names and byte spans without JSON reconstruction.
//! Rejects malformed image coordinates before they can enter canonical facts.

use core::str;

use sha2::{Digest, Sha256};
use thiserror::Error;

const MAGIC: [u8; 4] = *b"NCAI";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 88;
const DECLARATION_BYTES: usize = 20;
const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v1\0";

/// A closed Roslyn type-declaration kind.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A class declaration.
    Class = 1,
    /// A struct declaration.
    Struct = 2,
    /// An interface declaration.
    Interface = 3,
    /// An enum declaration.
    Enum = 4,
    /// A delegate declaration.
    Delegate = 5,
    /// A record class declaration.
    Record = 6,
    /// A record struct declaration.
    RecordStruct = 7,
}

impl DeclarationKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Class),
            2 => Some(Self::Struct),
            3 => Some(Self::Interface),
            4 => Some(Self::Enum),
            5 => Some(Self::Delegate),
            6 => Some(Self::Record),
            7 => Some(Self::RecordStruct),
            _ => None,
        }
    }
}

/// One Roslyn-selected declaration with an exact byte span in the bound source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Declaration<'image> {
    /// Closed Roslyn declaration kind.
    pub kind: DeclarationKind,
    /// Exact UTF-8 identifier bytes from the binary atom plane.
    pub name: &'image [u8],
    /// Inclusive source byte coordinate of the declared identifier.
    pub start: u32,
    /// Exclusive source byte coordinate of the declared identifier.
    pub end: u32,
}

/// A validated immutable Roslyn image borrowing caller-owned image bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CSharpImage<'image> {
    bytes: &'image [u8],
    declaration_count: usize,
    declaration_offset: usize,
    atom_offset: usize,
    atom_bytes: usize,
    source_digest: [u8; 32],
}

impl<'image> CSharpImage<'image> {
    /// Opens a complete fixed-layout Roslyn authority image.
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
        if expected_body != body_bytes || bytes.len() != HEADER_BYTES + body_bytes {
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

    /// Returns the SHA-256 digest of the source Roslyn bound before emission.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Iterates all declarations attributed to the source-binding file.
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
        if row[1..4] != [0; 3] {
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
        let start = u32_at(row, 12);
        let end = u32_at(row, 16);
        if start > end {
            return Err(ImageError::Span { index, start, end });
        }
        let name = &self.bytes[self.atom_offset + offset..self.atom_offset + offset + length];
        if str::from_utf8(name).is_err() {
            return Err(ImageError::NameUtf8 { index });
        }
        Ok(Declaration {
            kind,
            name,
            start,
            end,
        })
    }

    fn row(self, index: usize) -> &'image [u8] {
        let start = self.declaration_offset + index * DECLARATION_BYTES;
        &self.bytes[start..start + DECLARATION_BYTES]
    }
}

/// Exact Roslyn image validation error retained through compiler terminals.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ImageError {
    /// Fixed envelope validation failed.
    #[error("invalid C# authority image header: {0}")]
    Header(#[from] HeaderError),
    /// The image digest does not cover its fixed header and body.
    #[error("C# authority image checksum does not match")]
    Digest,
    /// A declaration kind tag is outside the closed Roslyn vocabulary.
    #[error("C# authority declaration {index} has unknown kind tag {found}")]
    DeclarationKind {
        /// Zero-based declaration row whose kind tag was rejected.
        index: usize,
        /// Unrecognized raw kind tag.
        found: u8,
    },
    /// A declaration row names non-zero reserved bytes.
    #[error("C# authority declaration {index} has non-zero reserved bytes")]
    DeclarationReserved {
        /// Zero-based declaration row that has reserved bits set.
        index: usize,
    },
    /// A declaration name lies outside the image atom plane.
    #[error("C# authority declaration {index} name range is invalid")]
    NameRange {
        /// Zero-based declaration row naming the invalid atom range.
        index: usize,
        /// Atom-plane byte offset supplied by the row.
        offset: usize,
        /// Atom-plane byte length supplied by the row.
        length: usize,
        /// Complete atom-plane byte capacity.
        atom_bytes: usize,
    },
    /// A declaration identifier is not valid UTF-8.
    #[error("C# authority declaration {index} name is not UTF-8")]
    NameUtf8 {
        /// Zero-based declaration row whose name cannot be decoded as UTF-8.
        index: usize,
    },
    /// A declaration span is inverted.
    #[error("C# authority declaration {index} span {start}..{end} is inverted")]
    Span {
        /// Zero-based declaration row with an inverted span.
        index: usize,
        /// Inclusive byte coordinate supplied by the row.
        start: u32,
        /// Exclusive byte coordinate supplied by the row.
        end: u32,
    },
}

/// Exact fixed-envelope error returned by [`CSharpImage::open`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HeaderError {
    /// The supplied image ends before the complete fixed header.
    #[error("truncated header: found {actual} bytes, need at least {HEADER_BYTES}")]
    Truncated {
        /// Number of image bytes made available to the decoder.
        actual: usize,
    },
    /// The image magic differs from `NCAI`.
    #[error("unexpected magic {found:?}")]
    Magic {
        /// Raw four-byte magic found in the image header.
        found: [u8; 4],
    },
    /// The image names an unsupported wire version.
    #[error("unsupported image version {found}")]
    Version {
        /// Unsupported raw image version.
        found: u16,
    },
    /// The header length differs from this version's fixed width.
    #[error("unexpected fixed header length {found}")]
    Length {
        /// Fixed-header length encoded by the image.
        found: usize,
    },
    /// Declared body dimensions differ from the supplied bytes.
    #[error("declared body length {declared} differs from actual {actual}")]
    BodyLength {
        /// Body length declared by the fixed header.
        declared: usize,
        /// Body length available after the fixed header.
        actual: usize,
    },
    /// Reserved header bytes are non-zero.
    #[error("reserved header bytes are non-zero")]
    Reserved,
}

/// Exact-size iterator over borrowed C# declaration rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationIter<'image> {
    image: CSharpImage<'image>,
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
