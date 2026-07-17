//! Error types for `nudox-ir-archive`.
//!
//! [`ArchiveError`] covers all open/read failures. [`SealError`] covers
//! failures during [`crate::seal::seal_package_archive`].

use thiserror::Error;

// ---------------------------------------------------------------------------
// ArchiveError
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::open::open_archive`] and
/// [`crate::view::PackageArchiveView`] accessors.
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// The first 4 bytes are not `b"NdIr"`.
    #[error("bad magic bytes")]
    BadMagic,

    /// The format version is newer than this reader understands.
    #[error("unsupported format version {found}; max supported is {max}")]
    UnsupportedFormat { found: u16, max: u16 },

    /// A section's uncompressed byte length exceeds the hard cap.
    #[error("section {section:?} size {got} exceeds limit {max}")]
    SectionTooLarge { section: u32, got: u64, max: u64 },

    /// A CRC32 check failed (header or section body).
    #[error("CRC32 mismatch")]
    Crc,

    /// The byte slice ended before the expected region.
    #[error("truncated archive")]
    Truncated,

    /// A mandatory section id is not known by this reader (and was not flagged
    /// as OPTIONAL in the TOC flags field).
    #[error("unknown mandatory section id {0}")]
    UnknownSection(u32),

    /// A reserved header/TOC field that must be zero in this format version
    /// carries a nonzero value.
    #[error("reserved field nonzero: {0}")]
    ReservedNonzero(&'static str),

    /// An ArenaIdx is out of range for the EntryHead table.
    #[error("arena index {0} out of range (entry count {1})")]
    IndexOutOfRange(u32, u32),

    /// Postcard decode failed for a payload body.
    #[error("postcard decode error: {0}")]
    Postcard(postcard::Error),

    /// An I/O error (for future `mmap` / file-backed codepaths).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<postcard::Error> for ArchiveError {
    fn from(e: postcard::Error) -> Self {
        ArchiveError::Postcard(e)
    }
}
