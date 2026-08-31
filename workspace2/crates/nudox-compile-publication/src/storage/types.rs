//! Typed immutable-file facts and exact filesystem failure vocabulary.

use std::{io, num::TryFromIntError, path::PathBuf, str::Utf8Error};

use nudox_id::{ArtifactId, Domain, Encoding};
use thiserror::Error;

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
