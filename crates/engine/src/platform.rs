//! Small platform adapters shared by process boundaries.
//!
//! The engine reexports the replication-owned peer credential inspection for
//! listeners that still compose through this crate. Keeping the implementation
//! in replication lets CLI and MCP use the same trust seam without depending on
//! the higher-level engine.

use std::path::Path;

#[cfg(unix)]
use std::io::Read;

#[cfg(unix)]
pub use backend_replication::{
    PeerCredentialError, PeerCredentials, current_effective_uid, peer_credentials,
    peer_is_same_effective_uid,
};

/// Failure while loading a process authority credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthoritySecretError {
    /// The credential path is not a regular file.
    NotRegularFile,
    /// The credential is not exactly the required bounded size.
    InvalidLength,
    /// The credential file is not private to its owner.
    InsecurePermissions,
    /// The credential file is owned by another effective user.
    WrongOwner,
    /// The target has no supported ownership or permission API.
    Unsupported,
    /// Opening or reading the credential failed.
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for AuthoritySecretError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRegularFile => {
                formatter.write_str("authority credential is not a regular file")
            }
            Self::InvalidLength => {
                formatter.write_str("authority credential must be exactly 32 bytes")
            }
            Self::InsecurePermissions => {
                formatter.write_str("authority credential permissions must be exactly 0600")
            }
            Self::WrongOwner => {
                formatter.write_str("authority credential is not owned by the effective user")
            }
            Self::Unsupported => {
                formatter.write_str("authority credential ownership checks are unsupported")
            }
            Self::Io(kind) => write!(formatter, "authority credential I/O failed: {kind:?}"),
        }
    }
}

impl std::error::Error for AuthoritySecretError {}

/// Loads one bounded authority secret from a private, owner-owned file.
///
/// The caller receives exactly 32 bytes. Metadata is checked before opening
/// and the read is exact, so a product process never silently truncates or
/// expands a credential supplied by its host.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn read_authority_secret(path: &Path) -> Result<[u8; 32], AuthoritySecretError> {
    let metadata =
        std::fs::metadata(path).map_err(|error| AuthoritySecretError::Io(error.kind()))?;
    if !metadata.file_type().is_file() {
        return Err(AuthoritySecretError::NotRegularFile);
    }
    if metadata.len() != 32 {
        return Err(AuthoritySecretError::InvalidLength);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o777 != 0o600 {
            return Err(AuthoritySecretError::InsecurePermissions);
        }
        let owner = metadata.uid();
        if current_effective_uid().map_err(|_| AuthoritySecretError::WrongOwner)? != owner {
            return Err(AuthoritySecretError::WrongOwner);
        }
    }
    #[cfg(not(unix))]
    {
        return Err(AuthoritySecretError::Unsupported);
    }

    let mut secret = [0_u8; 32];
    let mut file =
        std::fs::File::open(path).map_err(|error| AuthoritySecretError::Io(error.kind()))?;
    file.read_exact(&mut secret)
        .map_err(|error| AuthoritySecretError::Io(error.kind()))?;
    Ok(secret)
}
