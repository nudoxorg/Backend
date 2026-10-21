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

/// Replaces a file name without clearing the replaced inode's read-only attribute.
///
/// `source` must be the already-written handle at `source_path`. This does not flush either
/// file contents or directory entries: the caller must flush `source` before this call and
/// the destination directory afterwards. The source handle is compared with the no-follow
/// rename handle, so a different file substituted at `source_path` cannot be published.
///
/// This uses the local Win32 extended rename capability, including POSIX replacement of a
/// held target and ignoring its read-only attribute. Unsupported filesystems/OS versions,
/// insufficient access, and sharing refusals are errors; there is no attribute-changing or
/// unlink-then-rename fallback. In particular, this does not promise support on SMB shares.
#[cfg(windows)]
pub fn replace_file_preserving_readonly(
    source_path: &Path,
    source: &File,
    destination: &Path,
) -> io::Result<()> {
    let rename = open_regular_file_delete_nofollow(source_path)?;
    if crate::FileIdentity::of_file(source)? != crate::FileIdentity::of_file(&rename)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rename source changed",
        ));
    }
    crate::win32::file::replace_readonly(&rename, destination)
}

/// Unlinks the exact file at a private staging name without unsealing its other hard links.
/// Unsupported extended deletion or denied access fails without changing attributes.
#[cfg(windows)]
pub fn unlink_file_preserving_readonly(path: &Path, expected: &File) -> io::Result<()> {
    let unlink = open_regular_file_delete_nofollow(path)?;
    if crate::FileIdentity::of_file(expected)? != crate::FileIdentity::of_file(&unlink)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unlink source changed",
        ));
    }
    crate::win32::file::unlink_readonly(&unlink)
}

#[cfg(windows)]
fn open_regular_file_delete_nofollow(path: &Path) -> io::Result<File> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };

    ensure_regular_file(
        OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?,
    )
}

/// Opens a direct child directory relative to an actual Windows directory handle, rejecting
/// reparse points. The result supports both bounded enumeration and a durability flush.
#[cfg(windows)]
pub fn open_directory_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::Directory,
    )
}

/// Opens a direct directory child for validation and reading, without requesting write access.
#[cfg(windows)]
pub fn open_directory_readonly_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::ReadDirectory,
    )
}

/// Exclusively creates a direct directory child without resolving a parent path again.
#[cfg(windows)]
pub fn create_directory_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::CreateDirectory,
    )
}

/// Enumerates names from an actual directory handle without reopening its path.
#[cfg(windows)]
pub fn directory_child_names(directory: &File) -> io::Result<Vec<String>> {
    crate::win32::workspace_fs::durability_directory_names(directory)
}

/// Opens a direct regular-file child without resolving any parent pathname again.
#[cfg(windows)]
pub fn open_regular_file_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::ReadFile,
    )
}

/// Opens an existing writable regular-file child under a held directory.
#[cfg(windows)]
pub fn open_regular_file_readwrite_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::ReadWriteFile,
    )
}

/// Exclusively creates a writable regular-file child beneath the actual directory handle.
#[cfg(windows)]
pub fn create_regular_file_child_nofollow(parent: &File, name: &str) -> io::Result<File> {
    crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::CreateFile,
    )
}

/// Publishes an already-flushed source under a held destination directory. Both names are
/// direct children of their held parents; a parent-path substitution cannot redirect it.
/// Unsupported extended rename fails without changing any existing inode's attributes.
#[cfg(windows)]
pub fn replace_file_preserving_readonly_at(
    source_parent: &File,
    source_name: &str,
    source: &File,
    destination_parent: &File,
    destination_name: &str,
) -> io::Result<()> {
    let rename = crate::win32::workspace_fs::open_durability_child(
        source_parent,
        source_name,
        crate::win32::workspace_fs::DurabilityChild::DeleteFile,
    )?;
    if crate::FileIdentity::of_file(source)? != crate::FileIdentity::of_file(&rename)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "rename source changed",
        ));
    }
    crate::win32::file::replace_readonly_at(&rename, destination_parent, destination_name)
}

/// Unlinks an exact direct regular-file child without unsealing any other hard link.
#[cfg(windows)]
pub fn unlink_file_preserving_readonly_at(
    parent: &File,
    name: &str,
    expected: &File,
) -> io::Result<()> {
    let unlink = crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::DeleteFile,
    )?;
    if crate::FileIdentity::of_file(expected)? != crate::FileIdentity::of_file(&unlink)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unlink source changed",
        ));
    }
    crate::win32::file::unlink_readonly(&unlink)
}

/// Deletes only authorized regular direct children, without following reparse points or
/// reopening their parent by path. Uses bounded 64 KiB enumeration batches and checks each
/// opened child against its enumerated file identity. The caller retains cleanup authority.
#[cfg(windows)]
pub fn unlink_directory_files_preserving_readonly(
    parent: &File,
    allowed: impl Fn(&str) -> bool,
) -> io::Result<()> {
    crate::win32::workspace_fs::unlink_durability_children(parent, allowed)
}

/// Removes an exact empty child directory through a handle, with no recursive path deletion.
#[cfg(windows)]
pub fn remove_empty_directory_child(parent: &File, name: &str, expected: &File) -> io::Result<()> {
    let unlink = crate::win32::workspace_fs::open_durability_child(
        parent,
        name,
        crate::win32::workspace_fs::DurabilityChild::DeleteDirectory,
    )?;
    if crate::FileIdentity::of_file(expected)? != crate::FileIdentity::of_file(&unlink)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory cleanup name changed",
        ));
    }
    crate::win32::file::unlink_readonly(&unlink)
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

/// Completes independent durability barriers with at most sixteen flushes in flight.
///
/// The caller must separate dependent barriers into successive calls: file contents must be
/// flushed before the directory entries or records that make them reachable.
pub fn flush_concurrently<T: Sync>(
    items: &[T],
    flush: impl Fn(&T) -> io::Result<()> + Sync,
) -> io::Result<()> {
    flush_concurrently_bounded(items, 16, flush)
}

/// Completes every independent barrier, returning the first error in input order.
///
/// `max_in_flight` is clamped to 1..=64. A failed thread allocation runs that barrier on the
/// caller instead. All workers are joined even after failure; a worker panic fails the batch.
/// No dependent publication may proceed unless this returns success.
pub fn flush_concurrently_bounded<T: Sync>(
    items: &[T],
    max_in_flight: usize,
    flush: impl Fn(&T) -> io::Result<()> + Sync,
) -> io::Result<()> {
    try_flush_concurrently_bounded(items, max_in_flight, flush).map_err(|error| match error {
        BarrierError::Operation(error) => error,
        BarrierError::WorkerPanicked => io::Error::other("durability barrier worker panicked"),
    })
}

/// A typed failure from a bounded group of independent durability operations.
#[derive(Debug)]
pub enum BarrierError<E> {
    /// An operation returned its domain-specific error.
    Operation(E),
    /// A worker panicked before returning an operation result.
    WorkerPanicked,
}

/// Completes every independent barrier while preserving the caller's error type.
///
/// `max_in_flight` is clamped to 1..=64. A failed thread allocation runs that barrier on the
/// caller instead. All workers are joined even after failure; the first operation or panic in
/// input order is returned. No dependent publication may proceed unless this returns success.
pub fn try_flush_concurrently_bounded<T: Sync, E: Send>(
    items: &[T],
    max_in_flight: usize,
    flush: impl Fn(&T) -> Result<(), E> + Sync,
) -> Result<(), BarrierError<E>> {
    match items {
        [] => return Ok(()),
        [only] => return flush(only).map_err(BarrierError::Operation),
        _ => {}
    }
    let mut first_error = None;
    for batch in items.chunks(max_in_flight.clamp(1, 64)) {
        std::thread::scope(|scope| {
            let flush = &flush;
            let started = batch
                .iter()
                .map(|item| {
                    std::thread::Builder::new()
                        .name("nudox-flush".to_owned())
                        .spawn_scoped(scope, move || flush(item))
                })
                .collect::<Vec<_>>();
            for (thread, item) in started.into_iter().zip(batch) {
                let result = match thread {
                    Ok(thread) => match thread.join() {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(BarrierError::Operation(error)),
                        Err(_) => Err(BarrierError::WorkerPanicked),
                    },
                    Err(_) => flush(item).map_err(BarrierError::Operation),
                };
                if first_error.is_none() {
                    first_error = result.err();
                }
            }
        });
    }
    first_error.map_or(Ok(()), Err)
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

    #[cfg(windows)]
    #[test]
    fn relative_mutations_stay_under_the_opened_parent_after_its_path_is_rebound()
    -> std::io::Result<()> {
        let root = scratch("readonly-relative-parent");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        fs::create_dir_all(root.join("stage"))?;
        fs::create_dir(root.join("destination"))?;
        let root_handle = super::open_directory_nofollow(&root)?;
        let stage = super::open_directory_child_nofollow(&root_handle, "stage")?;
        let destination = super::open_directory_child_nofollow(&root_handle, "destination")?;
        let mut source = super::create_regular_file_child_nofollow(&stage, "member.tmp")?;
        source.write_all(b"verified durable bytes")?;
        source.sync_all()?;

        fs::rename(root.join("destination"), root.join("retired"))?;
        fs::create_dir(root.join("destination"))?;
        fs::write(root.join("destination/member"), b"foreign bytes")?;
        super::replace_file_preserving_readonly_at(
            &stage,
            "member.tmp",
            &source,
            &destination,
            "member",
        )?;
        destination.sync_all()?;
        stage.sync_all()?;
        assert_eq!(
            fs::read(root.join("retired/member"))?,
            b"verified durable bytes"
        );
        assert_eq!(fs::read(root.join("destination/member"))?, b"foreign bytes");
        super::unlink_directory_files_preserving_readonly(&destination, |name| name == "member")?;
        destination.sync_all()?;
        assert!(!root.join("retired/member").exists());
        assert_eq!(fs::read(root.join("destination/member"))?, b"foreign bytes");
        super::remove_empty_directory_child(&root_handle, "retired", &destination)?;
        root_handle.sync_all()?;
        drop((source, stage, destination, root_handle));
        fs::remove_dir_all(root)
    }

    #[test]
    fn bounded_barriers_cover_every_item_with_zero_or_small_limits() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        for limit in [0, 4] {
            let active = AtomicUsize::new(0);
            let maximum = AtomicUsize::new(0);
            let completed = AtomicUsize::new(0);
            super::flush_concurrently_bounded(&[0; 40], limit, |_| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                std::thread::yield_now();
                active.fetch_sub(1, Ordering::SeqCst);
                completed.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .expect("bounded barriers");
            assert!(maximum.load(Ordering::SeqCst) <= limit.max(1));
            assert_eq!(completed.load(Ordering::SeqCst), 40);
            assert_eq!(active.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn failed_barriers_still_join_every_item_and_return_input_order_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let completed = AtomicUsize::new(0);
        let items = (0..40).collect::<Vec<_>>();
        let result = super::flush_concurrently_bounded(&items, 4, |item| {
            completed.fetch_add(1, Ordering::SeqCst);
            if matches!(*item, 3 | 7 | 39) {
                Err(std::io::Error::other(format!("failed {item}")))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.expect_err("barrier failed").to_string(), "failed 3");
        assert_eq!(completed.load(Ordering::SeqCst), items.len());
    }

    #[test]
    fn typed_barriers_preserve_domain_errors_and_join_every_item() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug, Eq, PartialEq)]
        enum DomainError {
            UnsafePath,
            Corrupt,
        }

        let completed = AtomicUsize::new(0);
        let items = (0..40).collect::<Vec<_>>();
        let result = super::try_flush_concurrently_bounded(&items, 4, |item| {
            completed.fetch_add(1, Ordering::SeqCst);
            match *item {
                3 => Err(DomainError::UnsafePath),
                7 => Err(DomainError::Corrupt),
                _ => Ok(()),
            }
        });
        assert!(matches!(
            result,
            Err(super::BarrierError::Operation(DomainError::UnsafePath))
        ));
        assert_eq!(completed.load(Ordering::SeqCst), items.len());
    }

    #[test]
    fn panic_fails_the_batch_after_other_workers_finish() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let completed = AtomicUsize::new(0);
        let result = super::flush_concurrently_bounded(&[0, 1, 2, 3], 2, |item| {
            assert_ne!(*item, 0, "injected worker panic");
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(completed.load(Ordering::SeqCst), 3);
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
