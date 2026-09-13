//! Transport and admission error taxonomy.

use std::fmt;

use backend_version::{IdAdmissionError, IdDecodeError};

/// Errors raised while admitting or transporting replication data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicationError {
    /// No common protocol version exists.
    NoCommonVersion,
    /// No common schema exists.
    NoCommonSchema,
    /// No common recipe or capability exists.
    NoCommonRecipe,
    /// A version interval is malformed.
    InvalidVersionRange,
    /// A transport budget is zero or internally inconsistent.
    InvalidLimits,
    /// Capability advertisement is malformed.
    InvalidCapabilities,
    /// A transfer or attempt identifier is zero.
    InvalidIdentifier,
    /// A chunk exceeds the negotiated limit.
    ChunkTooLarge,
    /// An object exceeds the negotiated limit.
    ObjectTooLarge,
    /// A message exceeds the negotiated limit.
    MessageTooLarge,
    /// A compute declaration exceeds the negotiated worker envelope.
    ResourceLimit,
    /// A coverage interval budget was exceeded.
    CoverageLimit,
    /// Arithmetic overflow was detected.
    Overflow,
    /// A range is malformed or outside the object.
    Range,
    /// A canonical sequence or collection is not sorted.
    Unsorted,
    /// A duplicate key or read was supplied.
    DuplicateKey,
    /// A frame chain or payload was corrupted.
    CorruptFrame,
    /// A versioned envelope or field tag is outside the wire grammar.
    InvalidWire,
    /// The peer used a protocol envelope version this crate does not support.
    UnsupportedWireVersion,
    /// A complete outer frame was not available yet.
    TruncatedFrame,
    /// Bytes remained after one complete canonical envelope.
    TrailingFrame,
    /// The message tag is not recognized by this protocol version.
    UnknownMessage,
    /// A claim used an incompatible backend-version context.
    IdentityContext,
    /// A claim has a valid context but no canonical preimage was supplied for
    /// backend-version digest verification.
    UnverifiedIdentity,
    /// A claimed identity differed from the requested identity.
    IdentityMismatch,
    /// An attestation was absent or failed the caller-owned verifier.
    InvalidAttestation,
    /// A publishable result required an attestation but none was supplied.
    AttestationRequired,
    /// The verifier classified a statement as diagnostic-only evidence.
    UntrustedAttestation,
    /// A frame or result belongs to an old transfer/attempt fence.
    StaleFence,
    /// An authority epoch is too old for the policy.
    StaleAuthority,
    /// A revocation observation is too old or the authority is revoked.
    RevokedAuthority,
    /// A transfer does not cover the complete object.
    Incomplete,
    /// An identical sequence was replayed with different bytes.
    ReplayConflict,
    /// The local queue is full.
    Backpressure,
    /// The received message is not a frame.
    WrongMessage,
    /// The transport endpoint or lock is unavailable.
    Disconnected,
}
impl fmt::Display for ReplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "replication error: {self:?}")
    }
}
impl std::error::Error for ReplicationError {}
impl From<IdAdmissionError> for ReplicationError {
    fn from(error: IdAdmissionError) -> Self {
        match error {
            IdAdmissionError::ContextMismatch { .. } => Self::IdentityContext,
            IdAdmissionError::UnverifiedDigest => Self::UnverifiedIdentity,
            IdAdmissionError::DigestMismatch => Self::IdentityMismatch,
        }
    }
}
impl From<IdDecodeError> for ReplicationError {
    fn from(_: IdDecodeError) -> Self {
        Self::IdentityContext
    }
}
