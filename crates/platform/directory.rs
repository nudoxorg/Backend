//! Directory capabilities for race-resistant local state access.
//!
//! A capability pins a directory handle and resolves every child name beneath
//! that handle. On Unix, every opened component rejects symlinks; on Windows,
//! operations delegate to the workspace root's handle-relative NT boundary.

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
    /// handle to the resulting directory. Existing path components are walked
    /// without following symbolic links or reparse points.
    pub fn open_or_create_private(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(path)?;
            let directory = Self::open(path)?;
            directory.restrict_private()?;
            directory.validate_private()?;
            return Ok(directory);
        }
        #[cfg(windows)]
        {
            crate::win32::workspace_fs::WorkspaceRoot::ensure_private_child_directory(path)?;
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

    /// Opens an existing directory by walking its path without following any
    /// symbolic link or reparse point.
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
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
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
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
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
            let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
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
                    use rustix::fs::{AtFlags, statat};
                    match statat(self.handle.as_ref(), destination, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
                        Err(error) if error == rustix::io::Errno::NOENT => {}
                        Err(error) => return Err(io::Error::other(error.to_string())),
                    }
                    renameat(
                        self.handle.as_ref(),
                        source,
                        self.handle.as_ref(),
                        destination,
                    )?;
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

    let mut current = if path.is_absolute() {
        File::from(open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    } else {
        File::from(openat(
            CWD,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    };
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let child = openat(
                    &current,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                current = File::from(child);
            }
            Component::ParentDir | Component::Prefix(_) => return Err(invalid("unsafe root path")),
        }
    }
    if !current.metadata()?.is_dir() {
        return Err(invalid("capability root is not a directory"));
    }
    Ok(current)
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
