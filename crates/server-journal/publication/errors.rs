//! Defines publication errors behavior for `server-journal`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the publication errors invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;
use std::{
    collections::TryReserveError,
    io,
    num::{NonZeroUsize, TryFromIntError},
    sync::{Arc, mpsc::RecvError},
};

use heart_hydration::VerifiedGeneration;
use backend_version::{ContentIdDecodeError, Domain};
use server_workflow::{ReductionError, StageKey, WorkflowRecord};
use thiserror::Error;

use super::facts::PublicationFacts;
use crate::{CommitError, FrameSequence, JournalError, format::CHECKSUM_BYTES};

/// Maximum UTF-8 prefix retained from a publication-owner panic payload.
pub const MAX_PUBLICATION_OWNER_PANIC_BYTES: usize = 96;

/// Closed class of payload recovered from a publication-owner thread panic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationOwnerPanicClass {
    /// The thread panicked with a borrowed static message.
    StaticMessage,
    /// The thread panicked with an owned string message.
    OwnedMessage,
    /// The opaque payload was neither supported message representation.
    Opaque,
}

/// Bounded message facts recovered from a publication-owner panic payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationOwnerPanicMessage {
    /// Retained message prefix, zero-filled after `byte_len`.
    pub bytes: [u8; MAX_PUBLICATION_OWNER_PANIC_BYTES],
    /// Number of meaningful bytes in `bytes`.
    pub byte_len: usize,
    /// Whether the original message continued beyond the retained prefix.
    pub truncated: bool,
}

/// Exact supported facts recovered when the exclusive publication owner panics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationOwnerPanic {
    /// Concrete representation carried by the original panic payload.
    pub class: PublicationOwnerPanicClass,
    /// Bounded original message facts.
    pub message: PublicationOwnerPanicMessage,
}

impl PublicationOwnerPanic {
    pub(crate) fn capture(payload: &(dyn core::any::Any + Send)) -> Self {
        let (class, message) = if let Some(message) = payload.downcast_ref::<&'static str>() {
            (PublicationOwnerPanicClass::StaticMessage, *message)
        } else if let Some(message) = payload.downcast_ref::<String>() {
            (PublicationOwnerPanicClass::OwnedMessage, message.as_str())
        } else {
            (PublicationOwnerPanicClass::Opaque, "")
        };
        let mut retained = message.len().min(MAX_PUBLICATION_OWNER_PANIC_BYTES);
        while !message.is_char_boundary(retained) {
            retained -= 1;
        }
        let mut bytes = [0_u8; MAX_PUBLICATION_OWNER_PANIC_BYTES];
        bytes[..retained].copy_from_slice(&message.as_bytes()[..retained]);
        Self {
            class,
            message: PublicationOwnerPanicMessage {
                bytes,
                byte_len: retained,
                truncated: message.len() > retained,
            },
        }
    }
}

impl core::fmt::Display for PublicationOwnerPanic {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "publication owner panicked with {:?} payload",
            self.class
        )?;
        if let Some(bytes) = self.message.bytes.get(..self.message.byte_len) {
            if let Ok(message) = core::str::from_utf8(bytes) {
                if !message.is_empty() {
                    write!(formatter, ": {message}")?;
                }
            }
        }
        if self.message.truncated {
            formatter.write_str(" (truncated)")?;
        }
        Ok(())
    }
}

impl std::error::Error for PublicationOwnerPanic {}

/// Rejection while restoring one typed generation identity from a journal fact.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublicationGenerationError {
    /// The retained root bytes do not carry the root identity authority.
    #[error("publication root identity has the wrong authority")]
    PinnedRoot(#[source] ContentIdDecodeError),
    /// The retained dependency-set bytes do not carry that identity authority.
    #[error("publication dependency-set identity has the wrong authority")]
    DependencySet(#[source] ContentIdDecodeError),
}

/// Rejection of a publication resource bound before any owner is started.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublicationLimitError {
    /// The requested frame-byte credit product cannot be represented by `usize`.
    #[error("publication frame-byte capacity overflows usize for {queue_capacity:?} items")]
    FrameBytesOverflow { queue_capacity: NonZeroUsize },
    /// The bounded credit or owner storage could not be allocated before startup.
    #[error("publication {resource} capacity of {capacity} could not be allocated")]
    Allocation {
        /// The bounded storage that failed allocation.
        resource: &'static str,
        /// Number of elements requested for that storage.
        capacity: usize,
        /// Original allocator reservation source.
        #[source]
        source: TryReserveError,
    },
}

/// A typed physical operation used by immutable-fact and head diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationIoStep {
    /// Create the immutable fact file.
    CreateFact,
    /// Read the immutable fact file.
    ReadFact,
    /// Write immutable fact bytes.
    WriteFact,
    /// Sync immutable fact bytes.
    SyncFact,
    /// Open the fact's parent directory.
    OpenFactParent,
    /// Sync the fact's parent directory.
    SyncFactParent,
    /// Create the temporary head file.
    CreateHeadTemp,
    /// Read the visible head file.
    ReadHead,
    /// Write temporary head bytes.
    WriteHead,
    /// Sync temporary head bytes.
    SyncHead,
    /// Rename the fully synced temporary head into place.
    RenameHead,
    /// Open the head's parent directory.
    OpenHeadParent,
    /// Sync the head's parent directory.
    SyncHeadParent,
    /// Inspect whether a temporary head remains from an interrupted publication.
    InspectHeadTemp,
}

/// A source-bearing failure after a publication command was admitted.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublicationFailure {
    /// The journal group could not establish a stable receipt.
    #[error("journal group commit failed")]
    Journal(#[source] SharedCommitError),
    /// A publication artifact filesystem operation failed.
    #[error("publication I/O failed during {step:?}")]
    Io {
        /// Exact physical step.
        step: PublicationIoStep,
        /// Original filesystem source.
        #[source]
        source: io::Error,
    },
    /// The immutable fact or head was structurally the wrong width.
    #[error("{artifact} has {observed} bytes; expected {expected}")]
    Length {
        /// Artifact name retained in the typed error.
        artifact: ArtifactName,
        /// Required fixed width.
        expected: usize,
        /// Complete observed width.
        observed: u64,
    },
    /// A changed root, dependency set, or derived workflow fact conflicts with the current head.
    #[error("publication conflicts with the current single head")]
    Conflict {
        /// Exact incoming and retained root/dependency facts.
        facts: Arc<PublicationConflict>,
    },
    /// A recovered journal has a stable key but no retained root/dep fact to compare directly.
    #[error("publication conflicts with the recovered journal key")]
    JournalKeyConflict {
        /// Key derived from the incoming verified witness.
        expected_key: StageKey,
        /// Key retained by the recovered journal reduction.
        observed_key: StageKey,
    },
    /// A command could not bind its derived input to the terminal facts.
    #[error("publication terminal facts do not match the admitted verified input")]
    InputMismatch,
    /// The admitted input could not restore the typed facts needed for a public receipt.
    #[error("publication input cannot restore typed generation facts")]
    Generation(#[source] PublicationGenerationError),
    /// The owner was poisoned after a prior source-bearing failure.
    #[error("publication owner is poisoned and requires independent reopen")]
    Poisoned,
    /// The write-once published-state cell already retained a different terminal fact.
    #[error("publication write-once state already contains a terminal fact")]
    PublishedStateConflict(Arc<PublicationStateConflict>),
    /// The latest-publication scalar snapshot rejected an exact state transition.
    #[error("latest publication snapshot transition failed")]
    Snapshot(#[source] PublicationSnapshotError),
    /// Independent artifact parsing failed while reconciling an already-created file.
    #[error("publication artifact reconciliation failed")]
    Open(#[source] PublicationOpenError),
    /// A temporary head already exists and must be independently reconciled.
    #[error("publication temporary head exists after an incomplete head transition")]
    HeadTempPresent,
    /// The exact source is shared by terminals from one failed physical group.
    #[error("publication group failure: {0}")]
    Shared(#[source] SharedPublicationFailure),
}

/// Exact root and dependency facts from both sides of a single-head conflict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationConflict {
    /// Root named by the incoming command.
    pub expected_root: [u8; 32],
    /// Dependency set named by the incoming command.
    pub expected_dep_set: [u8; 32],
    /// Root retained by the current durable fact.
    pub observed_root: [u8; 32],
    /// Dependency set retained by the current durable fact.
    pub observed_dep_set: [u8; 32],
}

/// Exact facts from both sides of a rejected write-once publication transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationStateConflict {
    /// Existing fact, when independently observable after the failed write.
    pub retained: Option<PublicationFacts>,
    /// Fact rejected by the write-once state boundary.
    pub attempted: PublicationFacts,
}

/// Exact bounded failure from the latest-publication scalar snapshot.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[non_exhaustive]
pub enum PublicationSnapshotError {
    /// A reader could not obtain one stable version within the fixed retry budget.
    #[error(
        "latest publication snapshot remained contended after {attempts} attempts at version {observed_version}"
    )]
    ReadContended {
        /// Number of complete read attempts made.
        attempts: u8,
        /// Last version observed by the reader.
        observed_version: u64,
    },
    /// A second writer attempted to enter the single-owner transition.
    #[error("latest publication snapshot already has a writer at version {observed_version}")]
    ConcurrentWriter {
        /// Odd or newly changed version proving another writer won.
        observed_version: u64,
        /// Exact fact that could not become visible.
        attempted: PublicationFacts,
    },
    /// The even snapshot version cannot represent another complete transition.
    #[error("latest publication snapshot exhausted version {observed_version}")]
    VersionExhausted {
        /// Last complete even version.
        observed_version: u64,
        /// Exact fact that could not become visible.
        attempted: PublicationFacts,
    },
}

/// The closed set of source-bearing journal failures that can be fanned out.
#[derive(Debug, Error)]
enum SharedCommitSource {
    #[error(transparent)]
    Commit(#[from] CommitError),
    #[error("group reduction rejected attempted record {attempted:?}")]
    GroupReduction {
        attempted: Arc<WorkflowRecord>,
        #[source]
        source: ReductionError,
    },
    #[error("grouped receipt conversion failed for {sequence:?}")]
    ReceiptConversion {
        sequence: FrameSequence,
        #[source]
        source: TryFromIntError,
    },
}

/// Opaque owner for a journal source shared by publication terminals.
pub struct SharedCommitError(Arc<SharedCommitSource>);

impl SharedCommitError {
    pub(super) fn new(error: CommitError) -> Self {
        Self(Arc::new(SharedCommitSource::Commit(error)))
    }

    pub(super) fn reduction(attempted: Arc<WorkflowRecord>, source: ReductionError) -> Self {
        Self(Arc::new(SharedCommitSource::GroupReduction {
            attempted,
            source,
        }))
    }

    pub(super) fn receipt_conversion(sequence: FrameSequence, source: TryFromIntError) -> Self {
        Self(Arc::new(SharedCommitSource::ReceiptConversion {
            sequence,
            source,
        }))
    }
}

impl std::fmt::Debug for SharedCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::fmt::Display for SharedCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for SharedCommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Opaque source owner used when one physical failure fans out to several accepted commands.
pub struct SharedPublicationFailure(pub(super) Arc<PublicationFailure>);

impl Deref for SharedPublicationFailure {
    type Target = PublicationFailure;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl std::fmt::Debug for SharedPublicationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::fmt::Display for SharedPublicationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for SharedPublicationFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Fixed artifact names used in structural length errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactName {
    /// Immutable fact artifact.
    Fact,
    /// Compact visible head artifact.
    Head,
}

impl std::fmt::Display for ArtifactName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Fact => "immutable publication fact",
            Self::Head => "publication head",
        })
    }
}

/// Failure while opening or independently checking one publication directory.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PublicationOpenError {
    /// Bounded owner storage could not be allocated before the worker started.
    #[error("publication bounded storage could not be allocated")]
    Capacity(#[source] PublicationLimitError),
    /// The journal could not be opened or replayed.
    #[error("publication journal could not be opened")]
    Journal(#[source] JournalError),
    /// A publication filesystem operation failed.
    #[error("publication I/O failed during {step:?}")]
    Io {
        /// Exact physical step.
        step: PublicationIoStep,
        /// Original filesystem source.
        #[source]
        source: io::Error,
    },
    /// A fixed artifact has a wrong length.
    #[error("{artifact} has {observed} bytes; expected {expected}")]
    Length {
        /// Artifact name retained in the typed error.
        artifact: ArtifactName,
        /// Required fixed width.
        expected: usize,
        /// Complete observed width.
        observed: u64,
    },
    /// The immutable fact checksum is invalid.
    #[error("immutable publication fact checksum is invalid")]
    FactChecksum {
        /// Recomputed checksum.
        expected: [u8; CHECKSUM_BYTES],
        /// Stored checksum.
        observed: [u8; CHECKSUM_BYTES],
    },
    /// The head checksum is invalid.
    #[error("publication head checksum is invalid")]
    HeadChecksum {
        /// Recomputed checksum.
        expected: [u8; CHECKSUM_BYTES],
        /// Stored checksum.
        observed: [u8; CHECKSUM_BYTES],
    },
    /// A fact exists without a visible head.
    #[error("immutable publication fact exists without a visible head")]
    MissingHead,
    /// A head exists without its immutable fact.
    #[error("publication head exists without an immutable fact")]
    MissingFact,
    /// An interrupted temporary head must be reconciled before reopening.
    #[error("publication temporary head exists")]
    HeadTempPresent,
    /// The stable receipt does not name the journal's complete current frame.
    #[error("publication fact receipt does not link to the current journal image")]
    ReceiptMismatch,
    /// The journal's current workflow key is not the fact's key.
    #[error("publication fact key does not link to the journal reduction")]
    JournalKeyMismatch,
    /// The visible head does not link exactly to the immutable fact.
    #[error("publication head does not link to the immutable fact and stable receipt")]
    HeadLinkMismatch,
    /// An immutable fact file or head cannot be parsed as this fixed version.
    #[error("publication artifact has an unsupported fixed encoding")]
    EncodingMismatch,
    /// The immutable fact did not retain valid typed generation identities.
    #[error("publication fact contains an invalid typed generation identity")]
    Generation(#[source] PublicationGenerationError),
    /// The owner could not return its startup result.
    #[error("publication owner exited before startup completed")]
    OwnerStartupLost {
        /// Exact startup-channel disconnection.
        #[source]
        source: RecvError,
    },
    /// The owner panicked before it could return its startup result.
    #[error("publication owner panicked before startup completed")]
    OwnerStartupPanic(#[source] PublicationOwnerPanic),
    /// Startup attempted to replace an already initialized publication fact.
    #[error("publication startup write-once state already contains a terminal fact")]
    PublishedStateConflict(Arc<PublicationStateConflict>),
    /// The latest-publication scalar snapshot could not be read or initialized.
    #[error("latest publication snapshot failed")]
    Snapshot(#[source] PublicationSnapshotError),
}

/// A shutdown source from the owner or its join boundary.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShutdownError {
    /// The owner reached a source-bearing poisoned terminal.
    #[error("publication owner stopped with a durable failure")]
    Owner(#[source] PublicationFailure),
    /// The owner thread panicked before returning a typed terminal.
    #[error("publication owner thread could not be joined")]
    Join(#[source] PublicationOwnerPanic),
}

/// A rejected nonblocking submission retaining its exact verified witness.
pub enum SubmitError<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// The independent bounded admission slots are full.
    Full {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    },
    /// The owner receiver has been closed.
    Closed {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    },
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for SubmitError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple(match self {
                Self::Full { .. } => "Full",
                Self::Closed { .. } => "Closed",
            })
            .finish()
    }
}

impl<DomainTag, PayloadOwner> std::fmt::Display for SubmitError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Full { .. } => "publication admission is full",
            Self::Closed { .. } => "publication owner is closed",
        })
    }
}

impl<DomainTag, PayloadOwner> std::error::Error for SubmitError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
}

/// A cancellation race that reached a durable terminal before cancellation won.
pub enum CancelError<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// The owner already returned a stable publication.
    Completed {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        /// The stable publication facts.
        publication: Arc<PublicationFacts>,
    },
    /// The owner returned a source-bearing publication failure.
    Failed {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        /// The exact failure source.
        source: SharedPublicationFailure,
    },
    /// The owner receiver disappeared before cancellation could observe its terminal.
    OwnerLost {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        /// The exact receiver-loss source.
        source: RecvError,
    },
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for CancelError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple(match self {
                Self::Completed { .. } => "Completed",
                Self::Failed { .. } => "Failed",
                Self::OwnerLost { .. } => "OwnerLost",
            })
            .finish()
    }
}

impl<DomainTag, PayloadOwner> std::fmt::Display for CancelError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Completed { .. } => "publication completed before cancellation",
            Self::Failed { .. } => "publication failed before cancellation",
            Self::OwnerLost { .. } => "publication owner disappeared during cancellation",
        })
    }
}

impl<DomainTag, PayloadOwner> std::error::Error for CancelError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
}

/// A wait terminal that always retains the exact admitted verified witness on failure.
pub enum PublicationError<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// The owner returned a source-bearing failure.
    Failed {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        /// The exact durable or artifact source.
        source: SharedPublicationFailure,
    },
    /// The owner receiver was lost before a terminal could be observed.
    OwnerLost {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        /// The exact receiver-loss source.
        source: RecvError,
    },
    /// The owner observed a cancellation before any effect became visible.
    Cancelled {
        /// The original verified witness, unchanged.
        generation: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    },
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for PublicationError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple(match self {
                Self::Failed { .. } => "Failed",
                Self::OwnerLost { .. } => "OwnerLost",
                Self::Cancelled { .. } => "Cancelled",
            })
            .finish()
    }
}

impl<DomainTag, PayloadOwner> std::fmt::Display for PublicationError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Failed { .. } => "publication failed",
            Self::OwnerLost { .. } => "publication owner disappeared",
            Self::Cancelled { .. } => "publication was cancelled",
        })
    }
}

impl<DomainTag, PayloadOwner> std::error::Error for PublicationError<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
}
