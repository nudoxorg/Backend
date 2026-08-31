//! Shared single-owner filesystem transitions for typed immutable artifacts.

use std::{
    convert::TryFrom,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    num::TryFromIntError,
    path::{Path, PathBuf},
    str::Utf8Error,
};

use nudox_id::{ArtifactHasher, ArtifactId, Domain, Encoding, HASH_BYTES};
use thiserror::Error;

const READ_CHUNK_BYTES: usize = 8 * 1024;
const TEMP_ATTEMPTS: u8 = 16;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const HEX_NAME_BYTES: usize = HASH_BYTES * 2;
const ARTIFACT_EXTENSION_BYTES: usize = 7;
const ARTIFACT_NAME_BYTES: usize = HEX_NAME_BYTES + ARTIFACT_EXTENSION_BYTES;

/// Exact filesystem transition whose source is retained by [`ImmutableFileError::Io`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImmutableIoPhase {
    /// Create the sibling immutable-artifact directory.
    CreateArtifactDirectory,
    /// Open the sibling immutable-artifact directory for a durability barrier.
    OpenArtifactDirectory,
    /// Sync the sibling immutable-artifact directory after a namespace transition.
    SyncArtifactDirectory,
    /// Open an existing identity path for validation.
    OpenExisting,
    /// Read metadata for an existing identity path.
    ReadExistingMetadata,
    /// Read bytes from an existing identity path.
    ReadExisting,
    /// Create a same-directory temporary artifact without replacing a prior temporary file.
    CreateTemporary,
    /// Write caller-owned bytes to a temporary artifact.
    WriteTemporary,
    /// Sync temporary artifact bytes before publication.
    SyncTemporary,
    /// Publish a temporary artifact under its immutable identity path.
    PublishTemporary,
    /// Remove a temporary path after a failed transition.
    RemoveTemporary,
}

/// Typed filesystem rejection while storing immutable canonical bytes.
#[derive(Debug, Error)]
#[allow(
    missing_docs,
    reason = "each field repeats the exact typed fact documented by its enclosing filesystem terminal"
)]
pub enum ImmutableFileError<EncodingTag: Encoding, DomainTag: Domain> {
    /// A filesystem operation failed at one exact storage phase.
    #[error("immutable artifact I/O failed during {phase:?}")]
    Io {
        phase: ImmutableIoPhase,
        #[source]
        source: io::Error,
    },
    /// A failed temporary transition and its cleanup failure are both retained.
    #[error(
        "immutable artifact I/O failed during {phase:?}; cleanup failed during {cleanup_phase:?}"
    )]
    IoWithCleanup {
        phase: ImmutableIoPhase,
        #[source]
        source: io::Error,
        cleanup_phase: ImmutableIoPhase,
        cleanup_source: io::Error,
    },
    /// A final identity path already exists with a different byte length.
    #[error("existing immutable artifact has length {observed}, expected {expected}")]
    ExistingLengthMismatch { expected: u32, observed: u64 },
    /// A final identity path already exists with different bytes.
    #[error("existing immutable artifact identity differs from the requested identity")]
    ExistingIdentityMismatch {
        expected: ArtifactId<EncodingTag, DomainTag>,
        observed: ArtifactId<EncodingTag, DomainTag>,
    },
    /// Existing metadata claimed the expected length but the file ended early while being read.
    #[error("existing immutable artifact ended after {observed} bytes, expected {expected}")]
    ExistingTruncated {
        expected: u32,
        observed: usize,
        #[source]
        source: io::Error,
    },
    /// Every bounded temporary-name attempt collided with an existing path.
    #[error("could not allocate a temporary immutable-artifact name after {attempts} attempts")]
    TemporaryNamesExhausted {
        attempts: u8,
        #[source]
        source: io::Error,
    },
    /// The fixed generated artifact name unexpectedly was not UTF-8.
    #[error("generated immutable artifact name is not valid UTF-8")]
    InvalidArtifactName {
        #[source]
        source: Utf8Error,
    },
    /// The platform could not represent a claimed u32 length as a native buffer coordinate.
    #[error("artifact byte length {observed} cannot be represented in this address space")]
    LengthAddressSpace {
        observed: u32,
        #[source]
        source: TryFromIntError,
    },
    /// Caller bytes did not have the exact typed artifact length claimed before storage.
    #[error("candidate immutable artifact has {observed} bytes, expected {expected}")]
    InputLengthMismatch { expected: u32, observed: usize },
    /// Caller bytes did not satisfy the typed immutable identity claimed before storage.
    #[error("candidate immutable artifact identity differs from its typed claim")]
    InputIdentityMismatch {
        expected: ArtifactId<EncodingTag, DomainTag>,
        observed: ArtifactId<EncodingTag, DomainTag>,
    },
    /// Existing artifact length cannot fit this process's caller-owned read buffer coordinate.
    #[error("stored immutable artifact length {observed} cannot fit this address space")]
    ExistingLengthAddressSpace {
        observed: u64,
        #[source]
        source: TryFromIntError,
    },
    /// Caller output cannot hold one complete existing immutable artifact.
    #[error("immutable artifact output has {available} bytes, requires {required}")]
    ReadOutputTooSmall { required: usize, available: usize },
}

/// Location and immutable facts of a stored artifact with a typed encoding/domain identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredFile<EncodingTag, DomainTag> {
    pub(crate) identity: ArtifactId<EncodingTag, DomainTag>,
    pub(crate) length: u32,
    pub(crate) path: PathBuf,
}

type ExistingFile<EncodingTag, DomainTag> =
    Result<Option<StoredFile<EncodingTag, DomainTag>>, ImmutableFileError<EncodingTag, DomainTag>>;

/// Single-process owner for one typed immutable-artifact directory.
#[derive(Debug)]
pub(crate) struct ImmutableFileStore {
    directory: PathBuf,
    extension: &'static [u8; ARTIFACT_EXTENSION_BYTES],
    next_temporary: u64,
}

impl ImmutableFileStore {
    pub(crate) fn existing(
        parent: &Path,
        directory_name: &'static str,
        extension: &'static [u8; ARTIFACT_EXTENSION_BYTES],
    ) -> Self {
        Self {
            directory: parent.join(directory_name),
            extension,
            next_temporary: initial_nonce(),
        }
    }

    pub(crate) fn new<EncodingTag: Encoding, DomainTag: Domain>(
        parent: &Path,
        directory_name: &'static str,
        extension: &'static [u8; ARTIFACT_EXTENSION_BYTES],
    ) -> Result<Self, ImmutableFileError<EncodingTag, DomainTag>> {
        fs::create_dir_all(parent).map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::CreateArtifactDirectory,
            source,
        })?;
        let directory = parent.join(directory_name);
        match fs::create_dir(&directory) {
            Ok(()) => sync_directory(&directory)
                .map_err(|(phase, source)| ImmutableFileError::Io { phase, source })?,
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(ImmutableFileError::Io {
                    phase: ImmutableIoPhase::CreateArtifactDirectory,
                    source,
                });
            }
        }
        Ok(Self {
            directory,
            extension,
            next_temporary: initial_nonce(),
        })
    }

    pub(crate) fn ensure<EncodingTag: Encoding, DomainTag: Domain>(
        &mut self,
        identity: ArtifactId<EncodingTag, DomainTag>,
        length: u32,
        bytes: &[u8],
    ) -> Result<StoredFile<EncodingTag, DomainTag>, ImmutableFileError<EncodingTag, DomainTag>>
    {
        if bytes.len()
            != usize::try_from(length).map_err(|source| ImmutableFileError::LengthAddressSpace {
                observed: length,
                source,
            })?
        {
            return Err(ImmutableFileError::InputLengthMismatch {
                expected: length,
                observed: bytes.len(),
            });
        }
        let observed = ArtifactId::<EncodingTag, DomainTag>::from_encoded_bytes(bytes);
        if observed != identity {
            return Err(ImmutableFileError::InputIdentityMismatch {
                expected: identity,
                observed,
            });
        }
        let final_path = self.artifact_path(identity)?;
        if let Some(stored) = self.inspect_existing(&final_path, identity, length)? {
            return Ok(stored);
        }
        self.publish_missing(final_path, identity, length, bytes)
    }

    pub(crate) fn read_into<'output, EncodingTag: Encoding, DomainTag: Domain>(
        &self,
        identity: ArtifactId<EncodingTag, DomainTag>,
        expected_length: Option<u32>,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], ImmutableFileError<EncodingTag, DomainTag>> {
        let final_path = self.artifact_path(identity)?;
        let mut file = File::open(&final_path).map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::OpenExisting,
            source,
        })?;
        let metadata = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        if let Some(expected) = expected_length
            && metadata.len() != u64::from(expected)
        {
            return Err(ImmutableFileError::ExistingLengthMismatch {
                expected,
                observed: metadata.len(),
            });
        }
        let length = usize::try_from(metadata.len()).map_err(|source| {
            ImmutableFileError::ExistingLengthAddressSpace {
                observed: metadata.len(),
                source,
            }
        })?;
        if output.len() < length {
            return Err(ImmutableFileError::ReadOutputTooSmall {
                required: length,
                available: output.len(),
            });
        }
        let output = &mut output[..length];
        let observed = read_file_into::<EncodingTag, DomainTag>(&mut file, output, metadata.len())?;
        if observed != identity {
            return Err(ImmutableFileError::ExistingIdentityMismatch {
                expected: identity,
                observed,
            });
        }
        let post_read = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        if post_read.len() != metadata.len() {
            return Err(ImmutableFileError::ExistingLengthMismatch {
                expected: u32::try_from(metadata.len()).map_err(|source| {
                    ImmutableFileError::ExistingLengthAddressSpace {
                        observed: metadata.len(),
                        source,
                    }
                })?,
                observed: post_read.len(),
            });
        }
        Ok(output)
    }

    fn artifact_path<EncodingTag: Encoding, DomainTag: Domain>(
        &self,
        identity: ArtifactId<EncodingTag, DomainTag>,
    ) -> Result<PathBuf, ImmutableFileError<EncodingTag, DomainTag>> {
        let name = artifact_name(identity, self.extension);
        let name = core::str::from_utf8(&name)
            .map_err(|source| ImmutableFileError::InvalidArtifactName { source })?;
        Ok(self.directory.join(name))
    }

    fn inspect_existing<EncodingTag: Encoding, DomainTag: Domain>(
        &self,
        final_path: &Path,
        identity: ArtifactId<EncodingTag, DomainTag>,
        length: u32,
    ) -> ExistingFile<EncodingTag, DomainTag> {
        let mut file = match File::open(final_path) {
            Ok(file) => file,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ImmutableFileError::Io {
                    phase: ImmutableIoPhase::OpenExisting,
                    source,
                });
            }
        };
        let metadata = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        if metadata.len() != u64::from(length) {
            return Err(ImmutableFileError::ExistingLengthMismatch {
                expected: length,
                observed: metadata.len(),
            });
        }
        let observed = hash_file::<EncodingTag, DomainTag>(&mut file, length)?;
        if observed != identity {
            return Err(ImmutableFileError::ExistingIdentityMismatch {
                expected: identity,
                observed,
            });
        }
        let post_read = file.metadata().map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::ReadExistingMetadata,
            source,
        })?;
        if post_read.len() != u64::from(length) {
            return Err(ImmutableFileError::ExistingLengthMismatch {
                expected: length,
                observed: post_read.len(),
            });
        }
        Ok(Some(StoredFile {
            identity,
            length,
            path: final_path.to_path_buf(),
        }))
    }

    fn publish_missing<EncodingTag: Encoding, DomainTag: Domain>(
        &mut self,
        final_path: PathBuf,
        identity: ArtifactId<EncodingTag, DomainTag>,
        length: u32,
        bytes: &[u8],
    ) -> Result<StoredFile<EncodingTag, DomainTag>, ImmutableFileError<EncodingTag, DomainTag>>
    {
        let mut last_collision = io::Error::from(ErrorKind::AlreadyExists);
        for _attempt in 0..TEMP_ATTEMPTS {
            let nonce = self.next_temporary;
            self.next_temporary = self.next_temporary.wrapping_add(1);
            let temporary_path = self.temporary_path(identity, nonce)?;
            let mut temporary = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
            {
                Ok(file) => file,
                Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                    last_collision = source;
                    continue;
                }
                Err(source) => {
                    return Err(ImmutableFileError::Io {
                        phase: ImmutableIoPhase::CreateTemporary,
                        source,
                    });
                }
            };
            if let Err(source) = temporary.write_all(bytes) {
                return Err(self.with_cleanup(
                    ImmutableIoPhase::WriteTemporary,
                    source,
                    &temporary_path,
                ));
            }
            if let Err(source) = temporary.sync_all() {
                return Err(self.with_cleanup(
                    ImmutableIoPhase::SyncTemporary,
                    source,
                    &temporary_path,
                ));
            }
            drop(temporary);
            return self.publish_temporary(temporary_path, final_path, identity, length);
        }
        Err(ImmutableFileError::TemporaryNamesExhausted {
            attempts: TEMP_ATTEMPTS,
            source: last_collision,
        })
    }

    fn publish_temporary<EncodingTag: Encoding, DomainTag: Domain>(
        &mut self,
        temporary_path: PathBuf,
        final_path: PathBuf,
        identity: ArtifactId<EncodingTag, DomainTag>,
        length: u32,
    ) -> Result<StoredFile<EncodingTag, DomainTag>, ImmutableFileError<EncodingTag, DomainTag>>
    {
        match fs::rename(&temporary_path, &final_path) {
            Ok(()) => {
                sync_directory(&self.directory)
                    .map_err(|(phase, source)| ImmutableFileError::Io { phase, source })?;
                Ok(StoredFile {
                    identity,
                    length,
                    path: final_path,
                })
            }
            Err(source) => {
                Err(self.with_cleanup(ImmutableIoPhase::PublishTemporary, source, &temporary_path))
            }
        }
    }

    fn temporary_path<EncodingTag: Encoding, DomainTag: Domain>(
        &self,
        identity: ArtifactId<EncodingTag, DomainTag>,
        nonce: u64,
    ) -> Result<PathBuf, ImmutableFileError<EncodingTag, DomainTag>> {
        let name = artifact_name(identity, self.extension);
        let name = core::str::from_utf8(&name)
            .map_err(|source| ImmutableFileError::InvalidArtifactName { source })?;
        Ok(self.directory.join(format!("{name}.tmp.{nonce}")))
    }

    fn with_cleanup<EncodingTag: Encoding, DomainTag: Domain>(
        &self,
        phase: ImmutableIoPhase,
        source: io::Error,
        temporary_path: &Path,
    ) -> ImmutableFileError<EncodingTag, DomainTag> {
        match self.cleanup_temporary(temporary_path) {
            Ok(()) => ImmutableFileError::Io { phase, source },
            Err((cleanup_phase, cleanup_source)) => ImmutableFileError::IoWithCleanup {
                phase,
                source,
                cleanup_phase,
                cleanup_source,
            },
        }
    }

    fn cleanup_temporary(
        &self,
        temporary_path: &Path,
    ) -> Result<(), (ImmutableIoPhase, io::Error)> {
        fs::remove_file(temporary_path)
            .map_err(|source| (ImmutableIoPhase::RemoveTemporary, source))?;
        sync_directory(&self.directory)
    }
}

fn hash_file<EncodingTag: Encoding, DomainTag: Domain>(
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

fn read_file_into<EncodingTag: Encoding, DomainTag: Domain>(
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

fn sync_directory(directory: &Path) -> Result<(), (ImmutableIoPhase, io::Error)> {
    let file = File::open(directory)
        .map_err(|source| (ImmutableIoPhase::OpenArtifactDirectory, source))?;
    file.sync_all()
        .map_err(|source| (ImmutableIoPhase::SyncArtifactDirectory, source))
}

fn artifact_name<EncodingTag, DomainTag>(
    identity: ArtifactId<EncodingTag, DomainTag>,
    extension: &[u8; ARTIFACT_EXTENSION_BYTES],
) -> [u8; ARTIFACT_NAME_BYTES] {
    let raw: &[u8; HASH_BYTES] = identity.as_ref();
    let mut name = [0_u8; ARTIFACT_NAME_BYTES];
    for (index, byte) in raw.iter().copied().enumerate() {
        name[index * 2] = HEX_DIGITS[usize::from(byte >> 4)];
        name[index * 2 + 1] = HEX_DIGITS[usize::from(byte & 0x0f)];
    }
    name[HEX_NAME_BYTES..].copy_from_slice(extension);
    name
}

fn initial_nonce() -> u64 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    time ^ u64::from(std::process::id()).rotate_left(17)
}
