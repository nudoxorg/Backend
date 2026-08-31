use std::{
    collections::TryReserveError,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    mem::size_of,
    num::NonZeroUsize,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc::{Receiver, RecvError, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
};

use blake3::Hasher;
use nudox_hydration::{VerifiedGeneration, VerifiedGenerationFacts};
use nudox_id::Domain;
use nudox_workflow::{EventKind, StageKey, WorkflowEvent, WorkflowVersion};
use thiserror::Error;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout,
    byteorder::{LittleEndian, U16, U64},
};

use crate::{
    CommitError, FrameSequence, JournalError, JournalOffset, ReceiptFacts,
    format::CHECKSUM_BYTES,
    journal::{FileJournal, GroupCommitError},
};

const FACT_MAGIC: [u8; 8] = *b"NUDXPFC\0";
const HEAD_MAGIC: [u8; 8] = *b"NUDXPHD\0";
const PUBLICATION_VERSION: u16 = 1;
const FACT_DOMAIN: &[u8] = b"nudox.publication.fact.v1\0";
const HEAD_DOMAIN: &[u8] = b"nudox.publication.head.v1\0";
const KEY_DOMAIN: &[u8] = b"nudox.publication.key.v1\0";
const OUTPUT_DOMAIN: &[u8] = b"nudox.publication.output.v1\0";

const ACTIVE: u8 = 1;
const CANCELLED: u8 = 2;
const COMPLETED: u8 = 3;
const COMMITTING: u8 = 4;

/// The fixed paths owned by one local durable publication directory.
#[derive(Clone, Debug)]
pub struct PublicationPaths {
    directory: PathBuf,
}

impl PublicationPaths {
    /// Names the directory containing one journal, immutable fact, and visible head.
    #[must_use]
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    fn journal(&self) -> PathBuf {
        self.directory.join("journal")
    }

    fn fact(&self) -> PathBuf {
        self.directory.join("publication.fact")
    }

    fn head(&self) -> PathBuf {
        self.directory.join("publication.head")
    }

    fn head_temp(&self) -> PathBuf {
        self.directory.join("publication.head.tmp")
    }
}

/// Bounded admission and physical group sizes for one publisher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationLimits {
    queue_capacity: NonZeroUsize,
    group_capacity: NonZeroUsize,
}

impl PublicationLimits {
    /// Validates the nonzero queue and reusable group capacities.
    ///
    /// The validation includes both frame-credit products, even though only the group product is
    /// retained as a byte buffer. This keeps an admitted frame-byte bound representable for every
    /// legal queue size.
    pub fn new(
        queue_capacity: NonZeroUsize,
        group_capacity: NonZeroUsize,
    ) -> Result<Self, PublicationLimitError> {
        queue_capacity
            .get()
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow { queue_capacity })?;
        group_capacity
            .get()
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow {
                queue_capacity: group_capacity,
            })?;
        Ok(Self {
            queue_capacity,
            group_capacity,
        })
    }
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

/// Checksum identity of the immutable publication fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImmutablePublicationIdentity {
    /// BLAKE3 checksum of the complete canonical fact payload.
    pub checksum: [u8; CHECKSUM_BYTES],
}

/// Checksum identity of the compact visible publication head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationHeadIdentity {
    /// BLAKE3 checksum of the complete canonical head payload.
    pub checksum: [u8; CHECKSUM_BYTES],
}

/// Independently readable facts linking a stable journal receipt to both publication artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PublicationFacts {
    /// The stable journal receipt named by the immutable publication fact.
    pub stable: ReceiptFacts,
    /// Identity of the immutable publication fact.
    pub immutable: ImmutablePublicationIdentity,
    /// Identity of the visible compact publication head.
    pub head: PublicationHeadIdentity,
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
    /// The owner was poisoned after a prior source-bearing failure.
    #[error("publication owner is poisoned and requires independent reopen")]
    Poisoned,
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

/// Opaque owner for a journal source shared by publication terminals.
pub struct SharedCommitError(Arc<CommitError>);

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
pub struct SharedPublicationFailure(Arc<PublicationFailure>);

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
    /// The shared publication-state mutex was poisoned by an unexpected panic.
    #[error("publication owner state is poisoned")]
    StatePoisoned,
    /// The owner could not return its startup result.
    #[error("publication owner exited before startup completed")]
    OwnerStartupLost,
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
    Join,
}

/// A rejected nonblocking submission retaining its exact verified witness.
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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

/// The single owner service for one local journal and its publication artifacts.
pub struct DurablePublisher {
    sender: Option<SyncSender<Command>>,
    owner: Option<JoinHandle<OwnerExit>>,
    state: Arc<PublisherState>,
}

impl std::fmt::Debug for DurablePublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurablePublisher")
            .field("open", &self.sender.is_some())
            .finish_non_exhaustive()
    }
}

impl DurablePublisher {
    /// Creates a new local journal and starts its exclusive owner thread.
    pub fn create(
        paths: &PublicationPaths,
        limits: PublicationLimits,
    ) -> Result<Self, PublicationOpenError> {
        fs::create_dir_all(&paths.directory).map_err(|source| PublicationOpenError::Io {
            step: PublicationIoStep::CreateFact,
            source,
        })?;
        Self::start(paths, limits, OpenMode::Create)
    }

    /// Reopens and independently validates the journal, immutable fact, and visible head.
    pub fn reopen(
        paths: &PublicationPaths,
        limits: PublicationLimits,
    ) -> Result<Self, PublicationOpenError> {
        Self::start(paths, limits, OpenMode::Open)
    }

    /// Submits one real verified generation without blocking for owner or filesystem work.
    pub fn try_publish<'store, DomainTag, PayloadOwner>(
        &self,
        verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    ) -> Result<
        PendingPublication<'store, DomainTag, PayloadOwner>,
        SubmitError<'store, DomainTag, PayloadOwner>,
    >
    where
        DomainTag: Domain,
        PayloadOwner: AsRef<[u8]>,
    {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(SubmitError::Closed {
                generation: verified,
            });
        }
        let Some(reservation) = self.state.credits.reserve() else {
            return Err(SubmitError::Full {
                generation: verified,
            });
        };
        let facts = *verified;
        let input = PublicationInput::from_facts(facts);
        let (response_sender, response_receiver) = sync_channel(1);
        let command = Command {
            input,
            lease: Arc::clone(&reservation),
            response: response_sender,
        };
        let Some(sender) = self.sender.as_ref() else {
            drop(command);
            drop(reservation);
            return Err(SubmitError::Closed {
                generation: verified,
            });
        };
        match sender.try_send(command) {
            Ok(()) => Ok(PendingPublication {
                verified,
                input,
                response: response_receiver,
                lease: PendingLease(reservation),
            }),
            Err(TrySendError::Full(command)) => {
                drop(command);
                drop(reservation);
                Err(SubmitError::Full {
                    generation: verified,
                })
            }
            Err(TrySendError::Disconnected(command)) => {
                drop(command);
                drop(reservation);
                Err(SubmitError::Closed {
                    generation: verified,
                })
            }
        }
    }

    /// Returns independently validated current publication facts, if a head is visible.
    pub fn published(&self) -> Result<Option<PublicationFacts>, PublicationOpenError> {
        self.state
            .published
            .lock()
            .map(|published| *published)
            .map_err(|_| PublicationOpenError::StatePoisoned)
    }

    /// Closes admission, drains accepted commands, and joins the owner exactly once.
    pub fn shutdown(mut self) -> Result<(), ShutdownError> {
        self.state.closed.store(true, Ordering::Release);
        drop(self.sender.take());
        let Some(owner) = self.owner.take() else {
            return Ok(());
        };
        match owner.join() {
            Ok(OwnerExit::Clean) => Ok(()),
            Ok(OwnerExit::Failed(source)) => Err(ShutdownError::Owner(source)),
            Err(_) => Err(ShutdownError::Join),
        }
    }

    fn start(
        paths: &PublicationPaths,
        limits: PublicationLimits,
        mode: OpenMode,
    ) -> Result<Self, PublicationOpenError> {
        let queue_capacity = limits.queue_capacity.get();
        let group_capacity = limits.group_capacity.get();
        let credits = CreditPool::new(queue_capacity);
        let frame_bytes = group_capacity
            .checked_mul(crate::JOURNAL_FRAME_BYTES)
            .ok_or(PublicationLimitError::FrameBytesOverflow {
                queue_capacity: limits.group_capacity,
            })
            .map_err(PublicationOpenError::Capacity)?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(frame_bytes).map_err(|source| {
            PublicationOpenError::Capacity(PublicationLimitError::Allocation {
                resource: "group frame",
                capacity: frame_bytes,
                source,
            })
        })?;
        frames.resize(frame_bytes, 0);
        let mut group = Vec::new();
        group.try_reserve_exact(group_capacity).map_err(|source| {
            PublicationOpenError::Capacity(PublicationLimitError::Allocation {
                resource: "group command",
                capacity: group_capacity,
                source,
            })
        })?;
        let storage = OwnerStorage {
            frames,
            group,
            fact_bytes: [0; FACT_BYTES],
            head_bytes: [0; HEAD_BYTES],
        };
        let state = Arc::new(PublisherState {
            closed: AtomicBool::new(false),
            credits,
            published: Mutex::new(None),
            drop_failure: Mutex::new(None),
        });
        let (sender, receiver) = sync_channel(queue_capacity);
        let (startup_sender, startup_receiver) = sync_channel(1);
        let owner_paths = paths.clone();
        let owner_state = Arc::clone(&state);
        let owner = thread::Builder::new()
            .name("nudox-publication-owner".to_owned())
            .spawn(move || {
                owner_thread(
                    owner_paths,
                    group_capacity,
                    mode,
                    receiver,
                    startup_sender,
                    owner_state,
                    storage,
                )
            })
            .map_err(|source| PublicationOpenError::Io {
                step: PublicationIoStep::CreateFact,
                source,
            })?;
        let startup = match startup_receiver.recv() {
            Ok(startup) => startup,
            Err(_) => {
                drop(sender);
                drop(owner.join());
                return Err(PublicationOpenError::OwnerStartupLost);
            }
        };
        let initial = match startup {
            Ok(initial) => initial,
            Err(error) => {
                drop(sender);
                drop(owner.join());
                return Err(error);
            }
        };
        if let Some(published) = initial.published {
            if let Ok(mut target) = state.published.lock() {
                *target = Some(published);
            } else {
                drop(sender);
                drop(owner.join());
                return Err(PublicationOpenError::StatePoisoned);
            }
        }
        Ok(Self {
            sender: Some(sender),
            owner: Some(owner),
            state,
        })
    }
}

impl Drop for DurablePublisher {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::Release);
        drop(self.sender.take());
        if let Some(owner) = self.owner.take() {
            match owner.join() {
                Ok(OwnerExit::Clean) => {}
                Ok(OwnerExit::Failed(source)) => {
                    if let Ok(mut failure) = self.state.drop_failure.lock() {
                        *failure = Some(source);
                    }
                }
                Err(_) => {
                    if let Ok(mut failure) = self.state.drop_failure.lock() {
                        *failure = Some(PublicationFailure::Poisoned);
                    }
                }
            }
        }
    }
}

/// An admitted publication retaining its exact verified witness until terminal observation.
pub struct PendingPublication<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
    input: PublicationInput,
    response: Receiver<OwnerOutcome>,
    lease: PendingLease,
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for PendingPublication<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingPublication")
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl<'store, DomainTag, PayloadOwner> PendingPublication<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// Waits for one stable owner terminal and binds the exact witness on success.
    pub fn wait(
        self,
    ) -> Result<
        PublishedGeneration<'store, DomainTag, PayloadOwner>,
        PublicationError<'store, DomainTag, PayloadOwner>,
    > {
        let Self {
            verified,
            input,
            response,
            lease: _pending_lease,
        } = self;
        let outcome = response.recv();
        match outcome {
            Ok(OwnerOutcome::Published(publication)) => {
                if !input.matches_publication(publication) {
                    return Err(PublicationError::Failed {
                        generation: verified,
                        source: shared_failure(PublicationFailure::InputMismatch),
                    });
                }
                let facts = *verified;
                Ok(PublishedGeneration {
                    facts,
                    publication,
                    _seal: PublicationSeal {
                        _verified: verified,
                    },
                })
            }
            Ok(OwnerOutcome::Failed(source)) => Err(PublicationError::Failed {
                generation: verified,
                source: shared_failure(source),
            }),
            Ok(OwnerOutcome::Cancelled) => Err(PublicationError::Cancelled {
                generation: verified,
            }),
            Err(source) => Err(PublicationError::OwnerLost {
                generation: verified,
                source,
            }),
        }
    }

    /// Cancels this pending command and returns its exact witness when cancellation wins.
    pub fn cancel(
        self,
    ) -> Result<
        VerifiedGeneration<'store, DomainTag, PayloadOwner>,
        CancelError<'store, DomainTag, PayloadOwner>,
    > {
        let Self {
            verified,
            response,
            lease: pending_lease,
            ..
        } = self;
        let lease = pending_lease.arc();
        let _cancel_won = lease.cancel();
        match response.recv() {
            Ok(OwnerOutcome::Published(publication)) => Err(CancelError::Completed {
                generation: verified,
                publication: Arc::new(publication),
            }),
            Ok(OwnerOutcome::Failed(source)) => Err(CancelError::Failed {
                generation: verified,
                source: shared_failure(source),
            }),
            Ok(OwnerOutcome::Cancelled) => Ok(verified),
            Err(_) => Err(CancelError::OwnerLost {
                generation: verified,
                source: RecvError,
            }),
        }
    }
}

/// A witness-bound durable publication authority.
#[non_exhaustive]
pub struct PublishedGeneration<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    /// Independently checked immutable publication and head facts.
    pub publication: PublicationFacts,
    facts: VerifiedGenerationFacts,
    _seal: PublicationSeal<'store, DomainTag, PayloadOwner>,
}

impl<DomainTag, PayloadOwner> Deref for PublishedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    type Target = VerifiedGenerationFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag, PayloadOwner> std::fmt::Debug for PublishedGeneration<'_, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishedGeneration")
            .field("facts", &self.facts)
            .field("publication", &self.publication)
            .finish_non_exhaustive()
    }
}

struct PublicationSeal<'store, DomainTag, PayloadOwner>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
{
    _verified: VerifiedGeneration<'store, DomainTag, PayloadOwner>,
}

#[derive(Debug)]
struct PublisherState {
    closed: AtomicBool,
    credits: Arc<CreditPool>,
    published: Mutex<Option<PublicationFacts>>,
    drop_failure: Mutex<Option<PublicationFailure>>,
}

#[derive(Debug)]
struct CreditPool {
    capacity: usize,
    active: AtomicUsize,
    active_high_water: AtomicUsize,
}

impl CreditPool {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            active: AtomicUsize::new(0),
            active_high_water: AtomicUsize::new(0),
        })
    }

    fn reserve(self: &Arc<Self>) -> Option<Arc<CreditLease>> {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.capacity {
                return None;
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => active = observed,
            }
        }
        update_high_water(&self.active_high_water, active + 1);
        Some(Arc::new(CreditLease {
            pool: Arc::clone(self),
            state: AtomicU8::new(ACTIVE),
        }))
    }

    fn release_counts(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct CreditLease {
    pool: Arc<CreditPool>,
    state: AtomicU8,
}

impl CreditLease {
    fn cancel(&self) -> bool {
        self.state
            .compare_exchange(ACTIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn claim(&self) -> bool {
        self.state
            .compare_exchange(ACTIVE, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == CANCELLED
    }

    fn is_committing(&self) -> bool {
        self.state.load(Ordering::Acquire) == COMMITTING
    }

    fn complete(&self) {
        let mut state = self.state.load(Ordering::Acquire);
        while state == ACTIVE || state == CANCELLED || state == COMMITTING {
            match self
                .state
                .compare_exchange(state, COMPLETED, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(observed) => state = observed,
            }
        }
    }
}

impl Drop for CreditLease {
    fn drop(&mut self) {
        self.pool.release_counts();
    }
}

/// The pending-side guard cancels only while the command is still ACTIVE. It never releases the
/// shared lease; the queued command remains its owner until the worker observes a terminal.
struct PendingLease(Arc<CreditLease>);

impl PendingLease {
    fn arc(&self) -> Arc<CreditLease> {
        Arc::clone(&self.0)
    }
}

impl Drop for PendingLease {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn update_high_water(high_water: &AtomicUsize, observed: usize) {
    let mut current = high_water.load(Ordering::Relaxed);
    while observed > current {
        match high_water.compare_exchange_weak(
            current,
            observed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PublicationInput {
    root: [u8; 32],
    dep_set: [u8; 32],
    output: [u8; 32],
    key: StageKey,
}

impl PublicationInput {
    fn from_facts(facts: VerifiedGenerationFacts) -> Self {
        let root = *facts.pinned_root;
        let dep_set = *facts.dep_set;
        Self::from_parts(root, dep_set)
    }

    fn from_parts(root: [u8; 32], dep_set: [u8; 32]) -> Self {
        let mut key_hasher = Hasher::new();
        key_hasher.update(KEY_DOMAIN);
        key_hasher.update(&root);
        key_hasher.update(&dep_set);
        let key = StageKey::from(*key_hasher.finalize().as_bytes());
        let mut output_hasher = Hasher::new();
        output_hasher.update(OUTPUT_DOMAIN);
        output_hasher.update(&root);
        output_hasher.update(&dep_set);
        let output = *output_hasher.finalize().as_bytes();
        Self {
            root,
            dep_set,
            output,
            key,
        }
    }

    fn matches_publication(self, publication: PublicationFacts) -> bool {
        fact_identity(self, publication.stable).checksum == publication.immutable.checksum
            && head_identity(publication.immutable, publication.stable).checksum
                == publication.head.checksum
    }
}

fn conflict(expected: PublicationInput, observed: PublicationInput) -> PublicationFailure {
    PublicationFailure::Conflict {
        facts: Arc::new(PublicationConflict {
            expected_root: expected.root,
            expected_dep_set: expected.dep_set,
            observed_root: observed.root,
            observed_dep_set: observed.dep_set,
        }),
    }
}

fn journal_failure(error: CommitError) -> PublicationFailure {
    PublicationFailure::Journal(SharedCommitError(Arc::new(error)))
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct FactRecord {
    magic: [u8; 8],
    version: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
    root: [u8; 32],
    dep_set: [u8; 32],
    output: [u8; 32],
    key: [u8; 32],
    sequence: U64<LittleEndian>,
    durable_end: U64<LittleEndian>,
    checksum: [u8; CHECKSUM_BYTES],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout)]
struct HeadRecord {
    magic: [u8; 8],
    version: U16<LittleEndian>,
    reserved: U16<LittleEndian>,
    fact_checksum: [u8; CHECKSUM_BYTES],
    sequence: U64<LittleEndian>,
    durable_end: U64<LittleEndian>,
    checksum: [u8; CHECKSUM_BYTES],
}

const FACT_BYTES: usize = size_of::<FactRecord>();
const FACT_PAYLOAD_BYTES: usize = FACT_BYTES - CHECKSUM_BYTES;
const HEAD_BYTES: usize = size_of::<HeadRecord>();
const HEAD_PAYLOAD_BYTES: usize = HEAD_BYTES - CHECKSUM_BYTES;

fn fact_record(input: PublicationInput, receipt: ReceiptFacts) -> FactRecord {
    let mut record = FactRecord {
        magic: FACT_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        root: input.root,
        dep_set: input.dep_set,
        output: input.output,
        key: *input.key,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(FACT_DOMAIN, &record.as_bytes()[..FACT_PAYLOAD_BYTES]);
    record
}

fn fact_identity(input: PublicationInput, receipt: ReceiptFacts) -> ImmutablePublicationIdentity {
    ImmutablePublicationIdentity {
        checksum: fact_record(input, receipt).checksum,
    }
}

fn head_record(fact: ImmutablePublicationIdentity, receipt: ReceiptFacts) -> HeadRecord {
    let mut record = HeadRecord {
        magic: HEAD_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        fact_checksum: fact.checksum,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    record
}

fn head_identity(
    fact: ImmutablePublicationIdentity,
    receipt: ReceiptFacts,
) -> PublicationHeadIdentity {
    PublicationHeadIdentity {
        checksum: head_record(fact, receipt).checksum,
    }
}

struct ParsedFact {
    input: PublicationInput,
    receipt: ReceiptFacts,
    identity: ImmutablePublicationIdentity,
    bytes: [u8; FACT_BYTES],
}

struct ParsedHead {
    identity: PublicationHeadIdentity,
    fact_checksum: [u8; CHECKSUM_BYTES],
    receipt: ReceiptFacts,
    bytes: [u8; HEAD_BYTES],
}

struct StoredPublication {
    input: PublicationInput,
    receipt: ReceiptFacts,
    facts: PublicationFacts,
}

struct PendingJournal {
    key: StageKey,
    receipt: ReceiptFacts,
    input: Option<PublicationInput>,
}

struct InitialState {
    published: Option<PublicationFacts>,
}

enum OpenMode {
    Create,
    Open,
}

struct OwnerStorage {
    frames: Vec<u8>,
    group: Vec<Command>,
    fact_bytes: [u8; FACT_BYTES],
    head_bytes: [u8; HEAD_BYTES],
}

struct Command {
    input: PublicationInput,
    lease: Arc<CreditLease>,
    response: SyncSender<OwnerOutcome>,
}

/// One immutable physical source shared by every terminal in a failed group.
///
/// The source is reference-counted only at this failure fan-out boundary. No error is rebuilt from
/// its display text, so every terminal still exposes the original causal source and attempted
/// record.
struct FailureOwner(Arc<PublicationFailure>);

impl FailureOwner {
    fn new(source: PublicationFailure) -> Self {
        Self(Arc::new(source))
    }

    fn terminal(&self) -> PublicationFailure {
        PublicationFailure::Shared(SharedPublicationFailure(Arc::clone(&self.0)))
    }
}

fn shared_failure(source: PublicationFailure) -> SharedPublicationFailure {
    SharedPublicationFailure(Arc::new(source))
}

enum OwnerOutcome {
    Published(PublicationFacts),
    Failed(PublicationFailure),
    Cancelled,
}

enum OwnerExit {
    Clean,
    Failed(PublicationFailure),
}

fn owner_thread(
    paths: PublicationPaths,
    group_capacity: usize,
    mode: OpenMode,
    receiver: Receiver<Command>,
    startup: SyncSender<Result<InitialState, PublicationOpenError>>,
    state: Arc<PublisherState>,
    storage: OwnerStorage,
) -> OwnerExit {
    let OwnerStorage {
        mut frames,
        mut group,
        mut fact_bytes,
        mut head_bytes,
    } = storage;
    let mut journal = match mode {
        OpenMode::Create => match FileJournal::create(paths.journal()) {
            Ok(journal) => journal,
            Err(error) => {
                send_startup_error(startup, PublicationOpenError::Journal(error));
                return OwnerExit::Clean;
            }
        },
        OpenMode::Open => match FileJournal::open(paths.journal()) {
            Ok(journal) => journal,
            Err(error) => {
                send_startup_error(startup, PublicationOpenError::Journal(error));
                return OwnerExit::Clean;
            }
        },
    };
    let existing = match load_existing(&paths, &journal) {
        Ok(existing) => existing,
        Err(error) => {
            send_startup_error(startup, error);
            return OwnerExit::Clean;
        }
    };
    let initial = InitialState {
        published: existing.as_ref().map(|stored| stored.facts),
    };
    if startup.send(Ok(initial)).is_err() {
        return OwnerExit::Clean;
    }
    let mut current = existing;
    let mut pending = journal.last_receipt().and_then(|receipt| {
        (current.is_none()).then_some(PendingJournal {
            key: journal.current_key()?,
            receipt: *receipt,
            input: None,
        })
    });
    let mut poison = None;
    loop {
        let first = match receiver.recv() {
            Ok(command) => command,
            Err(_) => break,
        };
        group.clear();
        group.push(first);
        while group.len() < group_capacity {
            match receiver.try_recv() {
                Ok(command) => group.push(command),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        process_group(
            &mut journal,
            &mut group,
            &state,
            &mut current,
            &mut pending,
            &mut poison,
            OwnerBuffers {
                paths: &paths,
                frames: &mut frames,
                fact_bytes: &mut fact_bytes,
                head_bytes: &mut head_bytes,
            },
        );
    }
    match poison {
        Some(source) => OwnerExit::Failed(source.terminal()),
        None => OwnerExit::Clean,
    }
}

fn send_startup_error(
    startup: SyncSender<Result<InitialState, PublicationOpenError>>,
    error: PublicationOpenError,
) {
    drop(startup.send(Err(error)));
}

struct OwnerBuffers<'paths> {
    paths: &'paths PublicationPaths,
    frames: &'paths mut [u8],
    fact_bytes: &'paths mut [u8; FACT_BYTES],
    head_bytes: &'paths mut [u8; HEAD_BYTES],
}

fn process_group(
    journal: &mut FileJournal,
    group: &mut Vec<Command>,
    state: &PublisherState,
    current: &mut Option<StoredPublication>,
    pending: &mut Option<PendingJournal>,
    poison: &mut Option<FailureOwner>,
    buffers: OwnerBuffers<'_>,
) {
    if let Some(source) = poison.as_ref() {
        finish_poisoned(group, source, state);
        return;
    }

    // A dropped/cancelled pending command remains in the queue until this owner observes it. This
    // is the only point where the queued lease can become a terminal cancellation.
    while let Some(index) = group
        .iter()
        .position(|command| command.lease.is_cancelled())
    {
        let command = group.remove(index);
        finish(command, OwnerOutcome::Cancelled, state);
    }
    let Some(candidate) = group.iter().position(|command| command.lease.claim()) else {
        for command in group.drain(..) {
            finish(command, OwnerOutcome::Cancelled, state);
        }
        return;
    };
    let candidate_input = group[candidate].input;
    let receipt = if let Some(stored) = current.as_ref() {
        if stored.input == candidate_input {
            Some(stored.receipt)
        } else {
            let command = group.remove(candidate);
            finish(
                command,
                OwnerOutcome::Failed(conflict(candidate_input, stored.input)),
                state,
            );
            finish_conflicts(group, stored.input, state);
            return;
        }
    } else if let Some(existing) = pending.as_ref() {
        if existing.key == candidate_input.key
            && existing.input.is_none_or(|input| input == candidate_input)
        {
            Some(existing.receipt)
        } else if let Some(observed) = existing.input {
            let command = group.remove(candidate);
            finish(
                command,
                OwnerOutcome::Failed(conflict(candidate_input, observed)),
                state,
            );
            finish_conflicts(group, observed, state);
            return;
        } else {
            let command = group.remove(candidate);
            let source = PublicationFailure::JournalKeyConflict {
                expected_key: candidate_input.key,
                observed_key: existing.key,
            };
            finish(command, OwnerOutcome::Failed(source), state);
            finish_pending_conflicts(group, existing.key, state);
            return;
        }
    } else {
        let event = WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key: candidate_input.key,
            kind: EventKind::Requested,
        };
        match journal.append_group(&[event], buffers.frames) {
            Ok(receipts) => match receipts.receipt_at(0) {
                Some(receipt) => {
                    let facts = *receipt;
                    *pending = Some(PendingJournal {
                        key: candidate_input.key,
                        receipt: facts,
                        input: Some(candidate_input),
                    });
                    Some(facts)
                }
                None => {
                    poison_group(
                        poison,
                        journal_failure(CommitError::ReceiptOverflow {
                            sequence: FrameSequence::FIRST,
                        }),
                        group,
                        state,
                    );
                    return;
                }
            },
            Err(error) => {
                poison_group(poison, map_group_error(error), group, state);
                return;
            }
        }
    };
    let Some(receipt) = receipt else {
        return;
    };
    if current.is_none() {
        let fact = match persist_fact(buffers.paths, candidate_input, receipt, buffers.fact_bytes) {
            Ok(fact) => fact,
            Err(source) => {
                poison_group(poison, source, group, state);
                return;
            }
        };
        let head = match persist_head(buffers.paths, fact.identity, receipt, buffers.head_bytes) {
            Ok(head) => head,
            Err(source) => {
                poison_group(poison, source, group, state);
                return;
            }
        };
        let facts = PublicationFacts {
            stable: receipt,
            immutable: fact.identity,
            head: head.identity,
        };
        let stored = StoredPublication {
            input: candidate_input,
            receipt,
            facts,
        };
        if let Ok(mut published) = state.published.lock() {
            *published = Some(facts);
        }
        *current = Some(stored);
        *pending = None;
    }
    let Some(observed) = current.as_ref().map(|stored| stored.input) else {
        poison_group(poison, PublicationFailure::Poisoned, group, state);
        return;
    };
    finish_group_success(group, observed, candidate, state, current);
}

fn finish_poisoned(group: &mut Vec<Command>, source: &FailureOwner, state: &PublisherState) {
    for command in group.drain(..) {
        if command.lease.is_committing() {
            finish(command, OwnerOutcome::Failed(source.terminal()), state);
        } else if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            finish(command, OwnerOutcome::Failed(source.terminal()), state);
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn poison_group(
    poison: &mut Option<FailureOwner>,
    source: PublicationFailure,
    group: &mut Vec<Command>,
    state: &PublisherState,
) {
    *poison = Some(FailureOwner::new(source));
    if let Some(owner) = poison.as_ref() {
        finish_poisoned(group, owner, state);
    }
}

fn finish_conflicts(group: &mut Vec<Command>, observed: PublicationInput, state: &PublisherState) {
    for command in group.drain(..) {
        if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            let input = command.input;
            finish(
                command,
                OwnerOutcome::Failed(conflict(input, observed)),
                state,
            );
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish_pending_conflicts(
    group: &mut Vec<Command>,
    observed_key: StageKey,
    state: &PublisherState,
) {
    for command in group.drain(..) {
        if command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if command.lease.claim() {
            let expected_key = command.input.key;
            finish(
                command,
                OwnerOutcome::Failed(PublicationFailure::JournalKeyConflict {
                    expected_key,
                    observed_key,
                }),
                state,
            );
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish_group_success(
    group: &mut Vec<Command>,
    observed: PublicationInput,
    candidate: usize,
    state: &PublisherState,
    current: &Option<StoredPublication>,
) {
    for (index, command) in group.drain(..).enumerate() {
        let claimed = index == candidate;
        if !claimed && command.lease.is_cancelled() {
            finish(command, OwnerOutcome::Cancelled, state);
        } else if claimed || command.lease.claim() {
            if command.input == observed {
                if let Some(stored) = current.as_ref() {
                    finish(command, OwnerOutcome::Published(stored.facts), state);
                }
            } else {
                let input = command.input;
                finish(
                    command,
                    OwnerOutcome::Failed(conflict(input, observed)),
                    state,
                );
            }
        } else {
            finish(command, OwnerOutcome::Cancelled, state);
        }
    }
}

fn finish(command: Command, outcome: OwnerOutcome, _state: &PublisherState) {
    command.lease.complete();
    drop(command.response.send(outcome));
}

fn map_group_error(error: GroupCommitError) -> PublicationFailure {
    match error {
        GroupCommitError::Reduction { source, .. } => {
            journal_failure(CommitError::Reduction(source))
        }
        GroupCommitError::Poisoned => journal_failure(CommitError::Poisoned),
        GroupCommitError::ReceiptOverflow { sequence } => {
            journal_failure(CommitError::ReceiptOverflow { sequence })
        }
        GroupCommitError::StorageTooSmall { .. } => PublicationFailure::InputMismatch,
        GroupCommitError::OutcomeUnknown {
            attempted,
            first_sequence,
            step,
            source,
            ..
        } => journal_failure(CommitError::OutcomeUnknown {
            attempted: *attempted,
            sequence: first_sequence,
            step,
            source,
        }),
    }
}

/*
 * The old implementation rebuilt io::Error values from kind and display text here. Keeping that
 * routine would make a group terminal look source-bearing while silently dropping its causal
 * source, so all fan-out now goes through FailureOwner above.
 */

fn load_existing(
    paths: &PublicationPaths,
    journal: &FileJournal,
) -> Result<Option<StoredPublication>, PublicationOpenError> {
    if path_exists(&paths.head_temp(), PublicationIoStep::InspectHeadTemp)? {
        return Err(PublicationOpenError::HeadTempPresent);
    }
    let fact = read_fact(&paths.fact())?;
    let head = read_head(&paths.head())?;
    match (fact, head) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(PublicationOpenError::MissingHead),
        (None, Some(_)) => Err(PublicationOpenError::MissingFact),
        (Some(fact), Some(head)) => {
            if PublicationInput::from_parts(fact.input.root, fact.input.dep_set) != fact.input {
                return Err(PublicationOpenError::EncodingMismatch);
            }
            if !journal.receipt_is_current(fact.receipt) {
                return Err(PublicationOpenError::ReceiptMismatch);
            }
            if journal.current_key() != Some(fact.input.key) {
                return Err(PublicationOpenError::JournalKeyMismatch);
            }
            if head.fact_checksum != fact.identity.checksum || head.receipt != fact.receipt {
                return Err(PublicationOpenError::HeadLinkMismatch);
            }
            let facts = PublicationFacts {
                stable: fact.receipt,
                immutable: fact.identity,
                head: head.identity,
            };
            Ok(Some(StoredPublication {
                input: fact.input,
                receipt: fact.receipt,
                facts,
            }))
        }
    }
}

fn path_exists(path: &Path, step: PublicationIoStep) -> Result<bool, PublicationOpenError> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PublicationOpenError::Io { step, source }),
    }
}

fn read_fact(path: &Path) -> Result<Option<ParsedFact>, PublicationOpenError> {
    let Some(bytes) =
        read_fixed::<FACT_BYTES>(path, PublicationIoStep::ReadFact, ArtifactName::Fact)?
    else {
        return Ok(None);
    };
    parse_fact(bytes).map(Some)
}

fn read_head(path: &Path) -> Result<Option<ParsedHead>, PublicationOpenError> {
    let Some(bytes) =
        read_fixed::<HEAD_BYTES>(path, PublicationIoStep::ReadHead, ArtifactName::Head)?
    else {
        return Ok(None);
    };
    parse_head(bytes).map(Some)
}

fn read_fixed<const BYTES: usize>(
    path: &Path,
    step: PublicationIoStep,
    artifact: ArtifactName,
) -> Result<Option<[u8; BYTES]>, PublicationOpenError> {
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PublicationOpenError::Io { step, source }),
    };
    let observed = file
        .metadata()
        .map_err(|source| PublicationOpenError::Io { step, source })?
        .len();
    if observed != BYTES as u64 {
        return Err(PublicationOpenError::Length {
            artifact,
            expected: BYTES,
            observed,
        });
    }
    let mut bytes = [0; BYTES];
    file.read_exact(&mut bytes)
        .map_err(|source| PublicationOpenError::Io { step, source })?;
    Ok(Some(bytes))
}

fn parse_fact(bytes: [u8; FACT_BYTES]) -> Result<ParsedFact, PublicationOpenError> {
    let record: FactRecord = zerocopy::transmute!(bytes);
    if record.magic != FACT_MAGIC
        || record.version.get() != PUBLICATION_VERSION
        || record.reserved.get() != 0
    {
        return Err(PublicationOpenError::EncodingMismatch);
    }
    let expected = checksum(FACT_DOMAIN, &record.as_bytes()[..FACT_PAYLOAD_BYTES]);
    if record.checksum != expected {
        return Err(PublicationOpenError::FactChecksum {
            expected,
            observed: record.checksum,
        });
    }
    Ok(ParsedFact {
        input: PublicationInput {
            root: record.root,
            dep_set: record.dep_set,
            output: record.output,
            key: StageKey::from(record.key),
        },
        receipt: ReceiptFacts {
            sequence: FrameSequence::from(record.sequence.get()),
            durable_end: JournalOffset::from(record.durable_end.get()),
        },
        identity: ImmutablePublicationIdentity {
            checksum: record.checksum,
        },
        bytes,
    })
}

fn parse_head(bytes: [u8; HEAD_BYTES]) -> Result<ParsedHead, PublicationOpenError> {
    let record: HeadRecord = zerocopy::transmute!(bytes);
    if record.magic != HEAD_MAGIC
        || record.version.get() != PUBLICATION_VERSION
        || record.reserved.get() != 0
    {
        return Err(PublicationOpenError::EncodingMismatch);
    }
    let expected = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    if record.checksum != expected {
        return Err(PublicationOpenError::HeadChecksum {
            expected,
            observed: record.checksum,
        });
    }
    Ok(ParsedHead {
        identity: PublicationHeadIdentity {
            checksum: record.checksum,
        },
        fact_checksum: record.fact_checksum,
        receipt: ReceiptFacts {
            sequence: FrameSequence::from(record.sequence.get()),
            durable_end: JournalOffset::from(record.durable_end.get()),
        },
        bytes,
    })
}

struct PersistedFact {
    identity: ImmutablePublicationIdentity,
}

struct PersistedHead {
    identity: PublicationHeadIdentity,
}

fn persist_fact(
    paths: &PublicationPaths,
    input: PublicationInput,
    receipt: ReceiptFacts,
    bytes: &mut [u8; FACT_BYTES],
) -> Result<PersistedFact, PublicationFailure> {
    let mut record = FactRecord {
        magic: FACT_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        root: input.root,
        dep_set: input.dep_set,
        output: input.output,
        key: *input.key,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(FACT_DOMAIN, &record.as_bytes()[..FACT_PAYLOAD_BYTES]);
    bytes.copy_from_slice(record.as_bytes());
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.fact())
    {
        Ok(mut file) => {
            file.write_all(bytes)
                .map_err(|source| PublicationFailure::Io {
                    step: PublicationIoStep::WriteFact,
                    source,
                })?;
            file.sync_all().map_err(|source| PublicationFailure::Io {
                step: PublicationIoStep::SyncFact,
                source,
            })?;
            sync_parent_directory(
                &paths.fact(),
                PublicationIoStep::OpenFactParent,
                PublicationIoStep::SyncFactParent,
            )?;
            Ok(PersistedFact {
                identity: ImmutablePublicationIdentity {
                    checksum: record.checksum,
                },
            })
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let parsed = read_fact(&paths.fact()).map_err(PublicationFailure::Open)?;
            let Some(parsed) = parsed else {
                return Err(PublicationFailure::Open(PublicationOpenError::MissingFact));
            };
            if parsed.bytes != *bytes {
                return Err(conflict(input, parsed.input));
            }
            Ok(PersistedFact {
                identity: parsed.identity,
            })
        }
        Err(source) => Err(PublicationFailure::Io {
            step: PublicationIoStep::CreateFact,
            source,
        }),
    }
}

fn persist_head(
    paths: &PublicationPaths,
    fact: ImmutablePublicationIdentity,
    receipt: ReceiptFacts,
    bytes: &mut [u8; HEAD_BYTES],
) -> Result<PersistedHead, PublicationFailure> {
    let mut record = HeadRecord {
        magic: HEAD_MAGIC,
        version: U16::new(PUBLICATION_VERSION),
        reserved: U16::new(0),
        fact_checksum: fact.checksum,
        sequence: U64::new(*receipt.sequence),
        durable_end: U64::new(*receipt.durable_end),
        checksum: [0; CHECKSUM_BYTES],
    };
    record.checksum = checksum(HEAD_DOMAIN, &record.as_bytes()[..HEAD_PAYLOAD_BYTES]);
    bytes.copy_from_slice(record.as_bytes());
    match read_head(&paths.head()) {
        Ok(Some(existing)) => {
            if existing.bytes != *bytes {
                return Err(PublicationFailure::Open(
                    PublicationOpenError::HeadLinkMismatch,
                ));
            }
            return Ok(PersistedHead {
                identity: existing.identity,
            });
        }
        Ok(None) => {}
        Err(error) => return Err(PublicationFailure::Open(error)),
    }
    if path_exists(&paths.head_temp(), PublicationIoStep::InspectHeadTemp)
        .map_err(PublicationFailure::Open)?
    {
        return Err(PublicationFailure::HeadTempPresent);
    }
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.head_temp())
        .map_err(|source| PublicationFailure::Io {
            step: PublicationIoStep::CreateHeadTemp,
            source,
        })?;
    temp.write_all(bytes)
        .map_err(|source| PublicationFailure::Io {
            step: PublicationIoStep::WriteHead,
            source,
        })?;
    temp.sync_all().map_err(|source| PublicationFailure::Io {
        step: PublicationIoStep::SyncHead,
        source,
    })?;
    fs::rename(paths.head_temp(), paths.head()).map_err(|source| PublicationFailure::Io {
        step: PublicationIoStep::RenameHead,
        source,
    })?;
    sync_parent_directory(
        &paths.head(),
        PublicationIoStep::OpenHeadParent,
        PublicationIoStep::SyncHeadParent,
    )?;
    Ok(PersistedHead {
        identity: PublicationHeadIdentity {
            checksum: record.checksum,
        },
    })
}

fn sync_parent_directory(
    path: &Path,
    open_step: PublicationIoStep,
    sync_step: PublicationIoStep,
) -> Result<(), PublicationFailure> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = File::open(parent).map_err(|source| PublicationFailure::Io {
        step: open_step,
        source,
    })?;
    directory
        .sync_all()
        .map_err(|source| PublicationFailure::Io {
            step: sync_step,
            source,
        })
}

fn checksum(domain: &[u8], bytes: &[u8]) -> [u8; CHECKSUM_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(domain);
    hasher.update(bytes);
    let mut output = [0; CHECKSUM_BYTES];
    output.copy_from_slice(&hasher.finalize().as_bytes()[..CHECKSUM_BYTES]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credit_lease_retains_capacity_until_command_and_pending_drop() {
        let pool = CreditPool::new(1);
        let lease = pool.reserve().expect("one bounded lease");
        let command_lease = Arc::clone(&lease);
        let pending = PendingLease(Arc::clone(&lease));
        drop(lease);

        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        drop(pending);
        assert!(command_lease.is_cancelled());
        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        drop(command_lease);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn committing_lease_rejects_cancel_until_terminal_completion() {
        let pool = CreditPool::new(1);
        let lease = pool.reserve().expect("one bounded lease");

        assert!(lease.claim());
        assert!(!lease.cancel());
        lease.complete();
        assert!(!lease.is_cancelled());
        drop(lease);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn active_counter_is_the_only_admission_owner() {
        let pool = CreditPool::new(1);
        let lease = pool.reserve().expect("one bounded lease");
        assert!(pool.reserve().is_none());
        drop(lease);
        assert!(pool.reserve().is_some());
    }
}
