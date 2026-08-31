//! Race-safe storage for validated immutable IR fragment bytes.
//!
//! The caller owns semantic validation. This adapter only accepts a complete byte stream together
//! with its typed artifact identity and exact length, rechecks both before any write, and then
//! makes the identity path durable. Existing identity paths are independently revalidated before
//! reuse; an immutable identity is never overwritten.

use std::{
    convert::TryFrom,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    num::TryFromIntError,
    path::{Path, PathBuf},
    str::Utf8Error,
    sync::atomic::{AtomicU64, Ordering},
};

use nudox_id::{ArtifactHasher, ArtifactId, HASH_BYTES, IrFragmentDomain, IrFragmentEncoding};
use thiserror::Error;

/// Typed identity used for complete canonical IR fragment artifacts.
pub type FragmentIdentity = ArtifactId<IrFragmentEncoding, IrFragmentDomain>;

const ARTIFACT_DIRECTORY: &str = "fragments";
const ARTIFACT_EXTENSION: &str = ".irfrag";
const READ_CHUNK_BYTES: usize = 8 * 1024;
const TEMP_ATTEMPTS: u8 = 16;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const HEX_NAME_BYTES: usize = HASH_BYTES * 2;
const ARTIFACT_NAME_BYTES: usize = HEX_NAME_BYTES + ARTIFACT_EXTENSION.len();

/// Exact filesystem transition whose source is retained in [`ImmutableArtifactError::Io`].
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
    /// Remove a temporary path after its immutable link is durable.
    RemoveTemporary,
}

/// Failure while validating or durably storing one complete immutable artifact.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImmutableArtifactError {
    /// The caller-provided byte length cannot be represented by the protocol coordinate.
    #[error("artifact byte length {observed} cannot be represented as u32")]
    InputLengthAddressSpace {
        /// Native byte length observed before any filesystem write.
        observed: usize,
        /// Checked conversion source.
        #[source]
        source: TryFromIntError,
    },
    /// The caller's exact length claim differs from the supplied bytes.
    #[error("artifact byte length claim is {expected}, observed {observed}")]
    InputLengthMismatch {
        /// Claimed protocol length.
        expected: u32,
        /// Native length converted to the protocol coordinate.
        observed: u32,
    },
    /// The caller's typed identity does not commit to the supplied bytes.
    #[error("artifact identity does not commit to the supplied bytes")]
    InputIdentityMismatch {
        /// Caller-supplied identity retained on rejection.
        expected: FragmentIdentity,
        /// Identity calculated from the supplied bytes.
        observed: FragmentIdentity,
    },
    /// A filesystem operation failed at one exact storage phase.
    #[error("immutable artifact I/O failed during {phase:?}")]
    Io {
        /// Exact filesystem phase.
        phase: ImmutableIoPhase,
        /// Original filesystem source.
        #[source]
        source: io::Error,
    },
    /// A failed temporary transition and its cleanup failure are both retained.
    #[error(
        "immutable artifact I/O failed during {phase:?}; cleanup failed during {cleanup_phase:?}"
    )]
    IoWithCleanup {
        /// Primary filesystem phase.
        phase: ImmutableIoPhase,
        /// Primary filesystem source.
        #[source]
        source: io::Error,
        /// Cleanup filesystem phase.
        cleanup_phase: ImmutableIoPhase,
        /// Cleanup filesystem source.
        cleanup_source: io::Error,
    },
    /// A final identity path already exists with a different byte length.
    #[error("existing immutable artifact has length {observed}, expected {expected}")]
    ExistingLengthMismatch {
        /// Expected immutable byte length.
        expected: u32,
        /// Existing file metadata length.
        observed: u64,
    },
    /// A final identity path already exists with different bytes.
    #[error("existing immutable artifact identity differs from the requested identity")]
    ExistingIdentityMismatch {
        /// Requested immutable identity.
        expected: FragmentIdentity,
        /// Identity calculated from the existing bytes.
        observed: FragmentIdentity,
    },
    /// Existing metadata claimed the expected length but the file ended early while being read.
    #[error("existing immutable artifact ended after {observed} bytes, expected {expected}")]
    ExistingTruncated {
        /// Expected immutable byte length.
        expected: u32,
        /// Number of bytes actually read.
        observed: usize,
        /// Synthetic unexpected-EOF cause retaining the failed read condition.
        #[source]
        source: io::Error,
    },
    /// Every bounded temporary-name attempt collided with an existing path.
    #[error("could not allocate a temporary immutable-artifact name after {attempts} attempts")]
    TemporaryNamesExhausted {
        /// Number of bounded attempts made.
        attempts: u8,
        /// Last collision source.
        #[source]
        source: io::Error,
    },
    /// The fixed generated artifact name unexpectedly was not UTF-8.
    #[error("generated immutable artifact name is not valid UTF-8")]
    InvalidArtifactName {
        /// Conversion source retained for diagnostics.
        #[source]
        source: Utf8Error,
    },
    /// The platform could not represent a claimed u32 length as a native buffer coordinate.
    #[error("artifact byte length {observed} cannot be represented in this address space")]
    LengthAddressSpace {
        /// Claimed protocol length.
        observed: u32,
        /// Checked conversion source.
        #[source]
        source: TryFromIntError,
    },
}

/// Location and immutable facts of one stored artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredArtifact {
    /// Typed identity of the complete canonical bytes.
    pub identity: FragmentIdentity,
    /// Exact byte length committed by the identity path.
    pub length: u32,
    /// Safe fixed-name path of the immutable artifact.
    pub path: PathBuf,
}

/// Filesystem owner for one sibling immutable-fragment directory.
///
/// The owner contains only an atomic temporary-name nonce; no lock, asynchronous worker, or
/// per-operation heap owner is needed. Multiple callers may race on one identity: finalization
/// uses a no-clobber hard link and the winner's bytes are independently validated by every loser.
#[derive(Debug)]
pub struct ImmutableArtifactStore {
    fragments: PathBuf,
    next_temporary: AtomicU64,
}

impl ImmutableArtifactStore {
    /// Creates the sibling immutable-fragment directory and syncs its namespace.
    pub fn new(directory: &Path) -> Result<Self, ImmutableArtifactError> {
        let fragments = directory.join(ARTIFACT_DIRECTORY);
        fs::create_dir_all(&fragments).map_err(|source| ImmutableArtifactError::Io {
            phase: ImmutableIoPhase::CreateArtifactDirectory,
            source,
        })?;
        sync_directory(&fragments)?;
        Ok(Self {
            fragments,
            next_temporary: AtomicU64::new(initial_nonce()),
        })
    }

    /// Ensures one caller-validated complete fragment is durably available by its typed identity.
    ///
    /// Length and identity are checked before probing or writing any artifact. A missing path is
    /// staged in the same directory, file-synced, linked without replacing an existing identity,
    /// and directory-synced. An existing path is streamed and hashed before reuse.
    pub fn ensure(
        &self,
        identity: FragmentIdentity,
        length: u32,
        bytes: &[u8],
    ) -> Result<StoredArtifact, ImmutableArtifactError> {
        validate_input(identity, length, bytes)?;
        let final_path = self.artifact_path(identity)?;
        if let Some(stored) = self.inspect_existing(&final_path, identity, length)? {
            return Ok(stored);
        }
        self.publish_missing(final_path, identity, length, bytes)
    }

    fn artifact_path(&self, identity: FragmentIdentity) -> Result<PathBuf, ImmutableArtifactError> {
        let name = artifact_name(identity);
        let name = core::str::from_utf8(&name)
            .map_err(|source| ImmutableArtifactError::InvalidArtifactName { source })?;
        Ok(self.fragments.join(name))
    }

    fn inspect_existing(
        &self,
        final_path: &Path,
        identity: FragmentIdentity,
        length: u32,
    ) -> Result<Option<StoredArtifact>, ImmutableArtifactError> {
        let mut file = match File::open(final_path) {
            Ok(file) => file,
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ImmutableArtifactError::Io {
                    phase: ImmutableIoPhase::OpenExisting,
                    source,
                });
            }
        };
        let metadata = file
            .metadata()
            .map_err(|source| ImmutableArtifactError::Io {
                phase: ImmutableIoPhase::ReadExistingMetadata,
                source,
            })?;
        let observed_length = metadata.len();
        if observed_length != u64::from(length) {
            return Err(ImmutableArtifactError::ExistingLengthMismatch {
                expected: length,
                observed: observed_length,
            });
        }
        let observed = hash_file(&mut file, length)?;
        if observed != identity {
            return Err(ImmutableArtifactError::ExistingIdentityMismatch {
                expected: identity,
                observed,
            });
        }
        let post_read_metadata = file
            .metadata()
            .map_err(|source| ImmutableArtifactError::Io {
                phase: ImmutableIoPhase::ReadExistingMetadata,
                source,
            })?;
        if post_read_metadata.len() != u64::from(length) {
            return Err(ImmutableArtifactError::ExistingLengthMismatch {
                expected: length,
                observed: post_read_metadata.len(),
            });
        }
        sync_directory(&self.fragments)?;
        Ok(Some(StoredArtifact {
            identity,
            length,
            path: final_path.to_path_buf(),
        }))
    }

    fn publish_missing(
        &self,
        final_path: PathBuf,
        identity: FragmentIdentity,
        length: u32,
        bytes: &[u8],
    ) -> Result<StoredArtifact, ImmutableArtifactError> {
        let mut last_collision = io::Error::from(ErrorKind::AlreadyExists);
        for _attempt in 0..TEMP_ATTEMPTS {
            let nonce = self.next_temporary.fetch_add(1, Ordering::Relaxed);
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
                    return Err(ImmutableArtifactError::Io {
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
        Err(ImmutableArtifactError::TemporaryNamesExhausted {
            attempts: TEMP_ATTEMPTS,
            source: last_collision,
        })
    }

    fn publish_temporary(
        &self,
        temporary_path: PathBuf,
        final_path: PathBuf,
        identity: FragmentIdentity,
        length: u32,
    ) -> Result<StoredArtifact, ImmutableArtifactError> {
        match fs::hard_link(&temporary_path, &final_path) {
            Ok(()) => {
                if let Err(error) = sync_directory(&self.fragments) {
                    return Err(self.with_cleanup_from_error(error, &temporary_path));
                }
                fs::remove_file(&temporary_path).map_err(|source| ImmutableArtifactError::Io {
                    phase: ImmutableIoPhase::RemoveTemporary,
                    source,
                })?;
                sync_directory(&self.fragments)?;
                Ok(StoredArtifact {
                    identity,
                    length,
                    path: final_path,
                })
            }
            Err(source) if source.kind() == ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary_path).map_err(|remove_source| {
                    ImmutableArtifactError::Io {
                        phase: ImmutableIoPhase::RemoveTemporary,
                        source: remove_source,
                    }
                })?;
                sync_directory(&self.fragments)?;
                self.inspect_existing(&final_path, identity, length)?
                    .ok_or_else(|| ImmutableArtifactError::Io {
                        phase: ImmutableIoPhase::OpenExisting,
                        source: io::Error::new(
                            ErrorKind::NotFound,
                            "immutable identity disappeared after no-clobber publication race",
                        ),
                    })
            }
            Err(source) => {
                Err(self.with_cleanup(ImmutableIoPhase::PublishTemporary, source, &temporary_path))
            }
        }
    }

    fn temporary_path(
        &self,
        identity: FragmentIdentity,
        nonce: u64,
    ) -> Result<PathBuf, ImmutableArtifactError> {
        let name = artifact_name(identity);
        let name = core::str::from_utf8(&name)
            .map_err(|source| ImmutableArtifactError::InvalidArtifactName { source })?;
        Ok(self.fragments.join(format!("{name}.tmp.{nonce}")))
    }

    fn with_cleanup(
        &self,
        phase: ImmutableIoPhase,
        source: io::Error,
        temporary_path: &Path,
    ) -> ImmutableArtifactError {
        match self.cleanup_temporary(temporary_path) {
            Ok(()) => ImmutableArtifactError::Io { phase, source },
            Err((cleanup_phase, cleanup_source)) => ImmutableArtifactError::IoWithCleanup {
                phase,
                source,
                cleanup_phase,
                cleanup_source,
            },
        }
    }

    fn with_cleanup_from_error(
        &self,
        error: ImmutableArtifactError,
        temporary_path: &Path,
    ) -> ImmutableArtifactError {
        let ImmutableArtifactError::Io { phase, source } = error else {
            return error;
        };
        match self.cleanup_temporary(temporary_path) {
            Ok(()) => ImmutableArtifactError::Io { phase, source },
            Err((cleanup_phase, cleanup_source)) => ImmutableArtifactError::IoWithCleanup {
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
        sync_directory(&self.fragments).map_err(|error| match error {
            ImmutableArtifactError::Io { phase, source } => (phase, source),
            _ => (
                ImmutableIoPhase::SyncArtifactDirectory,
                io::Error::other("unexpected directory-sync error"),
            ),
        })
    }
}

fn validate_input(
    identity: FragmentIdentity,
    length: u32,
    bytes: &[u8],
) -> Result<(), ImmutableArtifactError> {
    let observed_length = u32::try_from(bytes.len()).map_err(|source| {
        ImmutableArtifactError::InputLengthAddressSpace {
            observed: bytes.len(),
            source,
        }
    })?;
    if observed_length != length {
        return Err(ImmutableArtifactError::InputLengthMismatch {
            expected: length,
            observed: observed_length,
        });
    }
    let observed = FragmentIdentity::from_encoded_bytes(bytes);
    if observed != identity {
        return Err(ImmutableArtifactError::InputIdentityMismatch {
            expected: identity,
            observed,
        });
    }
    Ok(())
}

fn hash_file(file: &mut File, length: u32) -> Result<FragmentIdentity, ImmutableArtifactError> {
    let mut remaining =
        usize::try_from(length).map_err(|source| ImmutableArtifactError::LengthAddressSpace {
            observed: length,
            source,
        })?;
    let mut observed_bytes = 0_usize;
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    let mut hasher = ArtifactHasher::<IrFragmentEncoding, IrFragmentDomain>::new();
    while remaining != 0 {
        let requested = remaining.min(buffer.len());
        let read =
            file.read(&mut buffer[..requested])
                .map_err(|source| ImmutableArtifactError::Io {
                    phase: ImmutableIoPhase::ReadExisting,
                    source,
                })?;
        if read == 0 {
            return Err(ImmutableArtifactError::ExistingTruncated {
                expected: length,
                observed: observed_bytes,
                source: io::Error::from(ErrorKind::UnexpectedEof),
            });
        }
        hasher.write_chunk(&buffer[..read]);
        remaining -= read;
        observed_bytes += read;
    }
    Ok(hasher.finalize())
}

fn sync_directory(directory: &Path) -> Result<(), ImmutableArtifactError> {
    let directory_file = File::open(directory).map_err(|source| ImmutableArtifactError::Io {
        phase: ImmutableIoPhase::OpenArtifactDirectory,
        source,
    })?;
    directory_file
        .sync_all()
        .map_err(|source| ImmutableArtifactError::Io {
            phase: ImmutableIoPhase::SyncArtifactDirectory,
            source,
        })
}

fn artifact_name(identity: FragmentIdentity) -> [u8; ARTIFACT_NAME_BYTES] {
    let raw: &[u8; HASH_BYTES] = identity.as_ref();
    let mut name = [0_u8; ARTIFACT_NAME_BYTES];
    for (index, byte) in raw.iter().copied().enumerate() {
        name[index * 2] = HEX_DIGITS[usize::from(byte >> 4)];
        name[index * 2 + 1] = HEX_DIGITS[usize::from(byte & 0x0f)];
    }
    name[HEX_NAME_BYTES..].copy_from_slice(ARTIFACT_EXTENSION.as_bytes());
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

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::{self, Read},
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use nudox_id::HASH_BYTES;

    use super::{FragmentIdentity, ImmutableArtifactError, ImmutableArtifactStore};

    const BYTES: &[u8] = b"validated-fragment-bytes";

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error(transparent)]
        Io(#[from] io::Error),
        #[error(transparent)]
        Artifact(#[from] ImmutableArtifactError),
        #[error("system clock is before the Unix epoch")]
        Clock(#[source] std::time::SystemTimeError),
    }

    #[test]
    fn ensure_reuses_valid_identity_without_rewriting_final_bytes() -> Result<(), TestError> {
        let directory = unique_directory()?;
        let store = ImmutableArtifactStore::new(&directory)?;
        let identity = FragmentIdentity::from_encoded_bytes(BYTES);
        let first = store.ensure(identity, u32::try_from(BYTES.len()).unwrap_or(0), BYTES)?;

        let file_name = first
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "artifact path is not valid UTF-8",
                )
            })?;
        assert_eq!(file_name.len(), HASH_BYTES * 2 + ".irfrag".len());
        assert!(
            file_name[..HASH_BYTES * 2]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );

        let before = fs::metadata(&first.path)?;
        let mut observed = [0_u8; BYTES.len()];
        File::open(&first.path)?.read_exact(&mut observed)?;
        assert_eq!(&observed, BYTES);

        let second = store.ensure(identity, u32::try_from(BYTES.len()).unwrap_or(0), BYTES)?;
        let after = fs::metadata(&second.path)?;
        assert_eq!(first, second);
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified()?, after.modified()?);

        fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn unique_directory() -> Result<PathBuf, TestError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(TestError::Clock)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nudox-compile-publication-immutable-{}-{elapsed}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(directory)
    }
}
