//! Read-only mapping for complete, canonical semantic-image readers.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use memmap2::{Mmap, MmapOptions};
use thiserror::Error;

use crate::ir::{GenerationId, SemanticImageIdentity};
use backend_version::{ArtifactHasher, IrSemanticImageDomain, IrSemanticImageEncoding};

use super::{FullSemanticImageError, view::SemanticImageView};

/// Maximum bytes requested from one exact-range image source at a time.
pub const SEMANTIC_IMAGE_MMAP_RANGE_BYTES: usize = 16 * 1024;

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

/// Counters for a successful exact-range image load into anonymous mapped memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappedSemanticImageRangeMetrics {
    /// Exact selected payload bytes copied into the mapping.
    pub bytes_read: u64,
    /// Exact payload bytes hashed while each range was copied into the mapping.
    pub identity_hash_bytes: u64,
    /// Number of bounded source-range calls used to fill the mapping.
    pub range_reads: u64,
}

/// Failure while loading the exact selected image through a bounded range source.
#[derive(Debug)]
pub enum MappedSemanticImageRangeError<E> {
    /// Mapping allocation or full image admission failed.
    Mapping(MappedSemanticImageError),
    /// The exact source range could not be read.
    Read { offset: u64, source: E },
    /// The source did not fill the requested exact range.
    ShortRead {
        /// Starting byte offset requested from the source.
        offset: u64,
        /// Exact range length requested.
        expected: usize,
        /// Number of bytes supplied by the source.
        observed: usize,
    },
    /// The selected generation changed while ranges were being loaded.
    Cancelled,
}

/// Loads a complete image into anonymous mapped memory using exact bounded ranges.
///
/// The source writes directly into the mapping's mutable slice: this path does
/// not build a payload-sized `Vec`. Every range must be filled exactly. The
/// final mapping is admitted against the selected image identity, canonical
/// generation, and complete semantic-image grammar before it is returned.
/// `is_cancelled` is checked before allocation, between each range, and before
/// full image admission so stale work stops before it publishes any proof.
pub fn load_semantic_image_mmap_from_ranges<E>(
    total_length: u64,
    expected_identity: SemanticImageIdentity,
    expected_generation: GenerationId,
    max_bytes: usize,
    mut read_range: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<(MappedSemanticImage, MappedSemanticImageRangeMetrics), MappedSemanticImageRangeError<E>>
{
    if is_cancelled() {
        return Err(MappedSemanticImageRangeError::Cancelled);
    }
    let mapping_length = usize::try_from(total_length).map_err(|source| {
        MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileLength {
            observed: total_length,
            source,
        })
    })?;
    if mapping_length == 0 {
        return Err(MappedSemanticImageRangeError::Mapping(
            MappedSemanticImageError::EmptyFile,
        ));
    }
    if mapping_length > max_bytes {
        return Err(MappedSemanticImageRangeError::Mapping(
            MappedSemanticImageError::FileTooLarge {
                observed: total_length,
                max: max_bytes,
            },
        ));
    }
    let mut writable = MmapOptions::new()
        .len(mapping_length)
        .map_anon()
        .map_err(|source| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::Io {
                phase: MappedSemanticImageIoPhase::Map,
                source,
            })
        })?;

    let mut offset = 0_u64;
    let mut range_reads = 0_u64;
    let mut identity_builder = SemanticImageIdentityBuilder::new();
    while offset < total_length {
        if is_cancelled() {
            return Err(MappedSemanticImageRangeError::Cancelled);
        }
        let remaining = total_length.checked_sub(offset).ok_or_else(|| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                observed: total_length,
                max: max_bytes,
            })
        })?;
        let count = usize::try_from(remaining.min(SEMANTIC_IMAGE_MMAP_RANGE_BYTES as u64))
            .map_err(|source| {
                MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileLength {
                    observed: remaining,
                    source,
                })
            })?;
        let start = usize::try_from(offset).map_err(|source| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileLength {
                observed: offset,
                source,
            })
        })?;
        let end = start.checked_add(count).ok_or_else(|| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                observed: total_length,
                max: max_bytes,
            })
        })?;
        let output = writable.get_mut(start..end).ok_or_else(|| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                observed: total_length,
                max: max_bytes,
            })
        })?;
        range_reads = range_reads.checked_add(1).ok_or_else(|| {
            MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                observed: total_length,
                max: max_bytes,
            })
        })?;
        let observed = read_range(offset, output)
            .map_err(|source| MappedSemanticImageRangeError::Read { offset, source })?;
        if observed != count {
            return Err(MappedSemanticImageRangeError::ShortRead {
                offset,
                expected: count,
                observed,
            });
        }
        identity_builder.update(output);
        offset = offset
            .checked_add(u64::try_from(count).map_err(|_| {
                MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                    observed: total_length,
                    max: max_bytes,
                })
            })?)
            .ok_or_else(|| {
                MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::FileTooLarge {
                    observed: total_length,
                    max: max_bytes,
                })
            })?;
    }
    if is_cancelled() {
        return Err(MappedSemanticImageRangeError::Cancelled);
    }

    let mapping = writable.make_read_only().map_err(|source| {
        MappedSemanticImageRangeError::Mapping(MappedSemanticImageError::Io {
            phase: MappedSemanticImageIoPhase::Map,
            source,
        })
    })?;
    let (observed_identity, observed_generation) = identity_builder.finish();
    let image = admit_mapping_with_identities(
        mapping,
        mapping_length,
        expected_identity,
        expected_generation,
        observed_identity,
        observed_generation,
    )
    .map_err(MappedSemanticImageRangeError::Mapping)?;
    Ok((
        image,
        MappedSemanticImageRangeMetrics {
            bytes_read: total_length,
            identity_hash_bytes: total_length,
            range_reads,
        },
    ))
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
    admit_mapping_with_identities(
        mapping,
        mapping_length,
        expected_identity,
        expected_generation,
        observed_identity,
        observed_generation,
    )
}

fn admit_mapping_with_identities(
    mapping: Mmap,
    mapping_length: usize,
    expected_identity: SemanticImageIdentity,
    expected_generation: GenerationId,
    observed_identity: SemanticImageIdentity,
    observed_generation: GenerationId,
) -> Result<MappedSemanticImage, MappedSemanticImageError> {
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

struct SemanticImageIdentityBuilder {
    image: ArtifactHasher<IrSemanticImageEncoding, IrSemanticImageDomain>,
    generation: blake3::Hasher,
}

impl SemanticImageIdentityBuilder {
    fn new() -> Self {
        Self {
            image: ArtifactHasher::<IrSemanticImageEncoding, IrSemanticImageDomain>::new(),
            generation: blake3::Hasher::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.image.write_chunk(bytes);
        self.generation.update(bytes);
    }

    fn finish(self) -> (SemanticImageIdentity, GenerationId) {
        (
            self.image.finalize(),
            GenerationId::from_raw(*self.generation.finalize().as_bytes()),
        )
    }
}

fn identities(bytes: &[u8]) -> (SemanticImageIdentity, GenerationId) {
    let mut identity_builder = SemanticImageIdentityBuilder::new();
    for chunk in bytes.chunks(16 * 1024) {
        identity_builder.update(chunk);
    }
    identity_builder.finish()
}
