//! Defines storage io behavior for `compiler-publication`, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the storage io invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Bounded read, hash, and directory-sync primitives for immutable transitions.

use std::{
    fs::File,
    io::{self, ErrorKind, Read},
    path::Path,
};

use heart_identity::{ArtifactHasher, ArtifactId, Domain, Encoding};

use super::{ImmutableFileError, ImmutableIoPhase};

const READ_CHUNK_BYTES: usize = 8 * 1024;

pub(super) fn hash_file<EncodingTag: Encoding, DomainTag: Domain>(
    file: &mut File,
    length: u32,
) -> Result<ArtifactId<EncodingTag, DomainTag>, ImmutableFileError<EncodingTag, DomainTag>> {
    let mut remaining =
        usize::try_from(length).map_err(|source| ImmutableFileError::LengthAddressSpace {
            observed: length,
            source,
        })?;
    let mut observed_bytes = 0_usize;
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    let mut hasher = ArtifactHasher::<EncodingTag, DomainTag>::new();
    while remaining != 0 {
        let requested = remaining.min(buffer.len());
        let read =
            file.read(&mut buffer[..requested])
                .map_err(|source| ImmutableFileError::Io {
                    phase: ImmutableIoPhase::ReadExisting,
                    source,
                })?;
        if read == 0 {
            return Err(ImmutableFileError::ExistingTruncated {
                expected: length,
                observed: observed_bytes,
                source: io::Error::from(ErrorKind::UnexpectedEof),
            });
        }
        hasher.write_chunk(&buffer[..read]);
        remaining -= read;
        observed_bytes += read;
    }
    let mut extra = [0_u8; 1];
    let extra_read = file
        .read(&mut extra)
        .map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExisting,
            source,
        })?;
    if extra_read != 0 {
        let observed = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        return Err(ImmutableFileError::ExistingLengthMismatch {
            expected: length,
            observed: observed.len(),
        });
    }
    Ok(hasher.finalize())
}

pub(super) fn read_file_into<EncodingTag: Encoding, DomainTag: Domain>(
    file: &mut File,
    output: &mut [u8],
    expected_length: u64,
) -> Result<ArtifactId<EncodingTag, DomainTag>, ImmutableFileError<EncodingTag, DomainTag>> {
    let mut hasher = ArtifactHasher::<EncodingTag, DomainTag>::new();
    let mut observed = 0_usize;
    while observed != output.len() {
        let read = file
            .read(&mut output[observed..])
            .map_err(|source| ImmutableFileError::Io {
                phase: ImmutableIoPhase::ReadExisting,
                source,
            })?;
        if read == 0 {
            return Err(ImmutableFileError::ExistingTruncated {
                expected: u32::try_from(expected_length).map_err(|source| {
                    ImmutableFileError::ExistingLengthAddressSpace {
                        observed: expected_length,
                        source,
                    }
                })?,
                observed,
                source: io::Error::from(ErrorKind::UnexpectedEof),
            });
        }
        hasher.write_chunk(&output[observed..observed + read]);
        observed += read;
    }
    let mut extra = [0_u8; 1];
    if file
        .read(&mut extra)
        .map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExisting,
            source,
        })?
        != 0
    {
        let metadata = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        return Err(ImmutableFileError::ExistingLengthMismatch {
            expected: u32::try_from(expected_length).map_err(|source| {
                ImmutableFileError::ExistingLengthAddressSpace {
                    observed: expected_length,
                    source,
                }
            })?,
            observed: metadata.len(),
        });
    }
    Ok(hasher.finalize())
}

pub(super) fn sync_directory(directory: &Path) -> Result<(), (ImmutableIoPhase, io::Error)> {
    let file = File::open(directory)
        .map_err(|source| (ImmutableIoPhase::OpenArtifactDirectory, source))?;
    file.sync_all()
        .map_err(|source| (ImmutableIoPhase::SyncArtifactDirectory, source))
}
