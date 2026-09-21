//! Small platform adapters shared by process boundaries.
//!
//! The engine reexports the replication-owned peer credential inspection for
//! listeners that still compose through this crate. Keeping the implementation
//! in replication lets CLI and MCP use the same trust seam without depending on
//! the higher-level engine.

use std::path::Path;

use std::io::Read;

#[cfg(unix)]
pub use backend_replication::{
    PeerCredentialError, PeerCredentials, current_effective_uid, peer_credentials,
    peer_is_same_effective_uid,
};

/// Failure while loading a process authority credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthoritySecretError {
    /// The credential path itself was a symbolic link.
    SymbolicLink,
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
            Self::SymbolicLink => {
                formatter.write_str("authority credential must not be a symbolic link")
            }
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
    #[cfg(unix)]
    let mut file = {
        use rustix::fs::{Mode, OFlags, open};
        if std::fs::symlink_metadata(path)
            .map_err(|error| AuthoritySecretError::Io(error.kind()))?
            .file_type()
            .is_symlink()
        {
            return Err(AuthoritySecretError::SymbolicLink);
        }
        open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|error| AuthoritySecretError::Io(error.kind()))?
    };
    #[cfg(windows)]
    let mut file = {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| AuthoritySecretError::Io(error.kind()))?;
        if metadata.file_type().is_symlink() {
            return Err(AuthoritySecretError::SymbolicLink);
        }
        std::fs::File::open(path).map_err(|error| AuthoritySecretError::Io(error.kind()))?
    };
    #[cfg(not(any(unix, windows)))]
    return Err(AuthoritySecretError::Unsupported);

    let metadata = file
        .metadata()
        .map_err(|error| AuthoritySecretError::Io(error.kind()))?;
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
    #[cfg(windows)]
    {
        let owner = backend_platform::win32::identity::file_owner(path)
            .map_err(|error| AuthoritySecretError::Io(error.kind()))?;
        if !backend_platform::win32::identity::is_owned_by_current_user(&owner)
            .map_err(|error| AuthoritySecretError::Io(error.kind()))?
        {
            return Err(AuthoritySecretError::WrongOwner);
        }
    }
    let mut secret = [0_u8; 32];
    file.read_exact(&mut secret)
        .map_err(|error| AuthoritySecretError::Io(error.kind()))?;
    Ok(secret)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn authority_secret_rejects_a_private_symlink() -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "backend-authority-secret-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&directory)?;
        let target = directory.join("target");
        std::fs::write(&target, [7_u8; 32])?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
        let link = directory.join("credential");
        symlink(&target, &link)?;
        assert_eq!(
            read_authority_secret(&link),
            Err(AuthoritySecretError::SymbolicLink)
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
