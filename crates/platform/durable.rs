//! Crash-safe publication of small durable state files.
//!
//! Writers create a unique sibling, flush its contents, atomically replace the
//! canonical path, and finally flush the containing directory. Keeping this
//! protocol here gives every process surface one reviewed operating-system
//! boundary instead of subtly different rename sequences.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Publishes `bytes` at `path` without exposing a partial state file.
///
/// # Errors
///
/// Returns an I/O error when the parent cannot be created, the sibling cannot
/// be written and flushed, the atomic replacement fails, or the publication
/// fence cannot be flushed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent(path);
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = create_temporary(parent, path.file_name())?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_parent(path)
}

/// Publishes a private state file with an owner-only ACL or mode.
///
/// The parent directory must already exist and be private to the current user.
/// The temporary is protected before any state bytes are written. The file is
/// flushed before replacement and the parent is flushed after replacement.
///
/// # Errors
/// Returns an I/O error if the parent or destination cannot be checked, the
/// temporary cannot be made private, or the durable replacement cannot be
/// completed.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_private_atomic_platform(path, bytes)
}

/// Opens a private state file for reading after checking the opened file.
///
/// On Windows this rejects reparse points throughout the path chain and on
/// the opened file, verifies its owner, single-link identity, and protected
/// current-user-only DACL on that handle. Unix opens and holds the private
/// parent directory, opens the final component relative to that handle without
/// following links, then checks the file's owner, type, permissions, and link
/// count on the opened handle.
///
/// # Errors
/// Returns an I/O error if the file is missing, is not a regular private file,
/// or cannot be checked and opened relative to a held parent without following
/// the final link.
pub fn open_private_read(path: &Path) -> io::Result<File> {
    open_private_read_platform(path)
}

/// Removes a checked private state file and durably flushes its parent.
///
/// # Errors
/// Returns an I/O error if the file cannot be checked or removed, or if the
/// parent directory durability barrier cannot be completed.
pub fn remove_private(path: &Path) -> io::Result<()> {
    remove_private_platform(path)
}

/// Creates or verifies a private directory for durable state files.
///
/// Only the final path component may be created; its parent must already exist
/// and be private to the current user. Existing directories are opened without
/// following a final link and checked through the opened object. The directory
/// and its parent are flushed before this returns.
///
/// # Errors
/// Returns an I/O error when the parent is missing or not private, the target
/// is not a private directory, a link or reparse point is encountered, or the
/// directory durability barriers cannot be completed.
pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    ensure_private_directory_platform(path)
}

/// Creates or verifies one owner-only application-state directory beneath an
/// existing operating-system data directory.
///
/// Unlike [`ensure_private_directory`], the immediate parent may be the
/// conventional per-user application-data directory, which is often readable
/// by other users. The parent must be a real directory owned by the current
/// user and must not be writable by group or other users. The requested child
/// is created with owner-only access, or an existing child is rejected unless
/// it already has owner-only access. No ancestor is changed.
///
/// This is intended for the first private application directory (for example,
/// `Application Support/Nudox`). Call [`ensure_private_directory`] for each
/// nested directory after this boundary has been established.
pub fn ensure_private_child_directory(path: &Path) -> io::Result<()> {
    ensure_private_child_directory_platform(path)
}

fn create_temporary(parent: &Path, file_name: Option<&OsStr>) -> io::Result<(PathBuf, File)> {
    loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name.unwrap_or_else(|| OsStr::new("state")));
        name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let path = parent.join(name);
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Atomically replaces `destination` with `source` on the same filesystem.
///
/// # Errors
///
/// Returns an I/O error when the operating system cannot replace the path.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    replace_file_platform(source, destination)
}

#[cfg(not(windows))]
fn replace_file_platform(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file_platform(source: &Path, destination: &Path) -> io::Result<()> {
    crate::win32::file::replace(source, destination)
}

/// Flushes the directory entry containing `path` after publication.
///
/// # Errors
///
/// Returns an I/O error when the containing directory cannot be opened or
/// flushed on the current platform.
#[cfg(unix)]
pub fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(parent(path))?.sync_all()
}

/// Windows replacement requests write-through and also flushes a directory
/// handle with the access required by `FlushFileBuffers`.
#[cfg(windows)]
pub fn sync_parent(path: &Path) -> io::Result<()> {
    let directory = parent(path);
    crate::durability::open_directory(directory)?.sync_all()
}

#[cfg(unix)]
fn write_private_atomic_platform(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let parent = parent(path);
    validate_unix_parent(parent)?;
    reject_unix_symlink_destination(path)?;
    let (temporary, mut file) = create_private_temporary(parent, path.file_name())?;
    // Set the exact mode through the already-open descriptor. This also
    // restores owner read/write if an unusually restrictive umask removed it.
    if let Err(error) = file
        .set_permissions(fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_parent(path)
}

#[cfg(windows)]
fn write_private_atomic_platform(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::win32::durable::write_private_atomic(path, bytes)
}

#[cfg(not(any(unix, windows)))]
fn write_private_atomic_platform(_path: &Path, _bytes: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private durable state files are not supported on this platform",
    ))
}

#[cfg(unix)]
fn open_private_read_platform(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let parent_path = parent(path).canonicalize()?;
    let mut parent_handle = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(rustix_io)?;
    for component in parent_path.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        parent_handle = openat(
            &parent_handle,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(rustix_io)?;
    }
    validate_unix_private_directory(&parent_handle)?;
    let name = path
        .file_name()
        .filter(|name| *name != "." && *name != "..")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "private state path needs a final file name",
            )
        })?;
    let file = openat(
        &parent_handle,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(rustix_io)?;
    validate_unix_private_file(&file.metadata()?)?;
    Ok(file)
}

#[cfg(windows)]
fn open_private_read_platform(path: &Path) -> io::Result<File> {
    crate::win32::durable::open_private_read(path)
}

#[cfg(not(any(unix, windows)))]
fn open_private_read_platform(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private durable state files are not supported on this platform",
    ))
}

#[cfg(unix)]
fn remove_private_platform(path: &Path) -> io::Result<()> {
    let file = open_private_read(path)?;
    drop(file);
    fs::remove_file(path)?;
    sync_parent(path)
}

#[cfg(windows)]
fn remove_private_platform(path: &Path) -> io::Result<()> {
    crate::win32::durable::remove_private(path)
}

#[cfg(not(any(unix, windows)))]
fn remove_private_platform(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private durable state files are not supported on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_directory_platform(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

    let parent = parent(path);
    validate_unix_parent(parent)?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.mode() & 0o300 != 0o300 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state directory parent is not writable and searchable by its owner",
        ));
    }

    match open_unix_private_directory(path) {
        Ok(directory) => flush_unix_private_directory(path, &directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {
                    let directory = open_unix_private_directory(path)?;
                    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
                    flush_unix_private_directory(path, &directory)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let directory = open_unix_private_directory(path)?;
                    flush_unix_private_directory(path, &directory)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn ensure_private_child_directory_platform(path: &Path) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags, fchmod, mkdirat, open, openat};
    use std::os::unix::fs::MetadataExt as _;

    let name = path
        .file_name()
        .filter(|name| *name != "." && *name != "..")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "private application directory needs a final component",
            )
        })?;
    let parent_path = parent(path).canonicalize()?;
    let mut parent_handle = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(rustix_io)?;
    for component in parent_path.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        parent_handle = openat(
            &parent_handle,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(rustix_io)?;
    }
    let parent_metadata = parent_handle.metadata()?;
    if !parent_metadata.is_dir()
        || parent_metadata.uid() != rustix::process::geteuid().as_raw()
        || parent_metadata.mode() & 0o300 != 0o300
        || parent_metadata.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "application data parent must be user-owned and not writable by other users",
        ));
    }

    let created = match mkdirat(&parent_handle, name, Mode::RWXU) {
        Ok(()) => true,
        Err(rustix::io::Errno::EXIST) => false,
        Err(error) => return Err(rustix_io(error)),
    };
    let child = openat(
        &parent_handle,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(rustix_io)?;
    if created {
        fchmod(&child, Mode::from_bits_truncate(0o700)).map_err(rustix_io)?;
    }
    let child_metadata = child.metadata()?;
    if !child_metadata.is_dir()
        || child_metadata.uid() != rustix::process::geteuid().as_raw()
        || child_metadata.mode() & 0o700 != 0o700
        || (!created && child_metadata.mode() & 0o077 != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "application data directory is not owner-only",
        ));
    }
    child.sync_all()?;
    parent_handle.sync_all()
}

#[cfg(windows)]
fn ensure_private_child_directory_platform(path: &Path) -> io::Result<()> {
    crate::win32::workspace_fs::WorkspaceRoot::ensure_private_child_directory(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_child_directory_platform(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private application state directories are not supported on this platform",
    ))
}

#[cfg(unix)]
fn rustix_io(error: rustix::io::Errno) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(windows)]
fn ensure_private_directory_platform(path: &Path) -> io::Result<()> {
    use crate::win32::workspace_fs::WorkspaceRoot;

    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private Windows directory needs a Unicode final component",
        )
    })?;
    let parent = WorkspaceRoot::open(parent(path))?;
    match parent.open_dir_checked(&[name]) {
        Ok(directory) => {
            directory.flush_dir()?;
            parent.flush_dir()
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match parent.create_child_dir_exclusive(name) {
                Ok(directory) => {
                    directory.flush_dir()?;
                    parent.flush_dir()
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let directory = parent.open_dir_checked(&[name])?;
                    directory.flush_dir()?;
                    parent.flush_dir()
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_directory_platform(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private durable state directories are not supported on this platform",
    ))
}

#[cfg(unix)]
fn open_unix_private_directory(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(path)
}

#[cfg(unix)]
fn validate_unix_private_directory(directory: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.mode() & 0o700 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state directory is not owner-owned, owner-accessible, and owner-only",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn flush_unix_private_directory(path: &Path, directory: &File) -> io::Result<()> {
    validate_unix_private_directory(directory)?;
    directory.sync_all()?;
    sync_parent(path)
}

#[cfg(unix)]
fn create_private_temporary(
    parent: &Path,
    file_name: Option<&OsStr>,
) -> io::Result<(PathBuf, File)> {
    use std::os::unix::fs::OpenOptionsExt as _;

    loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name.unwrap_or_else(|| OsStr::new("state")));
        name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let path = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn validate_unix_parent(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state parent is not a directory",
        ));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state parent is not owned by this user with owner-only access",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn reject_unix_symlink_destination(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn validate_unix_private_file(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state file is not a single-link owner-only regular file",
        ));
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-platform-durable-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    #[cfg(unix)]
    #[test]
    fn application_directory_creation_is_private_and_reopen_is_read_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = fixture("app-data-parent");
        let child = parent.join("Nudox");
        ensure_private_child_directory(&child).expect("create private app directory");
        ensure_private_child_directory(&child).expect("reopen private app directory");
        assert_eq!(
            fs::metadata(&child)
                .expect("private app directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let _ = fs::remove_dir_all(parent);
    }

    #[cfg(unix)]
    #[test]
    fn application_directory_rejects_public_existing_child_and_symlink() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = fixture("app-data-reject");
        let public = parent.join("public");
        fs::create_dir(&public).expect("create public child");
        fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).expect("keep child public");
        assert!(ensure_private_child_directory(&public).is_err());

        let target = parent.join("target");
        fs::create_dir(&target).expect("create link target");
        let link = parent.join("linked");
        std::os::unix::fs::symlink(&target, &link).expect("create child symlink");
        assert!(ensure_private_child_directory(&link).is_err());

        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn repeated_publication_never_leaves_sibling_temporaries() {
        let root = fixture("replace");
        let path = root.join("state.json");
        for generation in 0_u64..64 {
            let bytes = generation.to_le_bytes();
            write_atomic(&path, &bytes).expect("publish generation");
            assert_eq!(fs::read(&path).expect("read generation"), bytes);
        }
        let names = fs::read_dir(&root)
            .expect("read fixture")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("state.json")]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_replacement_removes_the_private_temporary() {
        let root = fixture("cleanup");
        let destination = root.join("occupied");
        fs::create_dir(&destination).expect("occupied destination");
        assert!(write_atomic(&destination, b"state").is_err());
        let names = fs::read_dir(&root)
            .expect("read fixture")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("occupied")]);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn private_state_is_restricted_and_survives_a_cold_reopen() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let root = fixture("private-reopen");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("protect fixture directory");
        let path = root.join("journal.bin");
        write_private_atomic(&path, b"durable private state").expect("publish private state");
        let metadata = fs::symlink_metadata(&path).expect("file metadata");
        assert_eq!(metadata.mode() & 0o777, 0o600);

        let mut reopened = open_private_read(&path).expect("cold reopen private state");
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reopened, &mut bytes).expect("read private state");
        assert_eq!(bytes, b"durable private state");
        drop(reopened);
        remove_private(&path).expect("durably remove private state");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn private_open_rejects_a_symlink_and_a_nonprivate_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = fixture("private-reject");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("protect fixture directory");
        let target = root.join("target");
        write_private_atomic(&target, b"private").expect("publish target");
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        assert!(open_private_read(&link).is_err());

        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))
            .expect("make fixture nonprivate");
        assert!(write_private_atomic(&root.join("rejected"), b"state").is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("restore fixture permissions");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn private_directory_is_created_and_reopened_without_following_links() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = fixture("private-directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("protect fixture directory");
        let directory = root.join("journal");
        ensure_private_directory(&directory).expect("create private journal directory");
        ensure_private_directory(&directory).expect("verify reopened journal directory");

        let journal = directory.join("owner-ack.bin");
        write_private_atomic(&journal, b"durable owner acknowledgement")
            .expect("publish journal record");
        let mut reopened = open_private_read(&journal).expect("cold reopen journal record");
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reopened, &mut bytes).expect("read journal record");
        assert_eq!(bytes, b"durable owner acknowledgement");
        drop(reopened);

        let link = root.join("journal-link");
        symlink(&directory, &link).expect("create directory symlink");
        assert!(ensure_private_directory(&link).is_err());

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn private_directory_rejects_an_existing_directory_with_public_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = fixture("public-directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("protect fixture directory");
        let directory = root.join("journal");
        fs::create_dir(&directory).expect("create journal directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .expect("make journal directory public");

        assert_eq!(
            ensure_private_directory(&directory)
                .expect_err("reject public journal directory")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
