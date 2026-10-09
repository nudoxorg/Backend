//! Directory capabilities for race-resistant local state access.
//!
//! A capability resolves its caller-supplied root once, pins the resulting
//! directory handle, then resolves every child name beneath that handle. On
//! Unix, opened descendants reject symlinks; on Windows, operations delegate
//! to the workspace root's handle-relative NT boundary.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use crate::file_identity::FileIdentity;
use crate::linkage::{IfUnlinked, Linkage, open_admitted};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

const MAX_DIRECTORY_CLEANUP_ENTRIES: usize = 1_000_000;
const MAX_DIRECTORY_LINK_TARGET_BYTES: usize = 4096;

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

/// Result category for an atomic namespace rename followed by its directory
/// flush. A flush error occurs after the new name is already visible and must
/// not be handled like a pre-commit rename failure.
#[derive(Debug)]
pub enum DirectoryRenameError {
    /// The requested rename did not commit in the containing directory.
    NotCommitted(io::Error),
    /// The rename committed, but the containing directory could not be
    /// confirmed durable.
    CommittedButNotDurable(io::Error),
}

impl DirectoryRenameError {
    /// Returns the underlying filesystem error without erasing commit state.
    #[must_use]
    pub fn cause(&self) -> &io::Error {
        match self {
            Self::NotCommitted(error) | Self::CommittedButNotDurable(error) => error,
        }
    }

    /// Returns whether the namespace operation completed before the error.
    #[must_use]
    pub const fn committed(&self) -> bool {
        matches!(self, Self::CommittedButNotDurable(_))
    }

    /// Converts to `io::Error` while preserving committed state as its source.
    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::NotCommitted(error) => error,
            committed @ Self::CommittedButNotDurable(_) => {
                let kind = committed.cause().kind();
                io::Error::new(kind, committed)
            }
        }
    }
}

impl std::fmt::Display for DirectoryRenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCommitted(error) => {
                write!(formatter, "directory rename did not commit: {error}")
            }
            Self::CommittedButNotDurable(error) => write!(
                formatter,
                "directory rename committed but its durability could not be confirmed: {error}"
            ),
        }
    }
}

impl std::error::Error for DirectoryRenameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause())
    }
}

/// Failure when publishing a child through its creation receipt.
#[derive(Debug)]
pub enum CreatedDirectoryRenameError {
    /// The recorded source name no longer resolves to the held child.
    NameChanged(io::Error),
    /// The no-replace rename failed or committed without a confirmed flush.
    Rename(DirectoryRenameError),
}

impl CreatedDirectoryRenameError {
    /// Returns the underlying filesystem or identity-fence error.
    #[must_use]
    pub fn cause(&self) -> &io::Error {
        match self {
            Self::NameChanged(error) => error,
            Self::Rename(error) => error.cause(),
        }
    }

    /// Returns whether the namespace rename committed before the error.
    #[must_use]
    pub const fn committed(&self) -> bool {
        matches!(self, Self::Rename(error) if error.committed())
    }
}

impl std::fmt::Display for CreatedDirectoryRenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameChanged(error) => write!(
                formatter,
                "created directory identity changed before publication: {error}"
            ),
            Self::Rename(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for CreatedDirectoryRenameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::NameChanged(error) => error,
            Self::Rename(error) => error,
        })
    }
}

/// Ownership outcome of an exclusive private-directory creation.
///
/// A `NotCreated` error carries no authority to inspect or remove the requested
/// name. `CreatedButUnready` carries a pinned child receipt for cleanup. On
/// Windows, that receipt retains the handle returned by `FILE_CREATE`. On
/// Unix, `mkdirat` does not return a handle; the receipt binds the private
/// direct child observed by the following `openat`. Callers must exclude
/// same-user namespace mutation across that interval when they require the
/// observed child to be the object created by `mkdirat`.
#[derive(Debug)]
pub enum DirectoryCreateFailure {
    /// The exclusive creation did not create an entry.
    NotCreated(io::Error),
    /// Creation succeeded and a pinned child receipt is retained for rollback.
    /// The strength of its create-time identity guarantee is platform-specific;
    /// see the enum documentation.
    CreatedButUnready {
        /// Receipt for the exclusively created child.
        directory: CreatedDirectory,
        /// The setup operation that failed after creation.
        source: io::Error,
    },
    /// Creation succeeded, but the new child could not be pinned safely.
    /// No cleanup authority is carried, and the caller must treat this as
    /// terminal rather than probing the name and guessing ownership.
    CreatedButUnpinned(io::Error),
}

impl std::fmt::Display for DirectoryCreateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCreated(source) => {
                write!(formatter, "private directory was not created: {source}")
            }
            Self::CreatedButUnready { source, .. } => write!(
                formatter,
                "private directory was created but setup failed: {source}"
            ),
            Self::CreatedButUnpinned(source) => write!(
                formatter,
                "created directory could not be pinned; refusing name-based cleanup: {source}"
            ),
        }
    }
}

impl std::error::Error for DirectoryCreateFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotCreated(source)
            | Self::CreatedButUnpinned(source)
            | Self::CreatedButUnready { source, .. } => Some(source),
        }
    }
}

#[derive(Debug)]
struct DirectoryCreateRollbackFailure {
    create: io::Error,
    cleanup: io::Error,
}

impl std::fmt::Display for DirectoryCreateRollbackFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "private directory setup failed ({}); receipt-authorized rollback failed ({})",
            self.create, self.cleanup
        )
    }
}

impl std::error::Error for DirectoryCreateRollbackFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.create)
    }
}

/// Operational receipt tying one exclusive child-creation attempt to its
/// parent handle, direct name, and held child identity. Windows retains the
/// exact object returned by exclusive `FILE_CREATE`. Unix pins the private
/// direct occupant observed after `mkdirat`; because `mkdirat` returns no
/// handle, a same-user actor can move that object and install a private
/// replacement before the follow-up open, and this receipt would then identify
/// the replacement; cleanup authority therefore applies to the identity that
/// was pinned, not independently to the original `mkdirat` inode. Callers that
/// rely on create-time inode continuity must serialize same-user namespace
/// actors across creation and pinning. After pinning, identity checks prevent
/// the receipt from traversing a replacement observed at its direct name,
/// subject to the documented name-based cleanup race.
#[derive(Debug)]
pub struct CreatedDirectory {
    parent: DirectoryCapability,
    child: DirectoryCapability,
    name: String,
    cleanup: CreatedDirectoryCleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreatedDirectoryCleanup {
    /// Windows child handle pins the name and can remove the empty child itself.
    PinnedHandle,
    /// The handle allows the publication rename; cleanup reopens and compares identity.
    RenameableHandle,
    /// Windows FILE_CREATE returned this exact handle before setup completed;
    /// cleanup may delete only that still-empty object through the handle.
    #[cfg(windows)]
    CreatedHandle,
}

impl CreatedDirectory {
    /// Returns the requested direct-child name recorded by this receipt.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the held child capability recorded by this receipt.
    #[must_use]
    pub fn capability(&self) -> &DirectoryCapability {
        &self.child
    }

    /// Transfers the pinned child handle to a compatibility caller.
    #[must_use]
    pub fn into_capability(self) -> DirectoryCapability {
        self.child
    }

    /// Verifies that the requested direct name still resolves through the
    /// retained parent to the held child identity.
    ///
    /// This is an identity fence for cooperating namespace users. The check
    /// and a later path operation are separate system calls, so it does not
    /// exclude a same-user process that ignores the caller's namespace lock.
    pub fn verify_named(&self) -> io::Result<()> {
        let named = self.parent.open_private_dir(&self.name)?;
        if !named.same_object_as(&self.child)? {
            return Err(invalid(
                "created directory name now refers to another object",
            ));
        }
        Ok(())
    }

    /// Renames this receipt's direct child to an unused direct name, preserving
    /// the rename's commit status. The identity check and rename are separate
    /// calls; callers must hold their cooperative namespace guard throughout.
    pub fn rename_noreplace(
        &self,
        destination: &str,
    ) -> Result<(), CreatedDirectoryRenameError> {
        self.verify_named()
            .map_err(CreatedDirectoryRenameError::NameChanged)?;
        self.parent
            .rename_with_outcome(&self.name, destination, false)
            .map_err(CreatedDirectoryRenameError::Rename)
    }

    /// Verifies that a raw path resolves to the retained parent and child
    /// capabilities. This catches replacement of either the parent path or
    /// the child name before and after path-based operations.
    ///
    /// The identity checks bracket a path operation but cannot lock out a
    /// same-user process that bypasses the caller's cooperative namespace
    /// protocol. Use capability-relative operations when they are available.
    pub fn verify_path(&self, path: &Path) -> io::Result<()> {
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let path_parent = DirectoryCapability::open(parent_path)?;
        if !path_parent.same_object_as(&self.parent)? {
            return Err(invalid(
                "created directory path resolves through another parent",
            ));
        }
        let path_child = DirectoryCapability::open(path)?;
        if !path_child.same_object_as(&self.child)? {
            return Err(invalid(
                "created directory path resolves to another object",
            ));
        }
        Ok(())
    }

    /// Removes this created directory using only the cleanup authority carried
    /// by the receipt. Once setup succeeded, cleanup checks that the direct name
    /// still refers to the held child identity before traversing it.
    ///
    /// On Unix, the final `unlinkat` is name-based: the identity is checked
    /// immediately before unlink, but Unix has no unlink-by-open-directory-handle
    /// operation, so a same-user rename in that final syscall window cannot be
    /// excluded by this API. This API also cannot prove that the observed child
    /// is the inode originally made by `mkdirat` if a same-user process changed
    /// the name before the initial pin. No replacement is traversed after an
    /// identity mismatch is observed.
    pub fn remove_all(&self, maximum_entries: usize) -> io::Result<()> {
        #[cfg(unix)]
        {
            let _ = self.cleanup;
            let expected = FileIdentity::of_file(self.child.handle.as_ref())?;
            let child = self.parent.open_private_dir(&self.name)?;
            if FileIdentity::of_file(child.handle.as_ref())? != expected {
                return Err(invalid(
                    "created directory name now refers to another object",
                ));
            }
            let mut visited = 0_usize;
            child.remove_contents(0, &mut visited, maximum_entries)?;
            let current = self.parent.open_private_dir(&self.name)?;
            if FileIdentity::of_file(current.handle.as_ref())? != expected {
                return Err(invalid("created directory name changed during cleanup"));
            }
            self.parent.remove_dir(&self.name)
        }
        #[cfg(windows)]
        {
            match self.cleanup {
                CreatedDirectoryCleanup::PinnedHandle
                | CreatedDirectoryCleanup::CreatedHandle => {
                    self.child.handle.remove_created_empty_self()
                }
                CreatedDirectoryCleanup::RenameableHandle => self
                    .parent
                    .handle
                    .remove_created_child_if_same(&self.name, &self.child.handle, maximum_entries),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = maximum_entries;
            Err(unsupported())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapabilityPurpose {
    PrivateState,
    ReadOnlySource,
}

/// An open directory handle used to resolve children without re-walking the
/// directory's original pathname.
#[derive(Clone)]
pub struct DirectoryCapability {
    #[cfg(unix)]
    handle: Arc<File>,
    #[cfg(windows)]
    handle: Arc<crate::win32::workspace_fs::WorkspaceRoot>,
    purpose: CapabilityPurpose,
}

impl std::fmt::Debug for DirectoryCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DirectoryCapability")
            .finish_non_exhaustive()
    }
}

impl DirectoryCapability {
    fn same_object_as(&self, other: &Self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            return Ok(FileIdentity::of_file(self.handle.as_ref())?
                == FileIdentity::of_file(other.handle.as_ref())?);
        }
        #[cfg(windows)]
        {
            return self.handle.same_object_as(&other.handle);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = other;
            Err(unsupported())
        }
    }

    /// Verifies that a raw path still resolves to this held directory object.
    ///
    /// This identity check does not pin the pathname for a later operation;
    /// callers using path-based APIs must keep their cooperative namespace
    /// guard for the operation's full duration.
    pub fn verify_path(&self, path: &Path) -> io::Result<()> {
        let observed = if self.purpose == CapabilityPurpose::ReadOnlySource {
            Self::open_read_only_source(path)?
        } else {
            Self::open(path)?
        };
        if !observed.same_object_as(self)? {
            return Err(invalid("directory path resolves to another object"));
        }
        Ok(())
    }

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
                purpose: CapabilityPurpose::PrivateState,
            });
        }
        #[cfg(windows)]
        {
            return crate::win32::workspace_fs::WorkspaceRoot::open(path).map(|handle| Self {
                handle: Arc::new(handle),
                purpose: CapabilityPurpose::PrivateState,
            });
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    /// Opens an ordinary source checkout for read-only inspection. Unlike a
    /// persisted-state capability, this does not require owner-only ACLs on
    /// Windows; it still rejects reparse points and pins every traversed
    /// directory handle. Mutation methods refuse this capability.
    pub fn open_read_only_source(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            return open_unix_path(path).map(|handle| Self {
                handle: Arc::new(handle),
                purpose: CapabilityPurpose::ReadOnlySource,
            });
        }
        #[cfg(windows)]
        {
            return crate::win32::workspace_fs::WorkspaceRoot::open_read_only_source(path).map(
                |handle| Self {
                    handle: Arc::new(handle),
                    purpose: CapabilityPurpose::ReadOnlySource,
                },
            );
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
                purpose: self.purpose,
            });
        }
        #[cfg(windows)]
        {
            let handle = if self.purpose == CapabilityPurpose::ReadOnlySource {
                self.handle.open_dir_source_checked(&[name])
            } else {
                self.handle.open_dir_checked(&[name])
            }?;
            return Ok(Self {
                handle: Arc::new(handle),
                purpose: self.purpose,
            });
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Reads the raw target of one direct symbolic-link child without
    /// following it. The caller supplies a bound no larger than the platform
    /// capability limit; an overlong target is refused instead of truncated.
    pub fn read_link_target(&self, name: &str, maximum_bytes: usize) -> io::Result<PathBuf> {
        validate_component(name)?;
        if maximum_bytes > MAX_DIRECTORY_LINK_TARGET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "symbolic-link target bound exceeds the capability limit",
            ));
        }
        #[cfg(unix)]
        {
            use rustix::fs::readlinkat_raw;
            use std::mem::MaybeUninit;

            let mut storage = [MaybeUninit::<u8>::uninit(); MAX_DIRECTORY_LINK_TARGET_BYTES + 1];
            let capacity = maximum_bytes + 1;
            let (target, _) = readlinkat_raw(self.handle.as_ref(), name, &mut storage[..capacity])?;
            if target.len() > maximum_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "symbolic-link target exceeds the requested limit",
                ));
            }
            return Ok(PathBuf::from(OsString::from_vec(target.to_vec())));
        }
        #[cfg(not(unix))]
        {
            let _ = (name, maximum_bytes);
            Err(unsupported())
        }
    }

    /// Opens one direct directory and validates its owner-only permissions.
    pub fn open_private_dir(&self, name: &str) -> io::Result<Self> {
        let directory = self.open_dir(name)?;
        directory.validate_private()?;
        Ok(directory)
    }

    /// Opens one owner-only child directory, creating it with private
    /// permissions when it is absent.
    ///
    /// Existing children are checked through a no-follow handle and are never
    /// chmodded or otherwise repaired. A concurrent creator is reopened and
    /// subjected to the same owner and permission checks.
    pub fn open_or_create_private_dir(&self, name: &str) -> io::Result<Self> {
        match self.open_private_dir(name) {
            Ok(directory) => Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match self.create_private_dir(name) {
                    Ok(directory) => Ok(directory),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        self.open_private_dir(name)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Validates this held directory's owner and private mode or ACL.
    pub fn validate_private(&self) -> io::Result<()> {
        if self.purpose == CapabilityPurpose::ReadOnlySource {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "read-only source capability is not a private state directory",
            ));
        }
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
        if self.purpose == CapabilityPurpose::ReadOnlySource {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "read-only source capability cannot be made private",
            ));
        }
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
    /// This compatibility entrypoint delegates to the tracked create path and
    /// rolls back a post-create setup failure through its pinned-child receipt.
    pub fn create_private_dir(&self, name: &str) -> io::Result<Self> {
        match self.create_private_dir_with_policy(
            name,
            CreatedDirectoryCleanup::PinnedHandle,
            || Ok(()),
            || Ok(()),
            || Ok(()),
        ) {
            Ok(created) => Ok(created.into_capability()),
            Err(DirectoryCreateFailure::NotCreated(source)) => Err(source),
            Err(DirectoryCreateFailure::CreatedButUnpinned(source)) => {
                Err(io::Error::other(DirectoryCreateFailure::CreatedButUnpinned(
                    source,
                )))
            }
            Err(DirectoryCreateFailure::CreatedButUnready { directory, source }) => {
                match directory.remove_all(MAX_DIRECTORY_CLEANUP_ENTRIES) {
                    Ok(()) => Err(source),
                    Err(cleanup) => Err(io::Error::other(DirectoryCreateRollbackFailure {
                        create: source,
                        cleanup,
                    })),
                }
            }
        }
    }

    /// Creates one direct private child while retaining an identity receipt for
    /// cleanup and publication. A pre-create error carries no authority over
    /// the requested name. Unix receipts observe the occupant after `mkdirat`,
    /// so callers requiring proof of the originally-created inode must
    /// serialize same-user namespace actors across this call; see
    /// [`CreatedDirectory`] for the exact platform boundary.
    pub fn create_private_dir_tracked(
        &self,
        name: &str,
    ) -> Result<CreatedDirectory, DirectoryCreateFailure> {
        self.create_private_dir_with_policy(
            name,
            CreatedDirectoryCleanup::RenameableHandle,
            || Ok(()),
            || Ok(()),
            || Ok(()),
        )
    }

    fn create_private_dir_with_policy(
        &self,
        name: &str,
        cleanup: CreatedDirectoryCleanup,
        before_create: impl FnOnce() -> io::Result<()>,
        after_create: impl FnOnce() -> io::Result<()>,
        after_pin: impl FnOnce() -> io::Result<()>,
    ) -> Result<CreatedDirectory, DirectoryCreateFailure> {
        self.ensure_writable()
            .map_err(DirectoryCreateFailure::NotCreated)?;
        validate_component(name).map_err(DirectoryCreateFailure::NotCreated)?;
        before_create().map_err(DirectoryCreateFailure::NotCreated)?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, mkdirat};
            mkdirat(self.handle.as_ref(), name, Mode::from_bits_truncate(0o700))
                .map_err(|error| DirectoryCreateFailure::NotCreated(io::Error::from(error)))?;
            if let Err(source) = after_create() {
                return Err(DirectoryCreateFailure::CreatedButUnpinned(source));
            }
            let child = match self.open_dir(name) {
                Ok(child) => child,
                Err(source) => {
                    return Err(DirectoryCreateFailure::CreatedButUnpinned(source));
                }
            };
            let created = CreatedDirectory {
                parent: self.clone(),
                child,
                name: name.to_owned(),
                cleanup,
            };
            if let Err(source) = after_pin()
                .and_then(|()| set_unix_private_directory(created.child.handle.as_ref()))
                .and_then(|()| created.child.validate_private())
                .and_then(|()| self.sync_all())
            {
                return Err(DirectoryCreateFailure::CreatedButUnready {
                    directory: created,
                    source,
                });
            }
            Ok(created)
        }
        #[cfg(windows)]
        {
            use crate::win32::workspace_fs::WorkspaceDirectoryCreateFailure;
            let child = match self.handle.create_child_dir_exclusive_tracked(
                name,
                cleanup == CreatedDirectoryCleanup::RenameableHandle,
            ) {
                Ok(child) => child,
                Err(WorkspaceDirectoryCreateFailure::NotCreated(source)) => {
                    return Err(DirectoryCreateFailure::NotCreated(source));
                }
                Err(WorkspaceDirectoryCreateFailure::Created { directory, source }) => {
                    let created = CreatedDirectory {
                        parent: self.clone(),
                        child: Self {
                            handle: Arc::new(directory),
                            purpose: CapabilityPurpose::PrivateState,
                        },
                        name: name.to_owned(),
                        // This handle is the one returned by exclusive FILE_CREATE.
                        // Setup did not complete, so rollback uses that exact
                        // handle and never reopens the not-yet-admitted ACL by name.
                        cleanup: CreatedDirectoryCleanup::CreatedHandle,
                    };
                    return Err(DirectoryCreateFailure::CreatedButUnready {
                        directory: created,
                        source,
                    });
                }
            };
            let created = CreatedDirectory {
                parent: self.clone(),
                child: Self {
                    handle: Arc::new(child),
                    purpose: CapabilityPurpose::PrivateState,
                },
                name: name.to_owned(),
                cleanup,
            };
            if let Err(source) = after_create().and_then(|()| after_pin()) {
                return Err(DirectoryCreateFailure::CreatedButUnready {
                    directory: created,
                    source,
                });
            }
            Ok(created)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (name, cleanup, after_create, after_pin);
            Err(DirectoryCreateFailure::NotCreated(unsupported()))
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
            return if self.purpose == CapabilityPurpose::ReadOnlySource {
                self.handle.open_file_read_source_checked(&[name])
            } else {
                self.handle.open_file_read_checked(&[name])
            };
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    /// Opens a direct regular file and validates owner-only permissions.
    ///
    /// A writer that publishes with a replacing rename may unlink the file between this open and
    /// its checks. The reader keeps the complete generation it opened instead of failing or
    /// chasing the name: see [`IfUnlinked::Keep`].
    pub fn open_private_file(&self, name: &str) -> io::Result<File> {
        validate_component(name)?;
        open_private(IfUnlinked::Keep, || {
            #[cfg(windows)]
            return self.handle.open_file_read_checked(&[name]);
            #[cfg(not(windows))]
            return self.open_file_read(name);
        })
    }

    /// Opens or creates a direct regular file for reading and writing. New
    /// files are owner-only; existing files are opened with no-follow semantics.
    pub fn open_file_read_write(&self, name: &str, create: bool) -> io::Result<File> {
        self.ensure_writable()?;
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
    ///
    /// A writer must act on the file the name refers to now, so a file unlinked between the open
    /// and its checks is discarded and the name is opened again: see [`IfUnlinked::Reopen`].
    pub fn open_private_file_read_write(&self, name: &str, create: bool) -> io::Result<File> {
        open_private(IfUnlinked::Reopen, || {
            if !create {
                return self.open_file_read_write(name, false);
            }
            match self.create_file_exclusive(name) {
                Ok(file) => Ok(file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.open_file_read_write(name, false)
                }
                Err(error) => Err(error),
            }
        })
    }

    /// Creates one direct regular file exclusively with owner-only mode or ACL.
    pub fn create_file_exclusive(&self, name: &str) -> io::Result<File> {
        self.ensure_writable()?;
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
            validate_private_file(&file, Linkage::Named)?;
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
        self.rename_with_outcome(source, destination, replace)
            .map_err(DirectoryRenameError::into_io_error)
    }

    /// Atomically renames one direct child and preserves whether a subsequent
    /// directory-flush failure happened before or after the namespace commit.
    pub fn rename_with_outcome(
        &self,
        source: &str,
        destination: &str,
        replace: bool,
    ) -> Result<(), DirectoryRenameError> {
        self.ensure_writable()
            .map_err(DirectoryRenameError::NotCommitted)?;
        validate_component(source).map_err(DirectoryRenameError::NotCommitted)?;
        validate_component(destination).map_err(DirectoryRenameError::NotCommitted)?;
        #[cfg(unix)]
        {
            use rustix::fs::renameat;
            return rename_then_sync(
                || {
                    if replace {
                        renameat(
                            self.handle.as_ref(),
                            source,
                            self.handle.as_ref(),
                            destination,
                        )
                        .map_err(io::Error::from)
                    } else {
                        #[cfg(any(
                            target_os = "linux",
                            target_vendor = "apple",
                            target_os = "redox"
                        ))]
                        {
                            rustix::fs::renameat_with(
                                self.handle.as_ref(),
                                source,
                                self.handle.as_ref(),
                                destination,
                                rustix::fs::RenameFlags::NOREPLACE,
                            )
                            .map_err(io::Error::from)
                        }
                        #[cfg(not(any(
                            target_os = "linux",
                            target_vendor = "apple",
                            target_os = "redox"
                        )))]
                        {
                            let _ = (source, destination);
                            Err(io::Error::new(
                                io::ErrorKind::Unsupported,
                                "atomic no-replace rename is unavailable on this platform",
                            ))
                        }
                    }
                },
                || self.sync_all(),
            );
        }
        #[cfg(windows)]
        {
            if self
                .handle
                .child_is_directory(&[source])
                .map_err(DirectoryRenameError::NotCommitted)?
            {
                if replace {
                    return Err(DirectoryRenameError::NotCommitted(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "directory replacement is unsupported",
                    )));
                }
                return self
                    .handle
                    .rename_directory_relative_with_outcome(&[source], &[destination]);
            }
            return self
                .handle
                .rename_relative_with_outcome(&[source], &[destination], replace);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (source, destination);
            Err(DirectoryRenameError::NotCommitted(unsupported()))
        }
    }

    /// Removes one direct regular file without following a replacement link.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        self.ensure_writable()?;
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
        self.ensure_writable()?;
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
        self.ensure_writable()?;
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
            use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
            // `Dir::read_from` can share an open-file-description cursor with
            // a cloned capability. Open `.` relative to the held directory
            // to get an independent enumeration handle on every call.
            let enumeration_handle = openat(
                self.handle.as_ref(),
                ".",
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let mut directory = Dir::read_from(&enumeration_handle)?;
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
            let entries = if self.purpose == CapabilityPurpose::ReadOnlySource {
                self.handle.read_dir_source_checked_limited(&[], maximum)?
            } else {
                self.handle.read_dir_checked_limited(&[], maximum)?
            };
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
        self.ensure_writable()?;
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

    fn ensure_writable(&self) -> io::Result<()> {
        if self.purpose == CapabilityPurpose::ReadOnlySource {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "read-only source capability cannot mutate its directory",
            ))
        } else {
            Ok(())
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

/// A verified private workspace directory that creates children without
/// relying on the process umask.
///
/// `open` requires an existing trusted parent: a current-user-owned parent
/// that is not group- or world-writable, or a root-owned sticky temporary
/// directory. Each child is created relative to a pinned directory handle
/// with owner-only permissions. Existing children must already be owned by
/// the current user and private. `open`, `child`, and `under_user_data` never
/// change existing directory permissions; only the explicitly application-owned
/// [`Self::under_user_data_application`] boundary permits a scoped repair.
/// Path-based consumers can call [`Self::verify_path`] immediately before
/// using [`Self::path`].
#[derive(Clone)]
pub struct OwnedWorkspaceDirectory {
    path: PathBuf,
    directory: DirectoryCapability,
}

impl std::fmt::Debug for OwnedWorkspaceDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedWorkspaceDirectory")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl OwnedWorkspaceDirectory {
    /// Establishes private application state beneath an existing user-owned
    /// profile/data anchor and its known operating-system directory suffix.
    /// Existing conventional directories may be readable, but never writable
    /// by other users. Their permissions are preserved. Missing components
    /// are created privately relative to held handles; links are never followed.
    pub fn under_user_data(anchor: &Path, suffix: &[&str], application: &str) -> io::Result<Self> {
        Self::under_user_data_impl(anchor, suffix, application, false)
    }

    /// Establishes a known application-owned state boundary, repairing only
    /// that final current-user-owned directory on Unix when an older build
    /// created it with readable permissions. Conventional profile/data parents
    /// keep strict ownership and non-writable admission; links are refused.
    /// Windows retains existing protected-DACL admission.
    pub fn under_user_data_application(
        anchor: &Path,
        suffix: &[&str],
        application: &str,
    ) -> io::Result<Self> {
        Self::under_user_data_impl(anchor, suffix, application, true)
    }

    fn under_user_data_impl(
        anchor: &Path,
        suffix: &[&str],
        application: &str,
        repair_application: bool,
    ) -> io::Result<Self> {
        #[cfg(not(unix))]
        let _ = repair_application;
        if !anchor.is_absolute() {
            return Err(invalid("user data anchor must be absolute"));
        }
        validate_component(application)?;
        for name in suffix {
            validate_component(name)?;
        }
        #[cfg(unix)]
        let directory = {
            let mut parent = DirectoryCapability::open(anchor)?;
            validate_user_data_parent(&parent)?;
            let mut path = anchor.to_path_buf();
            for name in suffix {
                parent.verify_path(&path)?;
                let child = match parent.open_dir(name) {
                    Ok(child) => child,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        match parent.create_private_dir(name) {
                            Ok(child) => child,
                            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                                parent.open_dir(name)?
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => return Err(error),
                };
                validate_user_data_parent(&child)?;
                path.push(name);
                child.verify_path(&path)?;
                parent = child;
            }
            parent.verify_path(&path)?;
            if repair_application {
                let application_path = path.join(application);
                crate::durable::ensure_private_application_directory(&application_path)
                    .map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!("{}: {error}", application_path.display()),
                        )
                    })?;
                parent.verify_path(&path)?;
            }
            parent.open_or_create_private_dir(application)?
        };
        #[cfg(windows)]
        let directory = DirectoryCapability {
            handle: Arc::new(crate::win32::workspace_fs::WorkspaceRoot::under_user_data(
                anchor,
                suffix,
                application,
            )?),
            purpose: CapabilityPurpose::PrivateState,
        };
        #[cfg(not(any(unix, windows)))]
        let directory = {
            return Err(unsupported());
        };
        let path = suffix
            .iter()
            .fold(anchor.to_path_buf(), |path, name| path.join(name))
            .join(application);
        let workspace = Self { path, directory };
        workspace.verify_path()?;
        Ok(workspace)
    }

    /// Opens an existing private directory or creates it beneath a verified
    /// private parent (or root-owned sticky temporary directory).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let requested = path.as_ref();
        let path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            std::env::current_dir()?.join(requested)
        };
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(invalid("owned workspace path contains a parent component"));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("owned workspace path has no valid final name"))?;
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = DirectoryCapability::open(parent_path)?;
        validate_workspace_parent(&parent)?;
        let directory = parent.open_or_create_private_dir(name)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
        let workspace = Self { path, directory };
        workspace.verify_path()?;
        Ok(workspace)
    }

    /// Opens or creates a private direct child relative to this pinned
    /// workspace directory.
    pub fn child(&self, name: &str) -> io::Result<Self> {
        let path = self.path.join(name);
        let directory = self.directory.open_or_create_private_dir(name)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))?;
        let workspace = Self {
            path,
            directory,
        };
        workspace.verify_path()?;
        Ok(workspace)
    }

    /// Opens or creates a private descendant beneath this workspace.
    /// Absolute paths, parent components, and non-normal components are
    /// rejected; each normal component is resolved through the preceding
    /// pinned directory handle.
    pub fn descendant(&self, relative: impl AsRef<Path>) -> io::Result<Self> {
        let relative = relative.as_ref();
        if relative.is_absolute() {
            return Err(invalid("owned workspace descendant must be relative"));
        }
        let mut directory = self.clone();
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(invalid("owned workspace descendant has an unsafe component"));
            };
            let name = name
                .to_str()
                .ok_or_else(|| invalid("owned workspace descendant name is not valid text"))?;
            directory = directory.child(name)?;
        }
        Ok(directory)
    }

    /// Returns the path associated with this pinned directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verifies that this directory is still private and its path still names
    /// the pinned directory object.
    pub fn verify_path(&self) -> io::Result<()> {
        self.directory.validate_private()?;
        self.directory.verify_path(&self.path)
    }
}

#[cfg(unix)]
fn validate_user_data_parent(parent: &DirectoryCapability) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = parent.handle.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o300 != 0o300
        || metadata.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "user data directory must be user-owned and not writable by other users",
        ));
    }
    Ok(())
}
#[cfg(unix)]
fn validate_workspace_parent(parent: &DirectoryCapability) -> io::Result<()> {
    use rustix::process::geteuid;
    use std::os::unix::fs::MetadataExt as _;

    let metadata = parent.handle.metadata()?;
    let mode = metadata.mode();
    let user_owned = metadata.uid() == geteuid().as_raw()
        && mode & 0o300 == 0o300
        && mode & 0o022 == 0;
    let protected_temporary = metadata.uid() == 0 && mode & 0o1000 != 0;
    if !metadata.is_dir() || !(user_owned || protected_temporary) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "workspace parent must be user-owned and not writable by other users, or root-owned and sticky",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_workspace_parent(parent: &DirectoryCapability) -> io::Result<()> {
    parent.validate_private()
}

#[cfg(not(any(unix, windows)))]
fn validate_workspace_parent(_parent: &DirectoryCapability) -> io::Result<()> {
    Err(unsupported())
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

/// Opens a private file and admits it, applying `if_unlinked` when a replacing rename removes its
/// last name between the open and the checks.
fn open_private(if_unlinked: IfUnlinked, open: impl Fn() -> io::Result<File>) -> io::Result<File> {
    open_admitted(
        if_unlinked,
        open,
        validate_private_file,
        file_is_unlinked,
        std::thread::sleep,
    )
}

#[cfg(unix)]
fn validate_private_file(file: &File, linkage: Linkage) -> io::Result<()> {
    use rustix::process::geteuid;
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || !linkage.admits(metadata.nlink())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is not a private single-link file owned by the current user",
        ));
    }
    Ok(())
}

/// Whether the file's last name was removed since it was opened.
#[cfg(unix)]
fn file_is_unlinked(file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    file.metadata().is_ok_and(|metadata| metadata.nlink() == 0)
}

/// The Windows workspace opens admit link counts and ownership on the handle themselves.
#[cfg(windows)]
fn validate_private_file(file: &File, _linkage: Linkage) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(invalid("child is not a regular file"));
    }
    Ok(())
}

/// The Windows workspace opens reopen a replaced name themselves.
#[cfg(windows)]
fn file_is_unlinked(_file: &File) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn validate_regular_file(_file: &File) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn validate_private_file(_file: &File, _linkage: Linkage) -> io::Result<()> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn file_is_unlinked(_file: &File) -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::{
        CreatedDirectoryCleanup, DirectoryCapability, DirectoryCreateFailure,
        OwnedWorkspaceDirectory,
    };
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;
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

    #[cfg(unix)]
    fn assert_private_mode(path: &Path) {
        let mode = fs::metadata(path)
            .expect("workspace directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "workspace directory mode for {path:?}");
    }

    #[test]
    fn default_user_data_state_keeps_its_pinned_identity_and_refuses_public_app_repair() {
        let root = scratch();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        let app = OwnedWorkspaceDirectory::under_user_data(&home, &["Library", "Application Support"], "Nudox")
            .expect("empty public home bootstrap");
        assert_private_mode(app.path());
        app.verify_path().expect("pinned original path");
        let moved = root.join("moved-home");
        fs::rename(&home, &moved).unwrap();
        fs::create_dir(&home).unwrap();
        assert!(app.verify_path().is_err(), "a replacement pathname cannot inherit the pinned identity");
        drop(app);
        let public_app = home.join("Nudox");
        fs::create_dir(&public_app).unwrap();
        fs::set_permissions(&public_app, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(OwnedWorkspaceDirectory::under_user_data(&home, &[], "Nudox").is_err());
        assert_eq!(fs::metadata(&public_app).unwrap().mode() & 0o777, 0o755);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owned_workspace_permission_probe_child() {
        let Some(mask) = std::env::var_os("BACKEND_PLATFORM_WORKSPACE_UMASK_PROBE") else {
            return;
        };
        let mask = u16::from_str_radix(mask.to_str().expect("umask text"), 8)
            .expect("umask value");
        let _previous_mask = rustix::process::umask(rustix::fs::Mode::from_bits_truncate(mask.into()));
        let root = std::env::temp_dir().join(format!(
            "backend-platform-workspace-umask-{}-{}",
            std::process::id(),
            mask
        ));
        let root_capability = DirectoryCapability::open_or_create_private(&root)
            .expect("create isolated private workspace fixture");
        assert_private_mode(&root);
        let workspace = OwnedWorkspaceDirectory::open(root.join("workspace"))
            .expect("open private workspace root");
        let search = workspace
            .child("search-index-v2")
            .expect("create private Tantivy root");
        let objects = search.child("objects").expect("create private object child");
        assert_private_mode(workspace.path());
        assert_private_mode(search.path());
        assert_private_mode(objects.path());

        drop(objects);
        drop(search);
        drop(workspace);
        let reopened = OwnedWorkspaceDirectory::open(root.join("workspace"))
            .expect("reopen existing workspace without repairing it");
        let reopened_search = reopened
            .child("search-index-v2")
            .expect("reopen existing Tantivy root");
        assert_private_mode(reopened.path());
        assert_private_mode(reopened_search.path());
        drop((reopened_search, reopened, root_capability));
        fs::remove_dir_all(root).expect("remove isolated private workspace fixture");
    }

    #[cfg(unix)]
    #[test]
    fn owned_workspace_directories_ignore_group_and_restrictive_umasks() {
        for mask in ["002", "077"] {
            let status = Command::new(std::env::current_exe().expect("test executable"))
                .args(["owned_workspace_permission_probe_child", "--nocapture"])
                .env("BACKEND_PLATFORM_WORKSPACE_UMASK_PROBE", mask)
                .status()
                .expect("launch isolated umask test process");
            assert!(status.success(), "workspace umask probe for {mask} failed");
        }
    }

    #[cfg(unix)]
    #[test]
    fn owned_workspace_reopen_refuses_group_writable_or_symlinked_children() {
        let root = scratch();
        let parent = DirectoryCapability::open_or_create_private(&root)
            .expect("make workspace fixture private");
        let workspace = parent
            .open_or_create_private_dir("workspace")
            .expect("create workspace root");
        let unsafe_path = root.join("workspace/unsafe");
        fs::create_dir(&unsafe_path).expect("create unsafe preexisting child");
        fs::set_permissions(&unsafe_path, fs::Permissions::from_mode(0o770))
            .expect("make child group-writable");
        let before = fs::metadata(&unsafe_path)
            .expect("unsafe child metadata")
            .permissions()
            .mode()
            & 0o777;
        assert!(workspace.open_or_create_private_dir("unsafe").is_err());
        assert_eq!(
            fs::metadata(&unsafe_path)
                .expect("unsafe child remains")
                .permissions()
                .mode()
                & 0o777,
            before,
            "refusal must not chmod an existing directory"
        );

        let target = root.join("outside");
        fs::create_dir(&target).expect("create symlink target");
        symlink(&target, root.join("workspace/link")).expect("create child symlink");
        assert!(workspace.open_or_create_private_dir("link").is_err());
        assert!(
            !target.join("new").exists(),
            "refused symlink child must have no target side effect"
        );
        drop(workspace);
        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn owned_workspace_rejects_a_foreign_owned_existing_child() {
        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }
        let root = scratch();
        let parent = DirectoryCapability::open_or_create_private(&root)
            .expect("make workspace fixture private");
        let foreign = root.join("foreign");
        fs::create_dir(&foreign).expect("create foreign-owner fixture");
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700))
            .expect("make foreign directory mode private before ownership change");
        let status = Command::new("chown")
            .arg("65534")
            .arg(&foreign)
            .status()
            .expect("run chown for foreign-owner fixture");
        assert!(status.success(), "assign fixture to the nobody account");

        let before = fs::metadata(&foreign).expect("foreign owner metadata");
        assert_ne!(before.uid(), rustix::process::geteuid().as_raw());
        assert!(OwnedWorkspaceDirectory::open(&foreign).is_err());
        let after = fs::metadata(&foreign).expect("foreign directory remains");
        assert_eq!(after.uid(), before.uid(), "refusal must not change ownership");
        assert_eq!(
            after.permissions().mode() & 0o777,
            before.permissions().mode() & 0o777,
            "refusal must not repair foreign permissions"
        );
        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
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

    #[test]
    fn bounded_link_target_reads_raw_text_without_following_or_truncating() {
        let root = scratch();
        symlink("missing-target", root.join("dangling-link")).expect("create dangling link");
        fs::write(root.join("ordinary-file"), b"ordinary bytes").expect("create regular file");
        let capability = DirectoryCapability::open_read_only_source(&root).expect("pin root");

        assert_eq!(
            capability
                .read_link_target("dangling-link", 32)
                .expect("read raw link target"),
            PathBuf::from("missing-target")
        );
        assert!(
            capability.read_link_target("dangling-link", 4).is_err(),
            "an overlong target must be refused rather than truncated"
        );
        assert!(
            capability.read_link_target("dangling-link", 4097).is_err(),
            "callers cannot raise the capability-wide bound"
        );
        assert!(
            capability.read_link_target("ordinary-file", 32).is_err(),
            "reading a regular file as a link must fail"
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn exclusive_creation_refusal_carries_no_authority_over_existing_child() {
        let root = scratch();
        let existing = root.join("occupied");
        fs::create_dir(&existing).expect("create existing child");
        fs::write(existing.join("payload"), b"preserve me").expect("existing payload");
        let parent = DirectoryCapability::open(&root).expect("pin parent");

        let failure = parent
            .create_private_dir_tracked("occupied")
            .expect_err("exclusive create must refuse the existing child");
        assert!(matches!(
            failure,
            DirectoryCreateFailure::NotCreated(ref error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            fs::read(existing.join("payload")).expect("existing bytes remain"),
            b"preserve me"
        );

        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn postcreate_setup_failure_cleans_only_the_receipted_child() {
        let root = scratch();
        let parent = DirectoryCapability::open(&root).expect("pin parent");
        let result = parent.create_private_dir_with_policy(
            "new-child",
            CreatedDirectoryCleanup::RenameableHandle,
            || Ok(()),
            || Ok(()),
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected setup refusal after pinning the child",
                ))
            },
        );
        let DirectoryCreateFailure::CreatedButUnready { directory, source } =
            result.expect_err("post-create failure must retain its receipt")
        else {
            panic!("post-create failure lost its ownership category");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        directory
            .remove_all(8)
            .expect("receipt removes its pinned child");
        assert!(!root.join("new-child").exists());

        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn mkdir_success_without_a_pinned_child_grants_no_cleanup_authority() {
        let root = scratch();
        let parent = DirectoryCapability::open(&root).expect("pin parent");
        let result = parent.create_private_dir_with_policy(
            "unresolved-child",
            CreatedDirectoryCleanup::RenameableHandle,
            || Ok(()),
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected failure after mkdirat and before openat",
                ))
            },
            || Ok(()),
        );
        let DirectoryCreateFailure::CreatedButUnpinned(source) =
            result.expect_err("the name must not be reopened after injected pin failure")
        else {
            panic!("post-mkdir failure must not claim a pinned creation receipt");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(root.join("unresolved-child").is_dir());
        assert_eq!(
            fs::read_dir(root.join("unresolved-child"))
                .expect("unresolved child remains untouched")
                .count(),
            0
        );

        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn unix_create_receipt_binds_the_private_occupant_observed_at_pin_time() {
        use crate::file_identity::FileIdentity;
        use std::cell::Cell;

        let root = scratch();
        let parent = DirectoryCapability::open(&root).expect("pin parent");
        let original_identity = Cell::new(None);
        let replacement_identity = Cell::new(None);
        let result = parent.create_private_dir_with_policy(
            "child",
            CreatedDirectoryCleanup::RenameableHandle,
            || Ok(()),
            || {
                let original = root.join("child");
                original_identity.set(Some(
                    FileIdentity::of_path_nofollow(&original).expect("original identity"),
                ));
                fs::rename(&original, root.join("moved-original"))
                    .expect("move mkdirat result before openat");
                let replacement = parent
                    .create_private_dir("child")
                    .expect("install private replacement");
                replacement_identity.set(Some(
                    FileIdentity::of_file(replacement.handle.as_ref())
                        .expect("replacement identity"),
                ));
                drop(replacement);
                Ok(())
            },
            || Ok(()),
        );
        let created = result.expect("openat pins the occupant currently at the direct name");
        let pinned_identity = FileIdentity::of_file(created.child.handle.as_ref())
            .expect("receipt child identity");
        assert_ne!(
            original_identity.get().expect("record original identity"),
            pinned_identity,
            "Unix mkdirat returns no handle, so this receipt cannot prove inode continuity"
        );
        assert_eq!(
            replacement_identity.get().expect("record replacement identity"),
            pinned_identity,
            "the receipt identifies the private child observed by openat"
        );
        created
            .remove_all(8)
            .expect("receipt cleans the child it actually pinned");
        drop(created);
        drop(parent);
        fs::remove_dir_all(root.join("moved-original")).expect("remove moved original");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn created_child_receipt_refuses_a_replacement_and_keeps_its_bytes() {
        let root = scratch();
        let parent = DirectoryCapability::open(&root).expect("pin parent");
        let created = parent
            .create_private_dir_tracked("child")
            .expect("create tracked child");
        fs::rename(root.join("child"), root.join("original-created-object"))
            .expect("move original child aside");
        let replacement = parent
            .create_private_dir("child")
            .expect("install private replacement directory");
        fs::write(root.join("child/payload"), b"replacement bytes")
            .expect("replacement payload");
        drop(replacement);

        let failure = created
            .remove_all(8)
            .expect_err("receipt must reject the replacement at its original name");
        assert_eq!(failure.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            fs::read(root.join("child/payload")).expect("replacement is untouched"),
            b"replacement bytes"
        );

        drop(created);
        drop(parent);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn created_child_cleanup_stays_with_a_renamed_parent_capability() {
        let root = scratch();
        let moved = root.with_file_name(format!(
            "{}-moved",
            root.file_name()
                .and_then(|name| name.to_str())
                .expect("scratch name")
        ));
        let parent = DirectoryCapability::open(&root).expect("pin parent");
        let created = parent
            .create_private_dir_tracked("child")
            .expect("create tracked child");
        fs::rename(&root, &moved).expect("rename pinned parent directory");
        fs::create_dir(&root).expect("install a replacement at the old parent path");
        let replacement_parent =
            DirectoryCapability::open(&root).expect("pin replacement parent");
        let replacement = replacement_parent
            .create_private_dir("child")
            .expect("install a private replacement child");
        fs::write(root.join("child/payload"), b"replacement parent bytes")
            .expect("replacement payload");
        drop(replacement);
        drop(replacement_parent);

        created
            .remove_all(8)
            .expect("receipt remains relative to the pinned original parent");
        assert!(!moved.join("child").exists());
        assert_eq!(
            fs::read(root.join("child/payload")).expect("old-path replacement survives"),
            b"replacement parent bytes"
        );

        drop(created);
        drop(parent);
        fs::remove_dir_all(root).expect("remove replacement fixture");
        fs::remove_dir_all(moved).expect("remove moved original parent");
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
    fn simultaneous_private_writable_opens_share_one_file_in_a_pinned_directory() {
        use super::FileIdentity;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        use std::sync::{Arc, Barrier};

        const WORKERS: usize = 24;
        let root = scratch();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("make private directory");
        let directory = DirectoryCapability::open(&root).expect("pin private directory");
        let barrier = Arc::new(Barrier::new(WORKERS));
        let workers = (0..WORKERS)
            .map(|_| {
                let directory = directory.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let file = directory.open_private_file_read_write("lease.lock", true)?;
                    file.lock()?;
                    let metadata = file.metadata()?;
                    Ok::<_, std::io::Error>((
                        FileIdentity::of_file(&file)?,
                        metadata.uid(),
                        metadata.mode(),
                        metadata.nlink(),
                    ))
                })
            })
            .collect::<Vec<_>>();
        let opened = workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .expect("private file opener thread")
                    .expect("open or admit the shared private file")
            })
            .collect::<Vec<_>>();

        let identity = opened[0].0;
        assert!(
            opened.iter().all(|entry| entry.0 == identity),
            "every descriptor must retain the same file identity"
        );
        let named = root.join("lease.lock");
        assert_eq!(
            FileIdentity::of_path_nofollow(&named).expect("named file identity"),
            identity,
            "the admitted descriptors must still refer to the named file"
        );
        let metadata = fs::symlink_metadata(&named).expect("named file metadata");
        assert!(metadata.file_type().is_file(), "named child must be regular");
        assert_eq!(
            metadata.uid(),
            rustix::process::geteuid().as_raw(),
            "named file must remain owned by the current user"
        );
        assert!(
            opened.iter().all(|entry| entry.1 == metadata.uid()),
            "every descriptor must retain current-owner identity"
        );
        assert_eq!(metadata.mode() & 0o777, 0o600, "file must remain private");
        assert_eq!(metadata.nlink(), 1, "file must retain one name");

        drop(directory);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn private_writable_open_refuses_unsafe_existing_children_unchanged() {
        use super::FileIdentity;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let root = scratch();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("make private directory");
        let directory = DirectoryCapability::open(&root).expect("pin private directory");

        let target = root.join("target");
        fs::write(&target, b"keep target bytes").expect("write safe target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("protect safe target");
        let target_identity = FileIdentity::of_path_nofollow(&target).expect("target identity");
        let link = root.join("link");
        symlink(&target, &link).expect("create symlink");
        let link_identity = FileIdentity::of_path_nofollow(&link).expect("link identity");
        assert!(
            directory
                .open_private_file_read_write("link", true)
                .is_err(),
            "a symlink must not be followed or admitted"
        );
        assert!(
            fs::symlink_metadata(&link)
                .expect("link remains present")
                .file_type()
                .is_symlink(),
            "refusal must leave the symlink in place"
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&link).expect("link remains the same object"),
            link_identity
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&target).expect("target remains the same object"),
            target_identity
        );
        assert_eq!(fs::read(&target).expect("read target"), b"keep target bytes");

        let hard = root.join("hard");
        fs::write(&hard, b"keep hard-linked bytes").expect("write hard-linked file");
        fs::set_permissions(&hard, fs::Permissions::from_mode(0o600))
            .expect("protect hard-linked file");
        let alias = root.join("hard-alias");
        fs::hard_link(&hard, &alias).expect("create hard link");
        let hard_identity = FileIdentity::of_path_nofollow(&hard).expect("hard file identity");
        let alias_identity = FileIdentity::of_path_nofollow(&alias).expect("alias identity");
        let hard_error = directory
            .open_private_file_read_write("hard", true)
            .expect_err("multi-link file must be refused");
        assert_eq!(hard_error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            FileIdentity::of_path_nofollow(&hard).expect("hard file remains"),
            hard_identity
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&alias).expect("alias remains"),
            alias_identity
        );
        assert_eq!(
            fs::symlink_metadata(&hard).expect("hard metadata").nlink(),
            2,
            "refusal must not unlink either name"
        );
        assert_eq!(fs::read(&hard).expect("read hard file"), b"keep hard-linked bytes");

        let loose = root.join("loose");
        fs::write(&loose, b"keep loose-mode bytes").expect("write loosely protected file");
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o644))
            .expect("set deliberately broad mode");
        let loose_identity = FileIdentity::of_path_nofollow(&loose).expect("loose identity");
        let loose_error = directory
            .open_private_file_read_write("loose", true)
            .expect_err("group-readable file must be refused");
        assert_eq!(loose_error.kind(), std::io::ErrorKind::PermissionDenied);
        let loose_metadata = fs::symlink_metadata(&loose).expect("loose metadata");
        assert_eq!(loose_metadata.mode() & 0o777, 0o644, "refusal must not chmod");
        assert_eq!(
            FileIdentity::of_path_nofollow(&loose).expect("loose file remains"),
            loose_identity
        );
        assert_eq!(fs::read(&loose).expect("read loose file"), b"keep loose-mode bytes");

        let non_file = root.join("directory");
        fs::create_dir(&non_file).expect("create non-file child");
        let directory_identity = FileIdentity::of_path_nofollow(&non_file)
            .expect("non-file child identity");
        assert!(
            directory
                .open_private_file_read_write("directory", true)
                .is_err(),
            "a directory must not be admitted as a file"
        );
        assert_eq!(
            FileIdentity::of_path_nofollow(&non_file).expect("directory remains"),
            directory_identity
        );
        assert!(non_file.is_dir(), "refusal must preserve the directory");

        drop(directory);
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

    #[cfg(unix)]
    #[test]
    fn source_capability_reads_checkout_without_granting_mutation() {
        use std::io::Read as _;

        let root = scratch();
        fs::write(root.join("README.md"), b"ordinary source tree")
            .expect("write ordinary source file");
        let source =
            DirectoryCapability::open_read_only_source(&root).expect("open ordinary source tree");
        assert_eq!(source.entries(4).expect("list source tree").len(), 1);
        let mut readme = source
            .open_file_read("README.md")
            .expect("read source file");
        let mut bytes = Vec::new();
        readme.read_to_end(&mut bytes).expect("read bytes");
        assert_eq!(bytes, b"ordinary source tree");
        assert_eq!(
            source
                .create_file_exclusive("forbidden")
                .expect_err("source capability is read-only")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(source.validate_private().is_err());
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_entries_use_independent_handles_for_large_listings() {
        use std::sync::{Arc, Barrier};

        const FILES: usize = 1_800;
        let root = scratch();
        for index in 0..FILES {
            let name = format!("entry-{index:05}-{}", "x".repeat(64));
            fs::write(root.join(name), b"x").expect("create listing member");
        }
        let capability = DirectoryCapability::open(&root).expect("pin large listing");
        let left = capability.clone();
        let right = capability;
        let barrier = Arc::new(Barrier::new(3));
        let left_barrier = Arc::clone(&barrier);
        let right_barrier = Arc::clone(&barrier);
        let left = std::thread::spawn(move || {
            left_barrier.wait();
            left.entries(FILES + 1).expect("left directory listing")
        });
        let right = std::thread::spawn(move || {
            right_barrier.wait();
            right.entries(FILES + 1).expect("right directory listing")
        });
        barrier.wait();
        let left = left.join().expect("join left listing");
        let right = right.join().expect("join right listing");
        assert_eq!(left, right, "clone cursors must not split the listing");
        assert_eq!(left.len(), FILES);
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn rename_outcome_distinguishes_post_commit_directory_flush_failure() {
        use super::{DirectoryRenameError, rename_then_sync};

        let root = scratch();
        let source = root.join("staged");
        let destination = root.join("selected");
        fs::write(&source, b"new state").expect("write staged state");
        let outcome = rename_then_sync(
            || fs::rename(&source, &destination),
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected directory flush failure after rename",
                ))
            },
        );
        let Err(DirectoryRenameError::CommittedButNotDurable(error)) = outcome else {
            panic!("post-commit flush failure lost its typed commit state: {outcome:?}");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!source.exists(), "rename source remains after commit");
        assert_eq!(
            fs::read(destination).expect("read committed destination"),
            b"new state"
        );

        let old = root.join("old");
        let new = root.join("new");
        fs::write(&old, b"selected old").expect("write selected state");
        let outcome = rename_then_sync(
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "injected pre-commit rename failure",
                ))
            },
            || Ok(()),
        );
        assert!(matches!(
            outcome,
            Err(DirectoryRenameError::NotCommitted(_))
        ));
        assert_eq!(
            fs::read(old).expect("read old selected state"),
            b"selected old"
        );
        assert!(!new.exists(), "pre-commit failure created a destination");
        fs::remove_dir_all(root).expect("remove fixture");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{CreatedDirectoryCleanup, DirectoryCapability, DirectoryCreateFailure};
    use std::fs;
    use std::io::Read as _;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(label: &str) -> PathBuf {
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-platform-source-{label}-{}-{tick}",
            std::process::id()
        ))
    }

    fn private_parent(label: &str) -> (PathBuf, DirectoryCapability) {
        let path = scratch(label);
        crate::win32::workspace_fs::WorkspaceRoot::create(&path)
            .expect("create private test parent");
        let directory = DirectoryCapability::open(&path).expect("open private test parent");
        (path, directory)
    }

    #[test]
    fn ordinary_inherited_source_acl_is_readable_without_private_state_admission() {
        let root = scratch("ordinary-acl");
        fs::create_dir(&root).expect("create source tree with inherited temp ACL");
        fs::write(root.join("README.md"), b"ordinary source tree")
            .expect("write inherited-ACL source file");
        assert!(
            DirectoryCapability::open(&root).is_err(),
            "fixture must not accidentally be a private persisted-state root"
        );
        let source = DirectoryCapability::open_read_only_source(&root)
            .expect("open ordinary inherited source ACL");
        assert_eq!(source.entries(4).expect("list source").len(), 1);
        let mut readme = source
            .open_file_read("README.md")
            .expect("read source under inherited ACL");
        let mut bytes = Vec::new();
        readme.read_to_end(&mut bytes).expect("read source bytes");
        assert_eq!(bytes, b"ordinary source tree");
        assert!(source.open_private_file("README.md").is_err());
        assert!(source.create_file_exclusive("forbidden").is_err());
        drop(source);
        fs::remove_dir_all(root).expect("remove source fixture");
    }

    #[test]
    fn tracked_creation_refusal_preserves_existing_child_bytes() {
        let (path, parent) = private_parent("tracked-collision");
        let existing = parent
            .create_private_dir("occupied")
            .expect("create existing child");
        fs::write(path.join("occupied/payload"), b"preserve me")
            .expect("existing payload");
        drop(existing);

        let failure = parent
            .create_private_dir_tracked("occupied")
            .expect_err("exclusive create refuses the existing child");
        assert!(matches!(
            failure,
            DirectoryCreateFailure::NotCreated(ref error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(
            fs::read(path.join("occupied/payload")).expect("existing bytes remain"),
            b"preserve me"
        );

        drop(parent);
        fs::remove_dir_all(path).expect("remove fixture");
    }

    #[test]
    fn postcreate_setup_failure_uses_the_held_created_handle_for_cleanup() {
        let (path, parent) = private_parent("tracked-setup-failure");
        let result = parent.create_private_dir_with_policy(
            "new-child",
            CreatedDirectoryCleanup::RenameableHandle,
            || Ok(()),
            || Ok(()),
            || {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected post-create setup refusal",
                ))
            },
        );
        let DirectoryCreateFailure::CreatedButUnready { directory, source } =
            result.expect_err("post-create failure retains the exclusive-create handle")
        else {
            panic!("post-create failure lost its ownership category");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        directory
            .remove_all(8)
            .expect("receipt removes the held created child");
        assert!(!path.join("new-child").exists());

        drop(parent);
        fs::remove_dir_all(path).expect("remove fixture");
    }

    #[test]
    fn created_handle_refuses_a_replacement_and_leaves_it_untouched() {
        let (path, parent) = private_parent("tracked-replacement");
        let created = parent
            .create_private_dir_tracked("child")
            .expect("create tracked child");
        fs::rename(path.join("child"), path.join("original-created-object"))
            .expect("move original child aside");
        let replacement = parent
            .create_private_dir("child")
            .expect("create replacement at the same name");
        fs::write(path.join("child/payload"), b"replacement bytes")
            .expect("replacement payload");
        drop(replacement);

        let failure = created
            .remove_all(8)
            .expect_err("receipt must reject a different file identity");
        assert_eq!(failure.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(path.join("child/payload")).expect("replacement remains untouched"),
            b"replacement bytes"
        );

        drop(created);
        drop(parent);
        fs::remove_dir_all(path).expect("remove fixture");
    }

    #[test]
    fn concurrent_entries_use_independent_handles_for_large_listings() {
        const FILES: usize = 1_300;
        let root = scratch("large-listing");
        fs::create_dir(&root).expect("create listing fixture with inherited temp ACL");
        for index in 0..FILES {
            let name = format!("entry-{index:05}-{}", "x".repeat(64));
            fs::write(root.join(name), b"x").expect("create listing member");
        }
        let capability = DirectoryCapability::open_read_only_source(&root)
            .expect("open ordinary-ACL listing root");
        let left = capability.clone();
        let right = capability;
        let barrier = Arc::new(Barrier::new(3));
        let left_barrier = Arc::clone(&barrier);
        let right_barrier = Arc::clone(&barrier);
        let left = std::thread::spawn(move || {
            left_barrier.wait();
            left.entries(FILES + 1).expect("left directory listing")
        });
        let right = std::thread::spawn(move || {
            right_barrier.wait();
            right.entries(FILES + 1).expect("right directory listing")
        });
        barrier.wait();
        let left = left.join().expect("join left listing");
        let right = right.join().expect("join right listing");
        assert_eq!(left, right, "clone cursors must not split the listing");
        assert_eq!(left.len(), FILES);
        drop(left);
        drop(right);
        fs::remove_dir_all(root).expect("remove listing fixture");
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

#[cfg(unix)]
fn rename_then_sync(
    rename: impl FnOnce() -> io::Result<()>,
    sync: impl FnOnce() -> io::Result<()>,
) -> Result<(), DirectoryRenameError> {
    rename().map_err(DirectoryRenameError::NotCommitted)?;
    sync().map_err(DirectoryRenameError::CommittedButNotDurable)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "directory capabilities are unavailable on this platform",
    )
}
