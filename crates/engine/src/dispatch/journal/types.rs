//! Versioned records for durable remote-dispatch attempts.
//!
//! Every record carries a version and a tag, fixed-width identities are never
//! length encoded, and variable fields use one bounded u32 length prefix.
//! The record is self-delimiting and independent from live daemon types.

use crate::journal::JournalLimits;
use std::fmt;

/// Current dispatch-record grammar version.
pub const DISPATCH_RECORD_VERSION: u8 = 1;
/// Maximum request bytes accepted by the codec.
pub const MAX_DISPATCH_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum result-proof bytes accepted by the codec.
pub const MAX_DISPATCH_PROOF_BYTES: usize = 4 * 1024 * 1024;
/// Maximum notification-cursor bytes accepted by the codec.
pub const MAX_DISPATCH_CURSOR_BYTES: usize = 64 * 1024;

/// Bounds for one dispatch journal and its replay fold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchJournalLimits {
    /// Maximum number of frames scanned from the journal.
    pub max_frames: usize,
    /// Maximum bytes scanned from the journal.
    pub max_bytes: usize,
    /// Maximum number of attempts retained by the replay fold.
    pub max_attempts: usize,
    /// Maximum request envelope bytes per admitted attempt.
    pub max_request_bytes: usize,
    /// Maximum accepted proof bytes per attempt.
    pub max_proof_bytes: usize,
    /// Maximum waiter/subscription cursor bytes per attempt.
    pub max_cursor_bytes: usize,
}

impl Default for DispatchJournalLimits {
    fn default() -> Self {
        Self {
            max_frames: 1_000_000,
            max_bytes: 256 * 1024 * 1024,
            max_attempts: 16_384,
            max_request_bytes: MAX_DISPATCH_REQUEST_BYTES,
            max_proof_bytes: MAX_DISPATCH_PROOF_BYTES,
            max_cursor_bytes: MAX_DISPATCH_CURSOR_BYTES,
        }
    }
}

impl DispatchJournalLimits {
    /// Returns the generic scanner limits for this dispatch journal.
    #[must_use]
    pub const fn journal_limits(self) -> JournalLimits {
        JournalLimits {
            max_frames: self.max_frames,
            max_bytes: self.max_bytes,
        }
    }

    /// Checks dispatch-specific bounds before opening a journal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn validate(self) -> Result<(), DispatchRecordError> {
        let defaults = Self::default();
        if self.max_frames == 0
            || self.max_frames > defaults.max_frames
            || self.max_bytes == 0
            || self.max_bytes > defaults.max_bytes
            || self.max_attempts == 0
            || self.max_attempts > defaults.max_attempts
            || self.max_request_bytes > defaults.max_request_bytes
            || self.max_proof_bytes > defaults.max_proof_bytes
            || self.max_cursor_bytes > defaults.max_cursor_bytes
        {
            return Err(DispatchRecordError::Bounds);
        }
        Ok(())
    }

    pub(crate) fn accepts(&self, record: &DispatchRecord) -> Result<(), DispatchRecordError> {
        let (variable, limit) = match record {
            DispatchRecord::Admitted { intent } => (intent.request.len(), self.max_request_bytes),
            DispatchRecord::Accepted { proof, .. } => {
                let total = proof
                    .proof
                    .len()
                    .checked_add(proof.provenance.len())
                    .ok_or(DispatchRecordError::Bounds)?;
                (total, self.max_proof_bytes)
            }
            DispatchRecord::Cursor { cursor } => {
                (cursor.position.cursor.len(), self.max_cursor_bytes)
            }
            DispatchRecord::PublishedWithCursor { cursor, .. } => {
                (cursor.cursor.len(), self.max_cursor_bytes)
            }
            DispatchRecord::Transfer { .. }
            | DispatchRecord::Published { .. }
            | DispatchRecord::PublicationPending { .. }
            | DispatchRecord::Terminal { .. }
            | DispatchRecord::Fenced { .. } => (0, usize::MAX),
        };
        if variable > limit {
            Err(DispatchRecordError::Bounds)
        } else {
            Ok(())
        }
    }
}

/// Stable identity of one remote attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DispatchAttemptKey {
    /// Semantic work identity.
    pub work_key: [u8; 32],
    /// One-based attempt ordinal.
    pub attempt: u64,
}

impl DispatchAttemptKey {
    /// Creates an attempt key, rejecting the impossible zero ordinal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub const fn new(work_key: [u8; 32], attempt: u64) -> Result<Self, DispatchRecordError> {
        if attempt == 0 {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        Ok(Self { work_key, attempt })
    }
}

/// Phase of the durable remote-attempt state machine.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum DispatchPhase {
    /// The owner admitted an intent but has not recorded transfer progress.
    Admitted = 1,
    /// Request or dependency transfer is in progress.
    Transferring = 2,
    /// The worker is executing under the admitted lease.
    Executing = 3,
    /// A result proof is durable and awaits owner publication.
    ResultAccepted = 4,
    /// The owner publication call is in flight or being retried.
    PublicationPending = 5,
    /// The owner acknowledged publication.
    Published = 6,
    /// The caller cancelled the attempt.
    Cancelled = 7,
    /// Local fallback became the terminal route.
    Fallback = 8,
    /// Remote publication rights were fenced during takeover or restart.
    Fenced = 9,
}

impl DispatchPhase {
    /// Decodes a stable phase tag.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub const fn from_tag(tag: u8) -> Result<Self, DispatchRecordError> {
        match tag {
            1 => Ok(Self::Admitted),
            2 => Ok(Self::Transferring),
            3 => Ok(Self::Executing),
            4 => Ok(Self::ResultAccepted),
            5 => Ok(Self::PublicationPending),
            6 => Ok(Self::Published),
            7 => Ok(Self::Cancelled),
            8 => Ok(Self::Fallback),
            9 => Ok(Self::Fenced),
            _ => Err(DispatchRecordError::UnknownPhase),
        }
    }

    /// Returns whether this phase has no further remote transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Cancelled | Self::Fallback)
    }

    /// Returns whether a remote request may be rebound after restart.
    #[must_use]
    pub const fn may_resend(self) -> bool {
        matches!(self, Self::Admitted | Self::Transferring | Self::Executing)
    }
}

/// Immutable remote intent retained before a request leaves the process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAttemptIntent {
    /// Attempt identity.
    pub key: DispatchAttemptKey,
    /// Canonical bounded request envelope.
    pub request: Box<[u8]>,
    /// Workspace root selected by the owner that admitted the attempt.
    pub root: [u8; 32],
    /// Full-width owner fence that admitted the attempt.
    pub fence: [u8; 32],
    /// Owner authority epoch that admitted the attempt.
    pub owner_epoch: u64,
    /// Authority revocation version observed at admission.
    pub revocation_version: u64,
    /// Remote lease expiry in the owner logical clock.
    pub lease_until: u64,
    /// Latest local-fallback deadline; zero means no promised deadline.
    pub deadline: u64,
    /// Initial phase. New intents begin in Admitted.
    pub phase: DispatchPhase,
}

impl RemoteAttemptIntent {
    /// Creates one admitted remote intent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new(
        key: DispatchAttemptKey,
        request: Box<[u8]>,
        root: [u8; 32],
        fence: [u8; 32],
        owner_epoch: u64,
        lease_until: u64,
        deadline: u64,
    ) -> Result<Self, DispatchRecordError> {
        if request.len() > MAX_DISPATCH_REQUEST_BYTES {
            return Err(DispatchRecordError::Bounds);
        }
        if fence == [0; 32] {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        Ok(Self {
            key,
            request,
            root,
            fence,
            owner_epoch,
            revocation_version: 0,
            lease_until,
            deadline,
            phase: DispatchPhase::Admitted,
        })
    }

    /// Binds the authority revocation version observed with the intent.
    #[must_use]
    pub const fn with_revocation_version(mut self, revocation_version: u64) -> Self {
        self.revocation_version = revocation_version;
        self
    }
}

/// Transfer/checkpoint progress for one remote attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferCheckpointRef {
    /// Current transfer identity.
    pub transfer: [u8; 32],
    /// Durable dependency/checkpoint identity.
    pub checkpoint: [u8; 32],
    /// Bytes known to be durably transferred.
    pub transferred_bytes: u64,
    /// Resume cursor within the transfer.
    pub cursor: u64,
    /// Phase reached by this progress record.
    pub phase: DispatchPhase,
}

impl TransferCheckpointRef {
    /// Creates checked transfer progress.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub const fn new(
        transfer: [u8; 32],
        checkpoint: [u8; 32],
        transferred_bytes: u64,
        cursor: u64,
        phase: DispatchPhase,
    ) -> Result<Self, DispatchRecordError> {
        if !matches!(
            phase,
            DispatchPhase::Transferring | DispatchPhase::Executing
        ) {
            return Err(DispatchRecordError::IllegalTransition);
        }
        Ok(Self {
            transfer,
            checkpoint,
            transferred_bytes,
            cursor,
            phase,
        })
    }
}

/// Accepted result proof retained until owner publication is acknowledged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedResultProof {
    /// Immutable output identity.
    pub output_root: [u8; 32],
    /// Content-addressed immutable output object staged before admission.
    ///
    /// A zero value is retained for records produced by callers that do not
    /// have a derived-output catalog (for example a one-shot test route). The
    /// daemon's catalog-backed path always fills both object references.
    pub output_object: [u8; 32],
    /// Content-addressed dependency-manifest object staged with the output.
    pub manifest_object: [u8; 32],
    /// Canonical proof or receipt envelope.
    pub proof: Box<[u8]>,
    /// Versioned, self-delimiting provenance for reconstructing the checked
    /// derived-output proof after a process restart.
    pub provenance: Box<[u8]>,
    /// Owner clock at result admission.
    pub accepted_at: u64,
}

impl AcceptedResultProof {
    /// Creates a bounded accepted-proof envelope.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new(
        output_root: [u8; 32],
        proof: Box<[u8]>,
        accepted_at: u64,
    ) -> Result<Self, DispatchRecordError> {
        if proof.len() > MAX_DISPATCH_PROOF_BYTES {
            return Err(DispatchRecordError::Bounds);
        }
        Ok(Self {
            output_root,
            output_object: [0; 32],
            manifest_object: [0; 32],
            proof,
            provenance: Box::new([]),
            accepted_at,
        })
    }

    /// Attaches the immutable CAS references and checked provenance envelope
    /// produced by the owner staging path.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn with_staged_objects(
        mut self,
        output_object: [u8; 32],
        manifest_object: [u8; 32],
        provenance: Box<[u8]>,
    ) -> Result<Self, DispatchRecordError> {
        if output_object == [0; 32]
            || manifest_object == [0; 32]
            || provenance.is_empty()
            || provenance.len() > MAX_DISPATCH_PROOF_BYTES
        {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        self.output_object = output_object;
        self.manifest_object = manifest_object;
        self.provenance = provenance;
        Ok(self)
    }

    /// Returns whether the proof carries reconstructible staged-object
    /// references. This is intentionally structural; semantic validation of
    /// the provenance bytes remains owned by the catalog/owner boundary.
    #[must_use]
    pub fn has_staged_objects(&self) -> bool {
        self.output_object != [0; 32]
            && self.manifest_object != [0; 32]
            && !self.provenance.is_empty()
    }
}

/// Owner acknowledgement that an accepted result is durably published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationAck {
    /// Published output identity.
    pub output_root: [u8; 32],
    /// Owner root or transaction acknowledgement identity.
    pub publication_root: [u8; 32],
    /// Owner epoch that acknowledged publication.
    pub owner_epoch: u64,
    /// Monotonic notification cursor at publication.
    pub notification_cursor: u64,
}

/// Terminal cancellation or local-fallback outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalState {
    /// Caller or authority cancelled the remote attempt.
    Cancelled {
        /// Stable reason code owned by the caller.
        reason: u16,
        /// Last notification cursor observed by the owner.
        notification_cursor: u64,
    },
    /// Owner fenced remote work and selected local execution.
    Fallback {
        /// Local output identity when one is already known; zero means pending.
        output_root: [u8; 32],
        /// Stable reason code owned by the caller.
        reason: u16,
        /// Last notification cursor observed by the owner.
        notification_cursor: u64,
    },
}

impl TerminalState {
    /// Returns the phase represented by this terminal value.
    #[must_use]
    pub const fn phase(self) -> DispatchPhase {
        match self {
            Self::Cancelled { .. } => DispatchPhase::Cancelled,
            Self::Fallback { .. } => DispatchPhase::Fallback,
        }
    }
}

/// Durable waiter and subscription notification position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotificationCursor {
    /// Waiter/follower sequence acknowledged by the owner.
    pub waiter: u64,
    /// Subscription sequence acknowledged by the owner.
    pub subscription: u64,
    /// Opaque versioned cursor bytes.
    pub cursor: Box<[u8]>,
}

impl NotificationCursor {
    /// Creates a bounded cursor envelope.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new(
        waiter: u64,
        subscription: u64,
        cursor: Box<[u8]>,
    ) -> Result<Self, DispatchRecordError> {
        if cursor.len() > MAX_DISPATCH_CURSOR_BYTES {
            return Err(DispatchRecordError::Bounds);
        }
        Ok(Self {
            waiter,
            subscription,
            cursor,
        })
    }
}

/// One canonical durable dispatch transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchRecord {
    /// Admission of the immutable remote request intent.
    Admitted {
        /// Intent payload.
        intent: RemoteAttemptIntent,
    },
    /// Transfer/checkpoint progress.
    Transfer {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// Progress payload.
        progress: TransferCheckpointRef,
    },
    /// Accepted result proof. This must precede publication acknowledgement.
    Accepted {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// Proof retained for retryable owner publication.
        proof: AcceptedResultProof,
    },
    /// Durable owner publication acknowledgement.
    Published {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// Publication payload.
        ack: PublicationAck,
    },
    /// Owner publication acknowledgement and its waiter/subscription cursor
    /// committed as one durable frame.
    ///
    /// Recovery uses this fused record when an accepted result is being
    /// replayed. Keeping the cursor in the same frame closes the crash window
    /// between publishing the owner head and recording notification progress:
    /// a recovered attempt is either still pending or has both durable facts.
    PublishedWithCursor {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// Publication payload.
        ack: PublicationAck,
        /// Exact waiter/subscription position observed at publication.
        cursor: NotificationCursor,
    },
    /// Owner publication has begun after the accepted proof was synced.
    PublicationPending {
        /// Attempt identity.
        key: DispatchAttemptKey,
    },
    /// Terminal cancellation or local fallback.
    Terminal {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// Terminal payload.
        terminal: TerminalState,
    },
    /// Waiter/subscription notification cursor update.
    Cursor {
        /// Attempt identity and cursor payload.
        cursor: DispatchCursor,
    },
    /// Explicit remote fencing during takeover or restart.
    Fenced {
        /// Attempt identity.
        key: DispatchAttemptKey,
        /// New owner epoch observing the fence.
        owner_epoch: u64,
        /// Replacement fence minted by the current owner.
        fence: [u8; 32],
        /// Stable fence reason code.
        reason: u16,
    },
}

/// Cursor record with its attempt identity adjacent to the cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchCursor {
    /// Attempt identity.
    pub key: DispatchAttemptKey,
    /// Cursor payload.
    pub position: NotificationCursor,
}

impl DispatchRecord {
    /// Returns the attempt identity touched by this record.
    #[must_use]
    pub const fn key(&self) -> DispatchAttemptKey {
        match self {
            Self::Admitted { intent } => intent.key,
            Self::Transfer { key, .. }
            | Self::Accepted { key, .. }
            | Self::Published { key, .. }
            | Self::PublishedWithCursor { key, .. }
            | Self::PublicationPending { key }
            | Self::Terminal { key, .. }
            | Self::Fenced { key, .. } => *key,
            Self::Cursor { cursor } => cursor.key,
        }
    }
}

/// Error in a dispatch record grammar or state-independent bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchRecordError {
    /// A length, count, or configured bound was exceeded.
    Bounds,
    /// A variable field did not contain enough bytes.
    Truncated,
    /// A field carried an invalid stable tag.
    InvalidTag,
    /// The record version is newer than this binary.
    UnsupportedVersion,
    /// An attempt ordinal, fence, or other required identity was zero.
    InvalidIdentifier,
    /// A phase tag was not recognized.
    UnknownPhase,
    /// A transition is not legal for the current phase.
    IllegalTransition,
    /// A record repeated an identity with different immutable content.
    IdentityCollision,
    /// A record was authenticated but referred to an unknown attempt.
    UnknownAttempt,
    /// A publication acknowledgement did not match the retained proof.
    PublicationMismatch,
    /// A terminal record did not match its phase or existing terminal value.
    TerminalMismatch,
    /// A record appeared after publication or terminal completion.
    AlreadyTerminal,
}

impl fmt::Display for DispatchRecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Bounds => "dispatch record exceeds bounds",
            Self::Truncated => "truncated dispatch record",
            Self::InvalidTag => "invalid dispatch record tag",
            Self::UnsupportedVersion => "unsupported dispatch record version",
            Self::InvalidIdentifier => "invalid dispatch identity",
            Self::UnknownPhase => "unknown dispatch phase",
            Self::IllegalTransition => "illegal dispatch transition",
            Self::IdentityCollision => "dispatch identity collision",
            Self::UnknownAttempt => "unknown dispatch attempt",
            Self::PublicationMismatch => "publication does not match accepted proof",
            Self::TerminalMismatch => "terminal dispatch state mismatch",
            Self::AlreadyTerminal => "dispatch attempt is already terminal",
        };
        f.write_str(message)
    }
}

impl std::error::Error for DispatchRecordError {}
