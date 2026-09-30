//! Directory capabilities for race-resistant local state access.
//!
//! A capability resolves its caller-supplied root once, pins the resulting
//! directory handle, then resolves every child name beneath that handle. On
//! Unix, opened descendants reject symlinks; on Windows, operations delegate
//! to the workspace root's handle-relative NT boundary.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

/// Kind of one direct child reported by a pinned directory capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link or platform reparse point.
    Link,
    /// A special filesystem object.
    Special,
}

/// One bounded direct-child observation from a pinned directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    /// Child name, not a path.
    pub name: OsString,
    /// Type observed without following a link.
    pub kind: EntryKind,
}

/// An open directory handle used to resolve children without re-walking the
/// directory's original pathname.
#[derive(Clone)]
pub struct DirectoryCapability {
    #[cfg(unix)]
    handle: Arc<File>,
    #[cfg(windows)]
    handle: Arc<crate::win32::workspace_fs::WorkspaceRoot>,
}

impl std::fmt::Debug for DirectoryCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectoryCapability")
            .finish_non_exhaustive()
    }
}

impl DirectoryCapability {
    /// Creates or opens a private directory at a local path, then retains a
    /// handle to the resulting directory. The parent must already exist; a
    /// missing final component is created relative to the pinned parent so a
    /// symlink cannot redirect directory creation.
    pub fn open_or_create_private(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("private directory has no valid final name"))?;
        match Self::open(path) {
            Ok(directory) => {
                directory.restrict_private()?;
                directory.validate_private()?;
                Ok(directory)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let parent = Self::open(parent)?;
                let directory = parent.create_private_dir(name)?;
                directory.validate_private()?;
                Ok(directory)
            }
            Err(error) => Err(error),
        }
    }

    /// Opens the supplied directory by walking its path without following
    /// symbolic links or reparse points. Every later operation is relative to
    /// that pinned handle.
    pub fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            return open_unix_path(path).map(|handle| Self {
                handle: Arc::new(handle),
            });
        }
        #[cfg(windows)]
        {
            return crate::win32::workspace_fs::WorkspaceRoot::open(path).map(|handle| Self {
                handle: Arc::new(handle),
            });
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    /// Opens one direct child directory without following a link.
    pub fn open_dir(&self, name: &str) -> io::Result<Self> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let handle = openat(
                self.handle.as_ref(),
                name,
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let handle = File::from(handle);
            if !handle.metadata()?.is_dir() {
                return Err(invalid("child is not a directory"));
            }
            return Ok(Self {
                handle: Arc::new(handle),
            });
        }
        #[cfg(windows)]
        {
            return self.handle.open_dir_checked(&[name]).map(|handle| Self {
                handle: Arc::new(handle),
            });
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Opens one direct directory and validates its owner-only permissions.
    pub fn open_private_dir(&self, name: &str) -> io::Result<Self> {
        let directory = self.open_dir(name)?;
        directory.validate_private()?;
        Ok(directory)
    }

    /// Validates this held directory's owner and private mode or ACL.
    pub fn validate_private(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::process::geteuid;
            use std::os::unix::fs::MetadataExt;
            let metadata = self.handle.metadata()?;
            if !metadata.is_dir()
                || metadata.uid() != geteuid().as_raw()
                || metadata.mode() & 0o077 != 0
                || metadata.mode() & 0o700 != 0o700
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "directory is not current-user-owned and private",
                ));
            }
            return Ok(());
        }
        #[cfg(windows)]
        {
            // WorkspaceRoot opens and validates its current-user-only DACL
            // through the held directory handle.
            return Ok(());
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    /// Applies owner-only permissions to this held directory.
    pub fn restrict_private(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::process::geteuid;
            use std::os::unix::fs::MetadataExt;
            let metadata = self.handle.metadata()?;
            if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "directory is not owned by the current user",
                ));
            }
            set_unix_private_directory(self.handle.as_ref())?;
            return Ok(());
        }
        #[cfg(windows)]
        {
            // Windows roots are opened only after the path-based constructor
            // has applied and verified the protected current-user DACL.
            return self.validate_private();
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    /// Creates one direct private directory and returns its pinned handle.
    pub fn create_private_dir(&self, name: &str) -> io::Result<Self> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, mkdirat};
            mkdirat(self.handle.as_ref(), name, Mode::from_bits_truncate(0o700))?;
            let child = self.open_dir(name)?;
            set_unix_private_directory(child.handle.as_ref())?;
            child.validate_private()?;
            self.sync_all()?;
            return Ok(child);
        }
        #[cfg(windows)]
        {
            return self
                .handle
                .create_child_dir_exclusive(name)
                .map(|handle| Self {
                    handle: Arc::new(handle),
                });
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Opens one direct regular file without following a link.
    pub fn open_file_read(&self, name: &str) -> io::Result<File> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let file = openat(
                self.handle.as_ref(),
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let file = File::from(file);
            if !file.metadata()?.is_file() {
                return Err(invalid("child is not a regular file"));
            }
            return Ok(file);
        }
        #[cfg(windows)]
        {
            return self.handle.open_file_read_checked(&[name]);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Opens a direct regular file and validates owner-only permissions.
    pub fn open_private_file(&self, name: &str) -> io::Result<File> {
        let file = self.open_file_read(name)?;
        validate_private_file(&file)?;
        Ok(file)
    }

    /// Opens or creates a direct regular file for reading and writing. New
    /// files are owner-only; existing files are opened with no-follow semantics.
    pub fn open_file_read_write(&self, name: &str, create: bool) -> io::Result<File> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            if create {
                flags |= OFlags::CREATE;
            }
            let file = openat(
                self.handle.as_ref(),
                name,
                flags,
                Mode::from_bits_truncate(0o600),
            )?;
            let file = File::from(file);
            validate_regular_file(&file)?;
            return Ok(file);
        }
        #[cfg(windows)]
        {
            return self.handle.open_file_read_write_checked(&[name], create);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (name, create);
            Err(unsupported())
        }
    }

    /// Opens or creates a direct private file for reading and writing.
    pub fn open_private_file_read_write(&self, name: &str, create: bool) -> io::Result<File> {
        let file = self.open_file_read_write(name, create)?;
        validate_private_file(&file)?;
        Ok(file)
    }

    /// Creates one direct regular file exclusively with owner-only mode or ACL.
    pub fn create_file_exclusive(&self, name: &str) -> io::Result<File> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let file = openat(
                self.handle.as_ref(),
                name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )?;
            let file = File::from(file);
            rustix::fs::fchmod(&file, Mode::from_bits_truncate(0o600))?;
            validate_private_file(&file)?;
            return Ok(file);
        }
        #[cfg(windows)]
        {
            return self.handle.create_file_exclusive(&[name]);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Atomically renames one direct child within the pinned directory.
    pub fn rename(&self, source: &str, destination: &str, replace: bool) -> io::Result<()> {
        validate_component(source)?;
        validate_component(destination)?;
        #[cfg(unix)]
        {
            use rustix::fs::renameat;
            if replace {
                renameat(
                    self.handle.as_ref(),
                    source,
                    self.handle.as_ref(),
                    destination,
                )?;
            } else {
                #[cfg(any(target_os = "linux", target_vendor = "apple", target_os = "redox"))]
                rustix::fs::renameat_with(
                    self.handle.as_ref(),
                    source,
                    self.handle.as_ref(),
                    destination,
                    rustix::fs::RenameFlags::NOREPLACE,
                )?;
                #[cfg(not(any(
                    target_os = "linux",
                    target_vendor = "apple",
                    target_os = "redox"
                )))]
                {
                    let _ = (source, destination);
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "atomic no-replace rename is unavailable on this platform",
                    ));
                }
            }
            return self.sync_all();
        }
        #[cfg(windows)]
        {
            if self.handle.child_is_directory(&[source])? {
                if replace {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory replacement is unsupported",
                    ));
                }
                return self
                    .handle
                    .rename_directory_relative(&[source], &[destination]);
            }
            return self
                .handle
                .rename_relative(&[source], &[destination], replace);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (source, destination);
            Err(unsupported())
        }
    }

    /// Removes one direct regular file without following a replacement link.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            unlinkat(self.handle.as_ref(), name, AtFlags::empty())?;
            return self.sync_all();
        }
        #[cfg(windows)]
        {
            return self.handle.remove_file_relative(&[name]);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Removes one empty direct directory.
    pub fn remove_dir(&self, name: &str) -> io::Result<()> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            unlinkat(self.handle.as_ref(), name, AtFlags::REMOVEDIR)?;
            return self.sync_all();
        }
        #[cfg(windows)]
        {
            return self.handle.remove_empty_dir(&[name]);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Removes one bounded directory tree by recursively opening each child
    /// beneath its already-held parent capability.
    pub fn remove_dir_all(&self, name: &str, maximum_entries: usize) -> io::Result<()> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            let child = self.open_dir(name)?;
            let mut visited = 0_usize;
            child.remove_contents(0, &mut visited, maximum_entries)?;
            return self.remove_dir(name);
        }
        #[cfg(windows)]
        {
            return self
                .handle
                .remove_dir_tree_limited(&[name], maximum_entries);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (name, maximum_entries);
            Err(unsupported())
        }
    }

    /// Lists direct children with an explicit cardinality ceiling.
    pub fn entries(&self, maximum: usize) -> io::Result<Vec<DirectoryEntry>> {
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, Dir, FileType, statat};
            let mut directory = Dir::read_from(self.handle.as_ref())?;
            let mut entries = Vec::new();
            while let Some(entry) = directory.read() {
                let entry = entry?;
                let raw_name = entry.file_name().to_bytes();
                if raw_name == b"." || raw_name == b".." {
                    continue;
                }
                if entries.len() >= maximum {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "directory entry limit exceeded",
                    ));
                }
                let name = OsString::from_vec(raw_name.to_vec());
                let stat = statat(self.handle.as_ref(), &name, AtFlags::SYMLINK_NOFOLLOW)?;
                let kind = match FileType::from_raw_mode(stat.st_mode) {
                    kind if kind.is_dir() => EntryKind::Directory,
                    kind if kind.is_file() => EntryKind::File,
                    kind if kind.is_symlink() => EntryKind::Link,
                    _ => EntryKind::Special,
                };
                entries.push(DirectoryEntry { name, kind });
            }
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            return Ok(entries);
        }
        #[cfg(windows)]
        {
            let entries = self.handle.read_dir_checked_limited(&[], maximum)?;
            let mut entries = entries
                .into_iter()
                .map(|entry| DirectoryEntry {
                    name: OsString::from(entry.name),
                    kind: match entry.kind {
                        crate::win32::workspace_fs::EntryKind::File => EntryKind::File,
                        crate::win32::workspace_fs::EntryKind::Directory => EntryKind::Directory,
                    },
                })
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.name.cmp(&right.name));
            return Ok(entries);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = maximum;
            Err(unsupported())
        }
    }

    /// Flushes the pinned directory handle.
    pub fn sync_all(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            return self.handle.sync_all();
        }
        #[cfg(windows)]
        {
            return self.handle.flush_dir();
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    #[cfg(unix)]
    fn remove_contents(
        &self,
        depth: usize,
        visited: &mut usize,
        maximum_entries: usize,
    ) -> io::Result<()> {
        if depth > 8 {
            return Err(invalid("directory tree is nested too deeply"));
        }
        for entry in self.entries(maximum_entries.saturating_sub(*visited))? {
            *visited = (*visited)
                .checked_add(1)
                .filter(|count| *count <= maximum_entries)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        "directory entry limit exceeded",
                    )
                })?;
            let name = entry
                .name
                .to_str()
                .ok_or_else(|| invalid("non-UTF-8 child name"))?;
            match entry.kind {
                EntryKind::File => self.remove_file(name)?,
                EntryKind::Directory => {
                    let child = self.open_dir(name)?;
                    child.remove_contents(depth + 1, visited, maximum_entries)?;
                    self.remove_dir(name)?;
                }
                EntryKind::Link | EntryKind::Special => {
                    return Err(invalid("directory tree contains a link or special file"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_unix_path(path: &Path) -> io::Result<File> {
    use rustix::fs::{CWD, Mode, OFlags, open, openat};
    use std::path::Component;
    let names = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(Ok(name.to_os_string())),
            Component::RootDir | Component::CurDir => None,
            Component::ParentDir | Component::Prefix(_) => Some(Err(invalid("unsafe root path"))),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let ancestor_flags = unix_search_directory_flags();
    let final_flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let mut current = if path.is_absolute() {
        let flags = if names.is_empty() {
            final_flags
        } else {
            ancestor_flags
        };
        File::from(open("/", flags, Mode::empty())?)
    } else {
        let flags = if names.is_empty() {
            final_flags
        } else {
            ancestor_flags
        };
        File::from(openat(CWD, ".", flags, Mode::empty())?)
    };
    #[cfg(target_os = "macos")]
    let mut at_system_root = path.is_absolute();
    for (index, name) in names.iter().enumerate() {
        let flags = if index + 1 == names.len() {
            final_flags
        } else {
            ancestor_flags
        };
        let child = openat(&current, name, flags, Mode::empty()).or_else(|error| {
            #[cfg(target_os = "macos")]
            if at_system_root
                && matches!(name.to_str(), Some("var" | "tmp"))
                && error.kind() == io::ErrorKind::NotADirectory
            {
                // macOS exposes these stable system paths as symlinks at
                // `/var` and `/tmp`. Resolve only these fixed aliases through
                // the already-held root; arbitrary symlinks stay rejected.
                let private = openat(&current, "private", ancestor_flags, Mode::empty())?;
                let private = File::from(private);
                let private_name_flags = if index + 1 == names.len() {
                    final_flags
                } else {
                    ancestor_flags
                };
                return openat(&private, name, private_name_flags, Mode::empty());
            }
            Err(error)
        })?;
        current = File::from(child);
        #[cfg(target_os = "macos")]
        {
            at_system_root = false;
        }
    }
    if !current.metadata()?.is_dir() {
        return Err(invalid("capability root is not a directory"));
    }
    Ok(current)
}

#[cfg(all(unix, target_os = "macos"))]
fn unix_search_directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    // macOS SDK sys/fcntl.h defines O_SEARCH as (O_EXEC | O_DIRECTORY);
    // rustix 1.1 does not expose the platform flag by name. This descriptor
    // is only used as an openat parent, never for reading or flushing.
    const O_EXEC: u32 = 0x4000_0000;
    OFlags::from_bits_retain(O_EXEC)
        | OFlags::DIRECTORY
        | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
}

#[cfg(all(unix, target_os = "linux"))]
fn unix_search_directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn unix_search_directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

#[cfg(unix)]
fn set_unix_private_directory(directory: &File) -> io::Result<()> {
    use rustix::fs::{Mode, fchmod};
    fchmod(directory, Mode::from_bits_truncate(0o700))
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(unix)]
fn validate_regular_file(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(invalid("child is not a regular file"));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_regular_file(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(invalid("child is not a regular file"));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file(file: &File) -> io::Result<()> {
    use rustix::process::geteuid;
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is not a private single-link file owned by the current user",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_file(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(invalid("child is not a regular file"));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_regular_file(_file: &File) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn validate_private_file(_file: &File) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(all(test, unix))]
mod tests {
    use super::DirectoryCapability;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        for _ in 0..64 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-platform-directory-{}-{id}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create directory capability fixture: {error}"),
            }
        }
        panic!("directory capability fixture capacity exhausted");
    }

    #[test]
    fn private_directory_creation_does_not_follow_symlinked_parent() {
        let root = scratch();
        let outside = root.join("outside");
        fs::create_dir(&outside).expect("outside directory");
        let link = root.join("link");
        symlink(&outside, &link).expect("symlink parent");

        let result = DirectoryCapability::open_or_create_private(&link.join("new"));
        assert!(result.is_err(), "symlinked parent is refused");
        assert!(
            !outside.join("new").exists(),
            "refused creation has no side effect through the symlink"
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn opening_fifo_as_regular_child_returns_without_blocking() {
        use rustix::fs::{Mode, mkfifoat};

        let root = scratch();
        let directory = DirectoryCapability::open(&root).expect("pin fixture");
        mkfifoat(
            directory.handle.as_ref(),
            "fifo",
            Mode::from_bits_truncate(0o600),
        )
        .expect("create FIFO");
        let error = directory
            .open_file_read("fifo")
            .expect_err("FIFO is not a regular file");
        assert_ne!(error.kind(), std::io::ErrorKind::WouldBlock);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn path_walk_uses_search_only_ancestor_handles() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = scratch();
        let ancestor = root.join("search-only");
        let child = ancestor.join("workspace");
        fs::create_dir(&ancestor).expect("create search-only ancestor");
        fs::create_dir(&child).expect("create accessible child");
        let original = fs::metadata(&ancestor)
            .expect("ancestor metadata")
            .permissions();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o300))
            .expect("allow search without enumeration");

        let capability = DirectoryCapability::open(&child)
            .expect("open child using held search-only ancestor handles");
        let mut file = capability
            .create_file_exclusive("known-child")
            .expect("create relative to pinned child");
        use std::io::Write as _;
        file.write_all(b"held").expect("write known child");
        file.sync_all().expect("sync known child");
        drop(file);
        fs::set_permissions(&ancestor, original).expect("restore ancestor permissions");
        assert_eq!(
            fs::read(child.join("known-child")).expect("read known child"),
            b"held"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn pinned_child_operations_survive_ancestor_rename_and_replacement() {
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::symlink;

        let root = scratch();
        let workspace = root.join("workspace");
        fs::create_dir(&workspace).expect("create workspace");
        let capability = DirectoryCapability::open(&workspace).expect("pin workspace");
        let mut file = capability
            .create_file_exclusive("fact")
            .expect("create relative fact");
        file.write_all(b"pinned").expect("write fact");
        file.sync_all().expect("sync fact");
        drop(file);

        let moved = root.join("workspace-real");
        fs::rename(&workspace, &moved).expect("move opened workspace");
        let outside = root.join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(outside.join("fact"), b"redirected").expect("write outside fact");
        symlink(&outside, &workspace).expect("replace original path with symlink");

        let mut file = capability
            .open_file_read("fact")
            .expect("read remains relative to held directory");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read held bytes");
        assert_eq!(bytes, b"pinned");
        fs::remove_dir_all(root).expect("remove fixture");
    }
}

fn validate_component(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(invalid("expected one safe path component"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "directory capabilities are unavailable on this platform",
    )
}
