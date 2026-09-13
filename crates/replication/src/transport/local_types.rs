//! Typed local control DTOs and bounded subscription state.

use super::{
    LOCAL_CONTROL_MAX_CURSOR, LOCAL_CONTROL_MAX_ERROR, LOCAL_CONTROL_MAX_FRAME,
    SUBSCRIPTION_ID_BYTES,
};
use std::io;

/// Limits shared by the local outer frame and its control payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalControlLimits {
    /// Maximum body bytes in one four-byte length-prefixed frame.
    pub max_frame: usize,
    /// Maximum cursor bytes in one subscribe request.
    pub max_cursor: usize,
    /// Maximum UTF-8 diagnostic bytes in one rejected response.
    pub max_error: usize,
}

impl Default for LocalControlLimits {
    fn default() -> Self {
        Self {
            max_frame: LOCAL_CONTROL_MAX_FRAME,
            max_cursor: LOCAL_CONTROL_MAX_CURSOR,
            max_error: LOCAL_CONTROL_MAX_ERROR,
        }
    }
}

impl LocalControlLimits {
    /// Validates the limits before any frame or payload allocation.
    ///
    /// # Errors
    ///
    /// Returns [`LocalControlError::InvalidLimits`] when a limit is zero,
    /// cannot fit in the four-byte length prefix, or exceeds its containing
    /// frame budget.
    pub fn validate(self) -> Result<(), LocalControlError> {
        if self.max_frame == 0
            || self.max_frame > u32::MAX as usize
            || self.max_cursor > self.max_frame
            || self.max_error > self.max_frame
        {
            return Err(LocalControlError::InvalidLimits);
        }
        Ok(())
    }
}

/// Raw local control request. Typed daemon code admits the nested payload
/// after this byte-level envelope has passed its bounds and version checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalControlRequest {
    /// Replication payload with an explicit owner request correlation ID.
    Replicate {
        /// Correlation identity.
        request_id: u64,
        /// Versioned replication bytes.
        payload: Box<[u8]>,
    },
    /// Completion claim with fixed-width scheduler identities.
    Complete {
        /// Correlation identity.
        request_id: u64,
        /// Claimed work key.
        work_key: [u8; 32],
        /// Claimed output identity.
        output: [u8; 32],
        /// Claimed winning attempt ordinal.
        ordinal: u32,
        /// Claimed publication fence.
        fence: [u8; 32],
    },
    /// Bounded subscription request.
    Subscribe {
        /// Correlation identity.
        request_id: u64,
        /// Cursor bytes retained by the client.
        cursor: Box<[u8]>,
        /// Maximum events requested by the client.
        credit: usize,
    },
    /// A leased subscription lifecycle operation.
    ///
    /// The legacy [`Self::Subscribe`] variant remains available for bounded
    /// poll compatibility. New clients should use this variant so a daemon
    /// can retain the lease across replies and reconnects.
    Subscription(LocalSubscriptionRequest),
}

impl LocalControlRequest {
    /// Returns the request correlation identity.
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        match self {
            Self::Replicate { request_id, .. }
            | Self::Complete { request_id, .. }
            | Self::Subscribe { request_id, .. } => *request_id,
            Self::Subscription(request) => request.request_id(),
        }
    }
}

/// Opaque producer-issued identity for one durable subscription lease.
///
/// The local codec carries this token without interpreting it. A daemon may
/// bind it to a durable owner epoch, expiry, and cursor. A client can persist
/// and present the exact bytes after reconnect; it cannot derive a valid lease
/// from an arbitrary digest through this type.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct LocalSubscriptionId([u8; SUBSCRIPTION_ID_BYTES]);

impl LocalSubscriptionId {
    /// Creates a lease identity from producer-issued opaque bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SUBSCRIPTION_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the exact bytes to persist or place on the wire.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; SUBSCRIPTION_ID_BYTES] {
        self.0
    }

    pub(crate) const fn is_zero(self) -> bool {
        let mut index = 0;
        while index < SUBSCRIPTION_ID_BYTES {
            if self.0[index] != 0 {
                return false;
            }
            index += 1;
        }
        true
    }
}

/// Why a leased subscription needs a complete replacement root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalSubscriptionResetReason {
    /// The requested cursor fell across a missing event gap.
    Gap,
    /// The selected branch or log was discarded.
    BranchDiscarded,
    /// The requested cursor was pruned from durable history.
    Pruned,
    /// The cursor schema is no longer compatible.
    SchemaMismatch,
    /// The cursor root does not chain to the retained stream.
    RootMismatch,
}

/// One explicit operation in a durable leased subscription protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalSubscriptionOperation {
    /// Opens a new producer-owned lease from an optional exact cursor.
    Open {
        /// Cursor bytes; empty asks the owner for its current cursor.
        cursor: Box<[u8]>,
        /// Number of events the owner may return per batch.
        credit: usize,
        /// Requested lease duration in milliseconds.
        lease_ms: u64,
    },
    /// Reconnects an existing lease at the caller's durable cursor.
    Resume {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Exact cursor last durably observed by the client.
        cursor: Box<[u8]>,
        /// Number of events the owner may return per batch.
        credit: usize,
        /// Requested renewed lease duration in milliseconds.
        lease_ms: u64,
    },
    /// Adds event credit to an existing lease.
    Credit {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Additional events the owner may emit.
        credit: usize,
    },
    /// Acknowledges durable consumption through an exact cursor.
    Ack {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Last cursor durably applied by the client.
        cursor: Box<[u8]>,
    },
    /// Extends a lease while fencing it to the client's exact cursor.
    Renew {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Last cursor durably applied by the client.
        cursor: Box<[u8]>,
        /// Number of events the owner may return per batch after renewal.
        credit: usize,
        /// Requested renewed lease duration in milliseconds.
        lease_ms: u64,
    },
    /// Releases a lease and all associated demand/retention state.
    Cancel {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
    },
    /// Requests one bounded authenticated snapshot page for a reset lease.
    /// An empty page cursor asks for the first page; a non-empty cursor is the
    /// exact continuation returned by the prior page.
    Page {
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Opaque authenticated snapshot-page continuation.
        page: Box<[u8]>,
        /// Number of rows the owner may return.
        credit: usize,
    },
}

/// Correlated durable subscription request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSubscriptionRequest {
    /// Request/response correlation identity.
    pub request_id: u64,
    /// Lease lifecycle operation.
    pub operation: LocalSubscriptionOperation,
}

impl LocalSubscriptionRequest {
    /// Returns the request correlation identity.
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id
    }
}

/// Raw local control response. The typed locald status maps directly onto
/// this enum while desktop only consumes its subscription payload form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalControlResponse {
    /// Operation was accepted without a payload.
    Accepted {
        /// Correlation identity.
        request_id: u64,
    },
    /// Operation was accepted with an opaque typed payload.
    AcceptedPayload {
        /// Correlation identity.
        request_id: u64,
        /// Versioned payload bytes.
        payload: Box<[u8]>,
    },
    /// Operation was queued and retained this many bytes.
    Queued {
        /// Correlation identity.
        request_id: u64,
        /// Owner queue accounting bytes.
        bytes: usize,
    },
    /// Operation was rejected with a bounded UTF-8 diagnostic.
    Rejected {
        /// Correlation identity.
        request_id: u64,
        /// Bounded diagnostic text.
        message: String,
    },
    /// A durable subscription lifecycle response.
    Subscription(LocalSubscriptionResponse),
}

impl LocalControlResponse {
    /// Returns the response correlation identity.
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        match self {
            Self::Accepted { request_id }
            | Self::AcceptedPayload { request_id, .. }
            | Self::Queued { request_id, .. }
            | Self::Rejected { request_id, .. } => *request_id,
            Self::Subscription(response) => response.request_id(),
        }
    }
}

/// Correlated result of a durable subscription lifecycle operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalSubscriptionResponse {
    /// A new lease was issued at the owner's exact cursor.
    Opened {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Current owner cursor.
        cursor: Box<[u8]>,
        /// Credit retained by the owner.
        credit: usize,
        /// Lease duration granted in milliseconds.
        lease_ms: u64,
    },
    /// An existing lease was reattached after reconnect.
    Resumed {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Cursor from which the owner resumed.
        cursor: Box<[u8]>,
        /// Credit retained by the owner.
        credit: usize,
        /// Lease duration granted in milliseconds.
        lease_ms: u64,
    },
    /// One certified, contiguous batch for an existing lease.
    Batch {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Cursor before the encoded subscription payload.
        previous: Box<[u8]>,
        /// Cursor after the encoded subscription payload.
        cursor: Box<[u8]>,
        /// Credit remaining after this batch was emitted.
        credit: usize,
        /// Versioned `SubscriptionDto` bytes.
        payload: Box<[u8]>,
    },
    /// A complete checked replacement root for a pruned or unchainable lease.
    ResetWithRoot {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Replacement cursor associated with the root payload.
        cursor: Box<[u8]>,
        /// Credit retained after the reset.
        credit: usize,
        /// Explicit gap/pruning/schema reason.
        reason: LocalSubscriptionResetReason,
        /// Versioned `SubscriptionDto` reset bytes carrying the root/certificate.
        payload: Box<[u8]>,
    },
    /// Acknowledgement accepted through the exact cursor supplied by the client.
    Acked {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Cursor now durable at the owner.
        cursor: Box<[u8]>,
    },
    /// Lease renewal accepted at the exact cursor supplied by the client.
    Renewed {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Cursor fenced by the renewal.
        cursor: Box<[u8]>,
        /// Credit retained by the owner.
        credit: usize,
        /// Lease duration granted in milliseconds.
        lease_ms: u64,
    },
    /// Lease cancellation accepted.
    Cancelled {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
    },
    /// One bounded authenticated snapshot page for a reset descriptor.
    SnapshotPage {
        /// Correlation identity.
        request_id: u64,
        /// Producer-issued lease identity.
        lease: LocalSubscriptionId,
        /// Page continuation used for this response; empty denotes first.
        page: Box<[u8]>,
        /// Next page continuation, if more rows remain.
        next: Option<Box<[u8]>>,
        /// Credit retained after the page was emitted.
        credit: usize,
        /// Versioned descriptor/page DTO bytes.
        payload: Box<[u8]>,
    },
}

impl LocalSubscriptionResponse {
    /// Returns the response correlation identity.
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        match self {
            Self::Opened { request_id, .. }
            | Self::Resumed { request_id, .. }
            | Self::Batch { request_id, .. }
            | Self::ResetWithRoot { request_id, .. }
            | Self::Acked { request_id, .. }
            | Self::Renewed { request_id, .. }
            | Self::Cancelled { request_id, .. }
            | Self::SnapshotPage { request_id, .. } => *request_id,
        }
    }

    /// Returns the producer-issued lease bound to this response.
    #[must_use]
    pub const fn lease(&self) -> LocalSubscriptionId {
        match self {
            Self::Opened { lease, .. }
            | Self::Resumed { lease, .. }
            | Self::Batch { lease, .. }
            | Self::ResetWithRoot { lease, .. }
            | Self::Acked { lease, .. }
            | Self::Renewed { lease, .. }
            | Self::Cancelled { lease, .. }
            | Self::SnapshotPage { lease, .. } => *lease,
        }
    }
}

/// Failure in the shared local frame/control byte grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalControlError {
    /// The peer closed the stream before starting another frame.
    Closed,
    /// Limits cannot safely bound an allocation.
    InvalidLimits,
    /// A length or payload exceeds the configured frame bound.
    FrameTooLarge,
    /// A complete frame or control header was not available.
    Truncated,
    /// Bytes remained after one complete frame/control payload.
    Trailing,
    /// A control field/tag/status is outside this versioned grammar.
    Invalid(&'static str),
    /// A rejected diagnostic was not valid UTF-8.
    InvalidUtf8,
    /// Underlying stream I/O failed.
    Io(io::ErrorKind),
    /// Too many out-of-order responses are waiting for a caller.
    PendingLimit,
}

impl std::fmt::Display for LocalControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => formatter.write_str("local control stream closed"),
            Self::InvalidLimits => formatter.write_str("invalid local control limits"),
            Self::FrameTooLarge => formatter.write_str("local control frame exceeds its bound"),
            Self::Truncated => formatter.write_str("truncated local control frame"),
            Self::Trailing => formatter.write_str("trailing bytes after local control frame"),
            Self::Invalid(field) => write!(formatter, "invalid local control {field}"),
            Self::InvalidUtf8 => formatter.write_str("invalid local control diagnostic encoding"),
            Self::Io(kind) => write!(formatter, "local control I/O failed: {kind:?}"),
            Self::PendingLimit => {
                formatter.write_str("local control pending response limit reached")
            }
        }
    }
}

impl std::error::Error for LocalControlError {}
