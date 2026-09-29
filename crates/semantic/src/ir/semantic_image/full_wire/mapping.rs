//! Read-only mapping for complete, canonical semantic-image readers.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use memmap2::{Mmap, MmapOptions};
use thiserror::Error;

use crate::ir::{GenerationId, SemanticImageIdentity};
use backend_version::{ArtifactHasher, IrSemanticImageDomain, IrSemanticImageEncoding};

use super::{FullSemanticImageError, view::SemanticImageView};

/// One complete canonical image retained by a read-only operating-system mapping.
pub struct MappedSemanticImage {
    mapping: Mmap,
    proof: super::view::AdmittedSemanticImage,
    identity: SemanticImageIdentity,
    generation: GenerationId,
}

impl MappedSemanticImage {
    /// Typed content identity checked against the complete mapped bytes.
    #[must_use]
    pub const fn identity(&self) -> SemanticImageIdentity {
        self.identity
    }

    /// Canonical IR-VCS generation checked against the complete mapped bytes.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Borrows the validated full reader directly from the mapping.
    #[must_use]
    pub fn view(&self) -> SemanticImageView<'_> {
        SemanticImageView::reopen_proven(&self.mapping, self.proof)
            .expect("mapped image retains the exact immutable bytes admitted with its proof")
    }
}

/// Filesystem phase retaining its original I/O source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappedSemanticImageIoPhase {
    /// Opening the immutable content-addressed image.
    Open,
    /// Reading descriptor metadata to bound the mapping.
    Metadata,
    /// Creating the read-only operating-system mapping.
    Map,
    /// Copying file bytes into an anonymous mapped region.
    Read,
}

/// Failure before a complete image can lend a mapped semantic reader.
#[derive(Debug, Error)]
pub enum MappedSemanticImageError {
    /// One named filesystem operation retained its exact I/O cause.
    #[error("mapped semantic image {phase:?} failed")]
    Io {
        /// Exact filesystem phase.
        phase: MappedSemanticImageIoPhase,
        /// Original operating-system error.
        #[source]
        source: io::Error,
    },
    /// File metadata length cannot fit this process address space.
    #[error("mapped semantic image length {observed} exceeds this process address space")]
    FileLength {
        /// Exact descriptor byte length.
        observed: u64,
        /// Original checked conversion error.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// Zero-length files do not contain a complete image header.
    #[error("mapped semantic image file is empty")]
    EmptyFile,
    /// File length exceeds the caller's explicit mapping bound.
    #[error("mapped semantic image length {observed} exceeds the configured bound {max}")]
    FileTooLarge {
        /// Exact descriptor byte length.
        observed: u64,
        /// Caller-provided maximum byte length.
        max: usize,
    },
    /// Mapped bytes do not match the exact typed artifact identity.
    #[error("mapped semantic image artifact identity differs from its claim")]
    Identity {
        /// Expected artifact identity.
        expected: SemanticImageIdentity,
        /// Identity recomputed from the mapped bytes.
        observed: SemanticImageIdentity,
    },
    /// Mapped bytes do not match the exact canonical VCS generation.
    #[error("mapped semantic image generation differs from its claim")]
    Generation {
        /// Expected semantic generation.
        expected: GenerationId,
        /// Generation recomputed from the mapped bytes.
        observed: GenerationId,
    },
    /// Complete image bytes failed structural reader admission.
    #[error("mapped semantic image failed full grammar validation")]
    Grammar(#[source] FullSemanticImageError),
}

/// Opens a read-only mapping and admits its complete NXFI grammar and identities.
///
/// The exact file metadata length bounds the mapping. The returned owner keeps
/// the mapping and its structural proof together, and `view` borrows that same
/// immutable byte region without reparsing or copying it.
///
/// # Safety
///
/// `path` must name an immutable inode whose contents and length will not
/// change until the returned [`MappedSemanticImage`] is dropped. A read-only
/// mapping cannot prevent another descriptor or process from modifying or
/// truncating the file. Call this only for a content-addressed file published
/// atomically to a new path, and never modify that inode while the mapping lives.
#[allow(
    unsafe_code,
    reason = "the caller's immutable-inode contract, checked nonzero metadata length, read-only descriptor, and owner-held Mmap are verified at this single boundary"
)]
pub unsafe fn open_semantic_image_mmap(
    path: impl AsRef<Path>,
    expected_identity: SemanticImageIdentity,
    expected_generation: GenerationId,
) -> Result<MappedSemanticImage, MappedSemanticImageError> {
    let file = File::open(path).map_err(|source| MappedSemanticImageError::Io {
        phase: MappedSemanticImageIoPhase::Open,
        source,
    })?;
    let file_length = file
        .metadata()
        .map_err(|source| MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Metadata,
            source,
        })?
        .len();
    let mapping_length =
        usize::try_from(file_length).map_err(|source| MappedSemanticImageError::FileLength {
            observed: file_length,
            source,
        })?;
    if mapping_length == 0 {
        return Err(MappedSemanticImageError::EmptyFile);
    }
    // SAFETY: the caller upholds the documented immutable-inode contract;
    // this maps exactly the checked nonzero metadata length on a read-only
    // descriptor, and `MappedSemanticImage` owns the mapping for every borrow.
    let mapping =
        unsafe { MmapOptions::new().len(mapping_length).map(&file) }.map_err(|source| {
            MappedSemanticImageError::Io {
                phase: MappedSemanticImageIoPhase::Map,
                source,
            }
        })?;
    admit_mapping(
        mapping,
        mapping_length,
        expected_identity,
        expected_generation,
    )
}

/// Loads and validates a bounded full image into a read-only anonymous mapping.
///
/// This safe variant is for callers whose crate forbids unsafe code. It keeps
/// the complete image out of the heap and gives `SemanticImageView` stable
/// borrowed bytes, while performing one bounded disk-to-mapping copy per open.
pub fn load_semantic_image_mmap(
    path: impl AsRef<Path>,
    expected_identity: SemanticImageIdentity,
    expected_generation: GenerationId,
    max_bytes: usize,
) -> Result<MappedSemanticImage, MappedSemanticImageError> {
    let mut file = File::open(path).map_err(|source| MappedSemanticImageError::Io {
        phase: MappedSemanticImageIoPhase::Open,
        source,
    })?;
    let file_length = file
        .metadata()
        .map_err(|source| MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Metadata,
            source,
        })?
        .len();
    let mapping_length =
        usize::try_from(file_length).map_err(|source| MappedSemanticImageError::FileLength {
            observed: file_length,
            source,
        })?;
    if mapping_length == 0 {
        return Err(MappedSemanticImageError::EmptyFile);
    }
    if mapping_length > max_bytes {
        return Err(MappedSemanticImageError::FileTooLarge {
            observed: file_length,
            max: max_bytes,
        });
    }
    let mut writable = MmapOptions::new()
        .len(mapping_length)
        .map_anon()
        .map_err(|source| MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Map,
            source,
        })?;
    file.read_exact(&mut writable)
        .map_err(|source| MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Read,
            source,
        })?;
    let mapping = writable
        .make_read_only()
        .map_err(|source| MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Map,
            source,
        })?;
    admit_mapping(
        mapping,
        mapping_length,
        expected_identity,
        expected_generation,
    )
}

fn admit_mapping(
    mapping: Mmap,
    mapping_length: usize,
    expected_identity: SemanticImageIdentity,
    expected_generation: GenerationId,
) -> Result<MappedSemanticImage, MappedSemanticImageError> {
    let (observed_identity, observed_generation) = identities(&mapping);
    if observed_identity != expected_identity {
        return Err(MappedSemanticImageError::Identity {
            expected: expected_identity,
            observed: observed_identity,
        });
    }
    if observed_generation != expected_generation {
        return Err(MappedSemanticImageError::Generation {
            expected: expected_generation,
            observed: observed_generation,
        });
    }
    let view = SemanticImageView::reopen(&mapping).map_err(MappedSemanticImageError::Grammar)?;
    let proof = view.proof();
    drop(view);
    Ok(MappedSemanticImage {
        mapping,
        proof,
        identity: expected_identity,
        generation: expected_generation,
    })
}

fn identities(bytes: &[u8]) -> (SemanticImageIdentity, GenerationId) {
    let mut image_hasher = ArtifactHasher::<IrSemanticImageEncoding, IrSemanticImageDomain>::new();
    let mut generation_hasher = blake3::Hasher::new();
    for chunk in bytes.chunks(16 * 1024) {
        image_hasher.write_chunk(chunk);
        generation_hasher.update(chunk);
    }
    (
        image_hasher.finalize(),
        GenerationId::from_raw(*generation_hasher.finalize().as_bytes()),
    )
}
