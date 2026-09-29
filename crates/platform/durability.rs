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
//! that also carries generic write access can be flushed, so the recipe has to be chosen once and shared
//! rather than rediscovered at each of the two dozen barriers in this workspace.
//!
//! The call sites keep their own `sync_all` and their own error mapping; several distinguish a
//! failure to open the directory from a failure to flush it, and that distinction survives here.

use std::fs::File;
use std::io;
use std::path::Path;

/// Opens a regular file without following a symbolic link or reparse point.
///
/// The type check is performed on the opened handle, so replacing a checked
/// pathname with a link between metadata inspection and open cannot redirect
/// the caller to mutable bytes outside its namespace.
#[cfg(unix)]
pub fn open_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    ensure_regular_file(file)
}

/// Opens a regular file without following a symbolic link or reparse point.
#[cfg(windows)]
pub fn open_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_regular_file(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_regular_file_nofollow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow regular-file opener is available on this platform",
    ))
}

/// Opens an existing regular file for read/write locking without following links.
#[cfg(unix)]
pub fn open_regular_file_readwrite_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    ensure_regular_file(file)
}

/// Opens an existing regular file for read/write locking without following links.
#[cfg(windows)]
pub fn open_regular_file_readwrite_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_regular_file(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_regular_file_readwrite_nofollow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow regular-file opener is available on this platform",
    ))
}

/// Opens or creates a regular file for reading and writing without following links.
#[cfg(unix)]
pub fn open_or_create_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    ensure_regular_file(file)
}

/// Opens or creates a regular file for reading and writing without following links.
#[cfg(windows)]
pub fn open_or_create_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_regular_file(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_or_create_regular_file_nofollow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow regular-file opener is available on this platform",
    ))
}

/// Opens or creates a regular file for appending without following links.
#[cfg(unix)]
pub fn open_or_create_append_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .append(true)
        .create(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    ensure_regular_file(file)
}

/// Opens or creates a regular file for appending without following links.
#[cfg(windows)]
pub fn open_or_create_append_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .append(true)
        .create(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_regular_file(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_or_create_append_file_nofollow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow regular-file opener is available on this platform",
    ))
}

/// Opens or creates a regular file with truncation without following links.
#[cfg(unix)]
pub fn open_or_truncate_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    ensure_regular_file(file)
}

/// Opens or creates a regular file with truncation without following links.
#[cfg(windows)]
pub fn open_or_truncate_regular_file_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_regular_file(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_or_truncate_regular_file_nofollow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow regular-file opener is available on this platform",
    ))
}

fn ensure_regular_file(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    let file_type = metadata.file_type();
    if file_type.is_file() && !file_type.is_symlink() && !is_reparse_point(&metadata) {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened path is not a regular file",
        ))
    }
}

/// Opens a directory without following a symbolic link or reparse point.
#[cfg(unix)]
pub fn open_directory_nofollow(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(directory)?;
    ensure_directory(file)
}

/// Opens a directory without following a symbolic link or reparse point.
#[cfg(windows)]
pub fn open_directory_nofollow(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let file = OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory)?;
    ensure_directory(file)
}

/// Opens a directory for path validation without following a symbolic link or reparse point.
/// Unlike [`open_directory_nofollow`], this handle is read-only and is not suitable for flushing.
#[cfg(unix)]
pub fn open_directory_readonly_nofollow(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(directory)?;
    ensure_directory(file)
}

/// Opens a directory for path validation without following a symbolic link or reparse point.
/// Unlike [`open_directory_nofollow`], this handle is read-only and is not suitable for flushing.
#[cfg(windows)]
pub fn open_directory_readonly_nofollow(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory)?;
    ensure_directory(file)
}

#[cfg(not(any(unix, windows)))]
pub fn open_directory_readonly_nofollow(_directory: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow directory opener is available on this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn open_directory_nofollow(_directory: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no no-follow directory opener is available on this platform",
    ))
}

fn ensure_directory(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    let file_type = metadata.file_type();
    if file_type.is_dir() && !file_type.is_symlink() && !is_reparse_point(&metadata) {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened path is not a directory",
        ))
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Opens `directory` so that the handle can be flushed with [`File::sync_all`].
///
/// This is the portable replacement for `File::open(directory)` in a durability barrier. The
/// returned handle is for flushing only; it is not positioned for reading entries.
///
/// On Unix this is exactly `File::open`. On Windows the directory is opened with backup
/// semantics and `GENERIC_WRITE`, which is required by `FlushFileBuffers`. Every barrier in this
/// workspace has just written into the directory it flushes, so that access is already the
/// caller's to ask for.
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
/// The directory is opened with backup semantics and `GENERIC_WRITE`, which is required by
/// `FlushFileBuffers`. Every barrier in this workspace has just written into the directory it
/// flushes, so that access is already the caller's to ask for.
///
/// # Errors
/// Returns an error when the directory cannot be opened, including when the caller may not write
/// to it. The caller flushes the handle and maps that failure itself.
#[cfg(windows)]
pub fn open_directory(directory: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    let file = OpenOptions::new()
        .access_mode(GENERIC_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::{
        open_directory, open_directory_nofollow, open_directory_readonly_nofollow,
        open_regular_file_nofollow,
    };
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

    #[test]
    fn no_follow_opener_accepts_a_regular_file() {
        let directory = scratch("regular-file");
        fs::create_dir_all(&directory).expect("create scratch directory");
        let path = directory.join("object");
        fs::write(&path, b"regular bytes").expect("write object");

        let file = open_regular_file_nofollow(&path).expect("open regular file without links");
        assert!(
            file.metadata()
                .expect("opened metadata")
                .file_type()
                .is_file()
        );

        fs::remove_dir_all(&directory).expect("remove scratch directory");
    }

    #[test]
    fn no_follow_directory_opener_accepts_a_real_directory() {
        let directory = scratch("nofollow-directory");
        fs::create_dir_all(&directory).expect("create scratch directory");

        let opened = open_directory_nofollow(&directory).expect("open directory without links");
        assert!(opened.metadata().expect("opened metadata").is_dir());

        drop(opened);
        fs::remove_dir_all(&directory).expect("remove scratch directory");
    }

    #[test]
    fn readonly_no_follow_directory_opener_accepts_a_real_directory() {
        let directory = scratch("readonly-nofollow-directory");
        fs::create_dir_all(&directory).expect("create scratch directory");

        let opened = open_directory_readonly_nofollow(&directory)
            .expect("open directory read-only without links");
        assert!(opened.metadata().expect("opened metadata").is_dir());

        drop(opened);
        fs::remove_dir_all(&directory).expect("remove scratch directory");
    }

    #[cfg(unix)]
    #[test]
    fn readonly_no_follow_directory_opener_rejects_a_symlink() {
        use std::os::unix::fs::symlink;

        let root = scratch("readonly-nofollow-symlink");
        let directory = root.join("directory");
        let link = root.join("link");
        fs::create_dir_all(&directory).expect("create scratch directory");
        symlink(&directory, &link).expect("create directory link");

        assert!(open_directory_readonly_nofollow(&link).is_err());
        fs::remove_dir_all(&root).expect("remove scratch directory");
    }
}
