//! Defines storage transition behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the storage transition invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Shared single-owner filesystem transitions for typed immutable artifacts.

use std::{
    convert::TryFrom,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
};

use backend_version::{ArtifactId, Domain, Encoding};

use super::{
    ImmutableFileError, ImmutableIoPhase, StorageNamespace, StoredFile,
    io::{hash_file, read_file_into, sync_directory},
    namespace::artifact_name,
};

const TEMP_ATTEMPTS: u8 = 16;

type ExistingFile<EncodingTag, DomainTag> =
    Result<Option<StoredFile<EncodingTag, DomainTag>>, ImmutableFileError<EncodingTag, DomainTag>>;

/// Single-process owner for one typed immutable-artifact directory.
#[derive(Debug)]
pub(crate) struct ImmutableFileStore {
    directory: PathBuf,
    namespace: StorageNamespace,
    next_temporary: u64,
}

impl ImmutableFileStore {
    pub(crate) fn existing(parent: &Path, namespace: StorageNamespace) -> Self {
        Self {
            directory: parent.join(namespace.directory()),
            namespace,
            next_temporary: initial_nonce(),
        }
    }

    pub(crate) fn new<EncodingTag: Encoding, DomainTag: Domain>(
        parent: &Path,
        namespace: StorageNamespace,
    ) -> Result<Self, ImmutableFileError<EncodingTag, DomainTag>> {
        fs::create_dir_all(parent).map_err(|source| ImmutableFileError::Io {
            phase: ImmutableIoPhase::CreateArtifactDirectory,
            source,
        })?;
        let directory = parent.join(namespace.directory());
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
            namespace,
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
        let name = artifact_name(identity, self.namespace);
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
        let name = artifact_name(identity, self.namespace);
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

fn initial_nonce() -> u64 {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        });
    time ^ u64::from(std::process::id()).rotate_left(17)
}
