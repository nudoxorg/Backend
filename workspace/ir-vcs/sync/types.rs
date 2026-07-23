//! Core types for nudox_ir::sync: change identifiers, channel references, and protocol messages.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use heart::sync::{SyncError, VerifyError};

/// The 64-hex-character Blake3 hash of a pijul change file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChangeId(pub(crate) String);

impl ChangeId {
    fn validate(s: &str) -> Result<(), VerifyError> {
        if s.len() != 64 {
            return Err(VerifyError::InvalidLength {
                expected: 64,
                got: s.len(),
            });
        }
        for ch in s.chars() {
            if !ch.is_ascii_hexdigit() || ch.is_ascii_uppercase() {
                return Err(VerifyError::InvalidChar(ch));
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
    pub fn new(s: impl Into<String>) -> Result<Self, SyncError> {
        let s = s.into();
        if s.is_empty() {
            return Err(SyncError::Other("package name must not be empty".into()));
        }
        if !s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
            return Err(SyncError::Other(format!(
                "package name {s:?} contains invalid characters"
            )));
        }
        Ok(PackageName(s))
    }

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
    pub package: PackageName,
    pub channel: String,
}

/// An announcement that a set of changes has been merged onto a channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipAnnouncement {
    pub channel: ChannelRef,
    pub tip: ChangeId,
    pub changes: Vec<(ChangeId, IrohHash)>,
}

/// Newtype wrapper around the raw 32-byte BLAKE3 hash as produced by iroh-blobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrohHash(pub [u8; 32]);

impl From<transport::blob::TransportHash> for IrohHash {
    fn from(h: transport::blob::TransportHash) -> Self {
        IrohHash(h.0)
    }
}

impl From<IrohHash> for transport::blob::TransportHash {
    fn from(h: IrohHash) -> Self {
        transport::blob::TransportHash(h.0)
    }
}

/// Acknowledgement sent by the receiver after successfully applying all changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAck {
    pub tip: ChangeId,
    pub applied: u64,
}

/// The event that triggers a push: a commit was merged onto a durable channel.
#[derive(Clone, Debug)]
pub struct MergeEvent {
    pub channel: ChannelRef,
    pub tip: ChangeId,
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
        assert!(matches!(err, VerifyError::InvalidChar('A')));
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
