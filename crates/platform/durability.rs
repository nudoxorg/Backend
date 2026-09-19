//! The directory half of the write-temp, fsync, rename, fsync-parent barrier.
//! Unix opens a directory for reading and flushes that handle; Windows refuses both halves of
//! that idiom unless the handle is opened with backup semantics *and* write access.
//!
//! # Why this is its own seam
//!
//! `File::open(directory)` is a Unix idiom. On Windows it calls `CreateFileW` without
//! `FILE_FLAG_BACKUP_SEMANTICS`, and opening a directory that way always fails with
//! `ERROR_ACCESS_DENIED`. Adding backup semantics fixes the open but not the flush:
//! `FlushFileBuffers` on a read-only directory handle fails with the same error. Only a handle
//! that also carries write access can be flushed, so the recipe has to be chosen once and shared
//! rather than rediscovered at each of the two dozen barriers in this workspace.
//!
//! The call sites keep their own `sync_all` and their own error mapping; several distinguish a
//! failure to open the directory from a failure to flush it, and that distinction survives here.

use std::fs::File;
use std::io;
use std::path::Path;

/// Opens `directory` so that the handle can be flushed with [`File::sync_all`].
///
/// This is the portable replacement for `File::open(directory)` in a durability barrier. The
/// returned handle is for flushing only; it is not positioned for reading entries.
///
/// On Unix this is exactly `File::open`. On Windows the directory is opened with backup
/// semantics and the least access `FlushFileBuffers` accepts: permission to list the directory,
/// to add a file to it, and to wait on the handle. Every barrier in this workspace has just
/// written into the directory it flushes, so that access is already the caller's to ask for.
///
/// # Errors
/// Returns an error when the directory cannot be opened, including when the caller may not write
/// to it. The caller flushes the handle and maps that failure itself.
#[cfg(not(windows))]
pub fn open_directory(directory: &Path) -> io::Result<File> {
    File::open(directory)
}

/// Opens `directory` so that the handle can be flushed with [`File::sync_all`].
///
/// This is the portable replacement for `File::open(directory)` in a durability barrier. The
/// returned handle is for flushing only; it is not positioned for reading entries.
///
/// On Unix this is exactly `File::open`. On Windows the directory is opened with backup
/// semantics and the least access `FlushFileBuffers` accepts: permission to list the directory,
/// to add a file to it, and to wait on the handle. Every barrier in this workspace has just
/// written into the directory it flushes, so that access is already the caller's to ask for.
///
/// # Errors
/// Returns an error when the directory cannot be opened, including when the caller may not write
/// to it. The caller flushes the handle and maps that failure itself.
#[cfg(windows)]
pub fn open_directory(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, SYNCHRONIZE,
    };

    OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_ADD_FILE | SYNCHRONIZE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)
}

#[cfg(test)]
mod tests {
    use super::open_directory;
    use std::fs::{self, File};
    use std::io::Write as _;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        std::env::temp_dir().join(format!("bp-dirsync-{label}-{}-{nonce}", std::process::id()))
    }

    /// The whole barrier, in the order every call site performs it. The flush is the half that
    /// Windows refuses for a directory opened read-only, so asserting the open alone would keep
    /// passing against a handle that cannot be flushed.
    #[test]
    fn a_renamed_file_is_followed_by_a_directory_flush_that_succeeds() {
        let directory = scratch("barrier");
        fs::create_dir_all(&directory).expect("create scratch directory");
        let temporary = directory.join("state.tmp");
        let target = directory.join("state");

        let mut file = File::create(&temporary).expect("create temporary");
        file.write_all(b"durable").expect("write temporary");
        file.sync_all().expect("flush temporary");
        drop(file);
        fs::rename(&temporary, &target).expect("publish by rename");

        open_directory(&directory)
            .expect("open the directory for flushing")
            .sync_all()
            .expect("flush the directory the rename was recorded in");

        fs::remove_dir_all(&directory).expect("remove scratch directory");
    }
}
