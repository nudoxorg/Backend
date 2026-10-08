//! Stable identity of one filesystem object, independent of the names that reach it.
//!
//! Two handles, or a handle and a path, name the same object exactly when their
//! identities are equal. Unix identifies an object by device and inode. Windows
//! uses the volume serial number and the 128-bit file id that NTFS and ReFS
//! report through `FileIdInfo`; the older 64-bit file index is not unique on
//! ReFS, and the standard library keeps both behind an unstable accessor, so
//! this seam reads the identity from the handle itself.

use std::fs::File;
use std::io;
use std::path::Path;

/// The volume and object a filesystem handle refers to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIdentity {
    volume: u64,
    object: u128,
}

impl FileIdentity {
    /// Returns the volume and full object identifier without truncating Windows file ids.
    #[must_use]
    pub const fn parts(self) -> (u64, u128) {
        (self.volume, self.object)
    }

    /// Returns the stable platform-independent volume/object identity bytes.
    /// These bytes identify one filesystem instance, rather than its path.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&self.volume.to_be_bytes());
        bytes[8..].copy_from_slice(&self.object.to_be_bytes());
        bytes
    }

    /// Reads the identity of the object an open file handle refers to.
    ///
    /// # Errors
    /// Returns the operating-system error when the handle cannot be queried,
    /// or `Unsupported` when the filesystem reports no stable object id.
    pub fn of_file(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::from_unix(&file.metadata()?))
        }
        #[cfg(windows)]
        {
            let (volume, object) = crate::win32::file::identity_of(file)?;
            Ok(Self {
                volume,
                object: u128::from_le_bytes(object),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Err(unsupported())
        }
    }

    /// Reads the identity of the object a path names, without following a
    /// final symbolic link or reparse point: a link is identified as itself.
    ///
    /// # Errors
    /// Returns the operating-system error when the path cannot be opened or
    /// queried, or `Unsupported` when the filesystem reports no stable id.
    pub fn of_path_nofollow(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::from_unix(&std::fs::symlink_metadata(path)?))
        }
        #[cfg(windows)]
        {
            use std::fs::OpenOptions;
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            };

            // Attribute-only access: querying an identity must not conflict
            // with a writer that holds the object open.
            let handle = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            Self::of_file(&handle)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    #[cfg(unix)]
    fn from_unix(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;

        Self {
            volume: metadata.dev(),
            object: u128::from(metadata.ino()),
        }
    }
}

/// Returns how many directory entries name the object held by an open file.
///
/// # Errors
/// Returns an OS error if the handle cannot be queried, or `Unsupported`
/// on platforms without a link-count implementation.
pub fn number_of_links(file: &File) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        crate::win32::file::number_of_links(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(unsupported())
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "file identity is unavailable on this platform",
    )
}

#[cfg(test)]
mod tests {
    use super::FileIdentity;
    use std::fs;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "bp-identity-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create scratch directory");
        path
    }

    #[test]
    fn a_handle_and_its_path_share_one_identity() {
        let directory = scratch("same");
        let path = directory.join("object");
        fs::write(&path, b"bytes").expect("write object");
        let file = fs::File::open(&path).expect("open object");
        assert_eq!(
            FileIdentity::of_file(&file).expect("handle identity"),
            FileIdentity::of_path_nofollow(&path).expect("path identity"),
        );
        fs::remove_dir_all(directory).expect("remove scratch directory");
    }

    #[test]
    fn distinct_files_have_distinct_identities() {
        let directory = scratch("distinct");
        let (left, right) = (directory.join("left"), directory.join("right"));
        fs::write(&left, b"left").expect("write left");
        fs::write(&right, b"right").expect("write right");
        assert_ne!(
            FileIdentity::of_path_nofollow(&left).expect("left identity"),
            FileIdentity::of_path_nofollow(&right).expect("right identity"),
        );
        fs::remove_dir_all(directory).expect("remove scratch directory");
    }

    #[test]
    fn a_hard_link_is_the_same_object() {
        let directory = scratch("link");
        let original = directory.join("original");
        let alias = directory.join("alias");
        fs::write(&original, b"bytes").expect("write original");
        let file = fs::File::open(&original).expect("open original");
        assert_eq!(super::number_of_links(&file).expect("one name"), 1);
        fs::hard_link(&original, &alias).expect("create hard link");
        assert_eq!(super::number_of_links(&file).expect("two names"), 2);
        assert_eq!(
            FileIdentity::of_path_nofollow(&original).expect("original identity"),
            FileIdentity::of_path_nofollow(&alias).expect("alias identity"),
        );
        fs::remove_dir_all(directory).expect("remove scratch directory");
    }

    #[test]
    fn a_missing_path_is_an_error_not_an_identity() {
        let directory = scratch("missing");
        let error = FileIdentity::of_path_nofollow(&directory.join("absent"))
            .expect_err("a missing path has no identity");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        fs::remove_dir_all(directory).expect("remove scratch directory");
    }
}
