//! Core types for ir-sync: change identifiers, channel references, and protocol messages.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The 64-hex-character Blake3 hash of a pijul change file.
///
/// Mirrors `ChangeHashHex` from `nudox-ir-vcs` by value (same 64-lowercase-hex
/// representation); this crate carries no dependency on that crate so the two
/// types are independent newtypes with a shared contract.
///
/// The hash is the pijul content hash of the change, recomputable from the change
/// file bytes by libpijul itself. It is the trust anchor: if the bytes hash to this
/// id, the change is genuine.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeId(pub(crate) String);

impl ChangeId {
    /// Parse without allocating an intermediate `String`.
    fn validate(s: &str) -> Result<(), VerifyError> {
        if s.len() != 64 {
            return Err(VerifyError::InvalidLength {
                expected: 64,
                got: s.len(),
            });
        }
        for ch in s.chars() {
            if !ch.is_ascii_hexdigit() || ch.is_ascii_uppercase() {
                return Err(VerifyError::InvalidHexChar(ch));
            }
        }
        Ok(())
    }

    /// Borrow the hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ChangeId {
    type Err = VerifyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(ChangeId(s.to_string()))
    }
}

impl TryFrom<&str> for ChangeId {
    type Error = VerifyError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<String> for ChangeId {
    type Error = VerifyError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        ChangeId::validate(&s)?;
        Ok(ChangeId(s))
    }
}

/// A validated, non-empty package name (e.g. `"my-org/my-package"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageName(String);

impl PackageName {
    /// Create a validated package name.
    ///
    /// Returns an error if the name is empty or contains characters outside
    /// `[A-Za-z0-9_\-./]`.
    pub fn new(s: impl Into<String>) -> Result<Self, SyncError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SyncError::InvalidPackageName(
                "package name must not be empty".into(),
            ));
        }
        if !s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
            return Err(SyncError::InvalidPackageName(format!(
                "package name {s:?} contains invalid characters"
            )));
        }
        Ok(PackageName(s))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for PackageName {
    type Error = SyncError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        PackageName::new(s)
    }
}

/// Identifies a libpijul channel on a specific package.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelRef {
    /// The package that owns this channel.
    pub package: PackageName,
    /// The channel name (e.g. `"main"`, `"release-2.0"`).
    pub channel: String,
}

/// An announcement that a set of changes has been merged onto a channel,
/// advancing it to a new tip.
///
/// The `changes` list is in dependency order (apply in order; no change in the
/// list depends on a later one). Each entry carries both the pijul change hash
/// (`ChangeId`) and the iroh-blobs BLAKE3 hash (`iroh_blobs::Hash`) so the
/// receiver can (a) fetch by iroh hash and (b) verify by pijul hash.
///
/// The iroh hash ≠ pijul hash: BLAKE3 of the raw bytes is what iroh-blobs
/// computes; pijul's hash is over the canonicalized change structure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipAnnouncement {
    /// The channel that was updated.
    pub channel: ChannelRef,
    /// The new tip after applying all `changes`.
    pub tip: ChangeId,
    /// Ordered list of `(pijul_hash, iroh_blob_hash)` pairs. Apply in order.
    /// The iroh hash is the raw-bytes BLAKE3 as produced by `iroh-blobs`.
    pub changes: Vec<(ChangeId, IrohHash)>,
}

/// A newtype wrapper around the raw 32-byte BLAKE3 hash as produced by iroh-blobs.
///
/// We keep this as raw bytes in postcard encoding rather than pulling in the full
/// `iroh_blobs` type graph into the protocol layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrohHash(pub [u8; 32]);

impl From<iroh_blobs::Hash> for IrohHash {
    fn from(h: iroh_blobs::Hash) -> Self {
        IrohHash(*h.as_bytes())
    }
}

impl From<IrohHash> for iroh_blobs::Hash {
    fn from(h: IrohHash) -> Self {
        iroh_blobs::Hash::from_bytes(h.0)
    }
}

/// Acknowledgement sent by the receiver after successfully applying all changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAck {
    /// The confirmed new tip after applying all announced changes.
    pub tip: ChangeId,
    /// Number of changes that were actually fetched and written (0 if all were
    /// already present — idempotent re-sync).
    pub applied: u64,
}

/// An error in change-id parsing or verification.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The hex string was not exactly 64 characters.
    #[error("expected 64-char hex ChangeId, got {got} chars")]
    InvalidLength { expected: usize, got: usize },

    /// A character in the hex string was not a lowercase hex digit.
    #[error("invalid hex character in ChangeId: {0:?}")]
    InvalidHexChar(char),

    /// The BLAKE3 hash of the raw bytes did not match the announced ChangeId.
    ///
    /// Note: full pijul-hash verification (which is the gold standard) lives in
    /// the repo-side `ChangeIo` implementation over `FsChanges`. This variant
    /// covers only the BLAKE3 prefix used by the test double.
    #[error("hash mismatch: expected {expected}, computed {got}")]
    HashMismatch { expected: ChangeId, got: String },

    /// The change bytes exceeded the per-blob size cap.
    #[error("change {id} is too large: {size} bytes (max {max})")]
    TooLarge { id: ChangeId, size: usize, max: usize },
}

/// Top-level error type for sync operations.
#[derive(Debug, Error)]
pub enum SyncError {
    /// A change failed pijul hash verification. NOTHING is written past this point.
    #[error("verification failed: {0}")]
    VerificationFailed(#[from] VerifyError),

    /// A transport / QUIC error.
    #[error("transport error: {0}")]
    Transport(String),

    /// A single change blob exceeds the configured size cap.
    #[error("change {id} exceeds size cap: {size} > {max} bytes")]
    ChangeTooLarge {
        id: ChangeId,
        size: usize,
        max: usize,
    },

    /// Could not connect to the remote node.
    #[error("connection to remote failed")]
    ConnectionFailed,

    /// The operation timed out.
    #[error("sync timed out")]
    Timeout,

    /// The remote node explicitly refused the sync.
    #[error("remote refused: {0}")]
    RemoteRefused(String),

    /// An I/O error from the local `ChangeIo` store.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Encoding / decoding error.
    #[error("codec error: {0}")]
    Codec(String),

    /// The sender does not have a change it announced.
    #[error("sender is missing announced change {0}")]
    MissingChange(ChangeId),

    /// Invalid package name.
    #[error("invalid package name: {0}")]
    InvalidPackageName(String),
}

/// The event that triggers a push: a commit was merged onto a durable channel.
///
/// Construct this in the merge hook (e.g. inside `IrRepository::record_generation`'s
/// completion path) and pass it to [`crate::Syncer::on_merge`].
#[derive(Clone, Debug)]
pub struct MergeEvent {
    /// The channel that received the new changes.
    pub channel: ChannelRef,
    /// The new tip after all `new_changes` have been applied.
    pub tip: ChangeId,
    /// The change hashes that were merged, in dependency order (apply in order).
    pub new_changes: Vec<ChangeId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_id_parse_valid() {
        let hex = "a".repeat(64);
        let id: ChangeId = hex.parse().unwrap();
        assert_eq!(id.as_str(), &hex);
        assert_eq!(id.to_string(), hex);
    }

    #[test]
    fn change_id_parse_invalid_len() {
        let err = "abc".parse::<ChangeId>().unwrap_err();
        assert!(matches!(err, VerifyError::InvalidLength { .. }));
    }

    #[test]
    fn change_id_parse_uppercase() {
        let hex = "A".repeat(64);
        let err = hex.parse::<ChangeId>().unwrap_err();
        assert!(matches!(err, VerifyError::InvalidHexChar('A')));
    }

    #[test]
    fn package_name_valid() {
        PackageName::new("org/pkg-1.0").unwrap();
        PackageName::new("simple").unwrap();
    }

    #[test]
    fn package_name_empty() {
        PackageName::new("").unwrap_err();
    }

    #[test]
    fn package_name_invalid_chars() {
        PackageName::new("pkg with space").unwrap_err();
    }
}
