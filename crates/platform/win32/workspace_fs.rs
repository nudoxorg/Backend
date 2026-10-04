//! Handle-relative, reparse-safe operations for private worker workspaces.
//!
//! Every name passed to this module is one validated UTF-8 path component.
//! Directory traversal and mutation use NT handles with `RootDirectory`, so
//! the kernel resolves each name beneath an already-open directory object.
//!
//! # Names are resolved, not canonicalized
//! The kernel matches names case-insensitively and, on a volume that generates
//! 8.3 aliases, by short name: `LONGFI~1.TXT` opens `longfilename_sample.txt`.
//! Enumeration reports only the long name. A caller that decides anything from
//! a name (an ignore rule, a collision check) must compare the entry's
//! identity or the names an enumeration returned, never the string it was
//! handed.
#![allow(
    unsafe_code,
    reason = "reviewed NT relative-open and handle metadata boundary for Windows workspace files"
)]

use super::busy::{is_busy, retry_while_busy, retry_while_busy_pausing};
use super::file::Replacement;
use super::identity::{current_user, is_owned_by_current_user, owner_of};
use super::security::restrict_handle_to_current_user;
use crate::directory::DirectoryRenameError;
use crate::linkage::{Attempt, IfUnlinked, Linkage, open_admitted, reopen_while_replaced};
use crate::retry::retry_when;
use std::cell::RefCell;
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::offset_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle as _, IntoRawHandle as _, OwnedHandle};
use std::path::{Component, Path, Prefix};
use std::ptr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GENERIC_ALL, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    GetAce, GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
    PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR_CONTROL,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_GENERIC_WRITE, FILE_ID_EXTD_DIR_INFO,
    FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_STANDARD_INFO, FILE_TRAVERSE, FILE_WRITE_DATA, FileAttributeTagInfo,
    FileDispositionInfo, FileDispositionInfoEx, FileIdExtdDirectoryInfo,
    FileIdExtdDirectoryRestartInfo, FileIdInfo, FileStandardInfo, FlushFileBuffers,
    GetFileInformationByHandleEx, READ_CONTROL, SYNCHRONIZE, SetFileInformationByHandle, WRITE_DAC,
};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

const STATUS_SUCCESS: i32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const FILE_SHARE_DELETE: u32 = 4;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

/// `FILE_INFORMATION_CLASS::FileRenameInformation`: the classic rename, which
/// takes a `BOOLEAN ReplaceIfExists` in the first byte of its header.
const FILE_RENAME_INFORMATION_CLASS: i32 = 10;
/// `FILE_INFORMATION_CLASS::FileRenameInformationEx`: the same header with a
/// `ULONG Flags`, available from Windows 10 1607.
const FILE_RENAME_INFORMATION_EX_CLASS: i32 = 65;
/// `FILE_RENAME_INFORMATION_EX::Flags`: replace an existing target.
const FILE_RENAME_REPLACE_IF_EXISTS: u32 = 0x1;
/// `FILE_RENAME_INFORMATION_EX::Flags`: unlink the target name at once, like
/// POSIX `rename`, even while another handle shares delete access to it.
const FILE_RENAME_POSIX_SEMANTICS: u32 = 0x2;
/// `FILE_RENAME_INFORMATION_EX::Flags`: replace a target that carries the
/// read-only attribute (Windows 10 1809 and later). POSIX `rename` consults
/// the directory's permissions and never the replaced file's own mode, and
/// `remove_file_relative` already ignores the attribute the same way.
const FILE_RENAME_IGNORE_READONLY_ATTRIBUTE: u32 = 0x40;

/// How a handle constrains every other opener of the same object.
///
/// The share mode is the whole of Windows' pinning story: a handle that does
/// not share delete access stops any other handle from renaming or deleting
/// the object, and the NT file systems also refuse to rename a directory while
/// any descendant is open. That is what keeps a held directory in its place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sharing {
    /// Others may read and write but may not delete or rename the object for
    /// as long as this handle lives. Directory handles that anchor a
    /// traversal, and files being written, are opened this way.
    Pinned,
    /// Others may do anything. Short-lived probes and mutation handles are
    /// opened this way so they never fail because someone else holds the same
    /// object open, and never keep anyone else from replacing it.
    Transient,
}

impl Sharing {
    const fn bits(self) -> u32 {
        match self {
            Self::Pinned => FILE_SHARE_READ | FILE_SHARE_WRITE,
            Self::Transient => FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        }
    }
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_qos: *mut c_void,
}

#[repr(C)]
union IoStatusValue {
    status: i32,
    pointer: *mut c_void,
}

#[repr(C)]
struct IoStatusBlock {
    value: IoStatusValue,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *mut i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *mut c_void,
        ea_length: u32,
    ) -> i32;
    fn NtSetInformationFile(
        file_handle: HANDLE,
        io_status_block: *mut IoStatusBlock,
        file_information: *const c_void,
        length: u32,
        file_information_class: i32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
    /// Returns nonzero when Win32 path normalization on the running system
    /// routes the single component to a DOS device.
    fn RtlIsDosDeviceName_U(name: *const u16) -> u32;
}

/// The header common to `FILE_RENAME_INFORMATION` and its `Ex` form. The
/// UTF-16 target name follows it in the same allocation.
#[repr(C)]
struct RenameInformation {
    /// `ReplaceIfExists` (a `BOOLEAN`) for the classic class, `Flags` for `Ex`.
    flags: u32,
    /// Directory the name is relative to; the kernel resolves the name there.
    root_directory: HANDLE,
    /// Byte length of the name that follows, without a terminator.
    file_name_length: u32,
    /// First code unit of the name.
    file_name: [u16; 1],
}

#[repr(C)]
struct FileAttributeTagInfo {
    file_attributes: u32,
    reparse_tag: u32,
}

struct DirectoryNode {
    handle: OwnedHandle,
    _parent: Option<Arc<DirectoryNode>>,
    name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspacePurpose {
    PrivateState,
    ReadOnlySource,
}

/// A pinned private directory. Its ancestor handles stay open for its lifetime,
/// preventing a concurrent rename from moving a descendant out of the root.
#[derive(Clone)]
pub struct WorkspaceRoot(Arc<DirectoryNode>);

/// Creation outcome for one exclusive direct child. `NotCreated` carries no
/// cleanup authority; `Created` retains the exact handle returned by the
/// successful `FILE_CREATE` operation when post-create setup fails.
pub enum WorkspaceDirectoryCreateFailure {
    /// The exclusive create did not create a directory.
    NotCreated(io::Error),
    /// The directory was created, but setup failed after its handle was pinned.
    Created {
        /// The exact newly created object, retained by its handle.
        directory: WorkspaceRoot,
        /// The post-create setup failure.
        source: io::Error,
    },
}

#[derive(Debug)]
struct WorkspaceCreateRollbackFailure {
    create: io::Error,
    cleanup: io::Error,
}

impl std::fmt::Display for WorkspaceCreateRollbackFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "workspace child setup failed ({}), and exact created-child rollback failed ({})",
            self.create, self.cleanup
        )
    }
}

impl std::error::Error for WorkspaceCreateRollbackFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.create)
    }
}

impl std::fmt::Debug for WorkspaceDirectoryCreateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCreated(source) => formatter
                .debug_tuple("NotCreated")
                .field(source)
                .finish(),
            Self::Created { source, .. } => formatter
                .debug_struct("Created")
                .field("source", source)
                .finish_non_exhaustive(),
        }
    }
}

/// The kind of a checked direct child returned by [`WorkspaceRoot::read_dir_checked`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A regular file with one link and a private DACL.
    File,
    /// A directory with a private DACL.
    Directory,
}

/// A checked child name and its file kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirEntry {
    /// The child's single UTF-8 name component.
    pub name: String,
    /// The kind checked through an open handle.
    pub kind: EntryKind,
}

impl WorkspaceRoot {
    /// Opens an existing absolute local-drive directory, walking from its drive
    /// root without following any reparse point. Relative paths, UNC paths,
    /// device namespaces, and `.`/`..` components are rejected.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_purpose(path, WorkspacePurpose::PrivateState)
    }

    /// Opens an ordinary read-only source tree without requiring its inherited
    /// DACL to be private. Reparse points and malformed directory handles are
    /// still refused, and all descendants stay relative to held handles.
    pub fn open_read_only_source(path: &Path) -> io::Result<Self> {
        Self::open_with_purpose(path, WorkspacePurpose::ReadOnlySource)
    }

    fn open_with_purpose(path: &Path, purpose: WorkspacePurpose) -> io::Result<Self> {
        let (drive_root, parts) = absolute_drive_components(path)?;
        let mut current = if purpose == WorkspacePurpose::ReadOnlySource {
            open_drive_root_source(&drive_root)?
        } else {
            open_drive_root(&drive_root)?
        };
        if parts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root cannot be the drive root",
            ));
        }
        for part in &parts[..parts.len() - 1] {
            current = open_directory_child_unchecked(&current, ExistingName::parse(part)?)?;
        }
        let final_part = parts.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root cannot be the drive root",
            )
        })?;
        let final_part = ExistingName::parse(final_part)?;
        current = if purpose == WorkspacePurpose::ReadOnlySource {
            open_directory_child_unchecked(&current, final_part)?
        } else {
            open_directory_child_writable(&current, final_part)?
        };
        if purpose == WorkspacePurpose::PrivateState {
            ensure_private_handle(current.handle.as_raw_handle())?;
        }
        Ok(Self(current))
    }

    /// Creates the exact final directory exclusively beneath a safely opened
    /// parent and returns its pinned handle. The parent must already exist.
    pub fn create(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace directory name is not UTF-8",
                )
            })?;
        let name = NewName::parse(name)?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace path has no parent directory",
            )
        })?;
        let (drive_root, parts) = absolute_drive_components(parent)?;
        let parent = if parts.is_empty() {
            open_drive_root_writable(&drive_root)?
        } else {
            let mut current = open_drive_root(&drive_root)?;
            for part in &parts[..parts.len() - 1] {
                current = open_directory_child_unchecked(&current, ExistingName::parse(part)?)?;
            }
            let last = parts.last().ok_or_else(invalid_name)?;
            open_directory_child_writable(&current, ExistingName::parse(last)?)?
        };
        let parent = Self(parent);
        parent.create_child_dir_exclusive(name.as_str())
    }

    /// Creates or verifies one owner-only directory beneath an existing
    /// current-user-owned application-data parent. Unlike `open`, this allows
    /// the parent to inherit the operating system's ordinary application-data
    /// ACL; the final child is still opened or created with the strict private
    /// workspace DACL. Traversal is handle-relative and rejects reparse points.
    pub fn ensure_private_child_directory(path: &Path) -> io::Result<()> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "application data directory name is not UTF-8",
                )
            })?;
        let name = NewName::parse(name)?;
        let parent_path = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "application data directory has no parent",
            )
        })?;
        let (drive_root, parts) = absolute_drive_components(parent_path)?;
        if parts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "application data parent cannot be a drive root",
            ));
        }
        let mut current = open_drive_root(&drive_root)?;
        for part in &parts[..parts.len() - 1] {
            current = open_directory_child_unchecked(&current, ExistingName::parse(part)?)?;
        }
        let last = parts.last().ok_or_else(invalid_name)?;
        let parent = open_directory_child_writable(&current, ExistingName::parse(last)?)?;
        if !is_owned_by_current_user(&owner_of(&HandleRef(parent.handle.as_raw_handle()))?)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "application data parent has a foreign owner",
            ));
        }

        let parent = Self(parent);
        let open_existing = || open_directory_child(&parent.0, name.existing()).map(Self);
        match open_existing() {
            Ok(child) => {
                child.flush_dir()?;
                parent.flush_dir()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match parent.create_child_dir_exclusive(name.as_str()) {
                    Ok(child) => {
                        child.flush_dir()?;
                        parent.flush_dir()
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let child = open_existing()?;
                        child.flush_dir()?;
                        parent.flush_dir()
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Creates one direct child directory exclusively and applies the current
    /// user's protected DACL through the created handle before returning it.
    pub fn create_child_dir_exclusive(&self, name: &str) -> io::Result<Self> {
        match self.create_child_dir_exclusive_with_sharing(name, Sharing::Pinned, || Ok(())) {
            Ok(directory) => Ok(directory),
            Err(WorkspaceDirectoryCreateFailure::NotCreated(source)) => Err(source),
            Err(WorkspaceDirectoryCreateFailure::Created { directory, source }) => {
                match directory.remove_created_empty_self() {
                    Ok(()) => Err(source),
                    Err(cleanup) => Err(io::Error::other(WorkspaceCreateRollbackFailure {
                        create: source,
                        cleanup,
                    })),
                }
            }
        }
    }

    /// Creates a direct child while retaining the exact handle in any
    /// post-create failure. `renameable` uses a share mode that allows the
    /// publisher's later atomic rename. After setup succeeds, cleanup reopens
    /// the direct name with delete-pinning and compares file IDs; if setup
    /// itself fails, the receipt can delete only the still-empty object through
    /// the exact handle returned by `FILE_CREATE`.
    pub(crate) fn create_child_dir_exclusive_tracked(
        &self,
        name: &str,
        renameable: bool,
    ) -> Result<Self, WorkspaceDirectoryCreateFailure> {
        self.create_child_dir_exclusive_with_sharing(
            name,
            if renameable {
                Sharing::Transient
            } else {
                Sharing::Pinned
            },
            || Ok(()),
        )
    }

    fn create_child_dir_exclusive_with_sharing(
        &self,
        name: &str,
        sharing: Sharing,
        after_create: impl FnOnce() -> io::Result<()>,
    ) -> Result<Self, WorkspaceDirectoryCreateFailure> {
        let name = NewName::parse(name).map_err(WorkspaceDirectoryCreateFailure::NotCreated)?;
        let mut access = FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_GENERIC_WRITE
            | FILE_LIST_DIRECTORY
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | READ_CONTROL
            | WRITE_DAC
            | SYNCHRONIZE;
        // The initial FILE_CREATE handle always carries DELETE rights so any
        // setup failure can roll back only that still-empty object without
        // reopening an untrusted name. A successful renameable stage swaps to
        // a separate operation handle without DELETE rights below.
        access |= FILE_DELETE_CHILD | DELETE;
        let handle = create_relative(
            self.handle(),
            name,
            access,
            FILE_DIRECTORY_FILE,
            sharing,
        )
        .map_err(WorkspaceDirectoryCreateFailure::NotCreated)?;
        let directory = Self(Arc::new(DirectoryNode {
            handle,
            _parent: Some(Arc::clone(&self.0)),
            name: Some(name.as_str().to_owned()),
        }));
        if let Err(source) = after_create()
            .and_then(|()| restrict_handle_to_current_user(directory.handle()))
            .and_then(|()| ensure_directory_handle(directory.handle()))
            .and_then(|()| ensure_private_handle(directory.handle()))
        {
            return Err(WorkspaceDirectoryCreateFailure::Created { directory, source });
        }
        if sharing == Sharing::Pinned {
            // Compatibility callers keep the original delete-capable pinned
            // handle, preserving their previous rename exclusion and exact
            // rollback behavior. Only renameable publication stages need a
            // second, no-DELETE operation handle.
            return Ok(directory);
        }
        // Keep the long-lived renameable receipt handle free of DELETE access.
        // The later cleanup open can then request delete-pinning, compare the
        // exact file id, and hold the name stable while removing that object.
        let operational = match open_directory_child_writable_with_sharing(
            &self.0,
            name.existing(),
            sharing,
            false,
        ) {
            Ok(operational) => operational,
            Err(source) => {
                return Err(WorkspaceDirectoryCreateFailure::Created { directory, source });
            }
        };
        if let Err(source) = ensure_private_handle(operational.handle.as_raw_handle()) {
            return Err(WorkspaceDirectoryCreateFailure::Created { directory, source });
        }
        let same_object = match same_directory_object(
            operational.handle.as_raw_handle(),
            directory.handle(),
        ) {
            Ok(same_object) => same_object,
            Err(source) => {
                return Err(WorkspaceDirectoryCreateFailure::Created { directory, source });
            }
        };
        if !same_object {
            return Err(WorkspaceDirectoryCreateFailure::Created {
                directory,
                source: invalid_data(
                    "created child name changed before its operation handle opened",
                ),
            });
        }
        drop(directory);
        Ok(Self(operational))
    }

    /// Deletes the exact object created by a renameable receipt if the direct
    /// name still refers to it. The reopened handle pins its name for the full
    /// tree removal, so a replacement is neither traversed nor deleted.
    pub(crate) fn remove_created_child_if_same(
        &self,
        name: &str,
        expected: &Self,
        maximum_entries: usize,
    ) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(&[name])?;
        let expected_parent = expected
            .0
            ._parent
            .as_ref()
            .ok_or_else(|| invalid_data("created child has no pinned parent"))?;
        if !Arc::ptr_eq(parent, expected_parent)
            || expected.0.name.as_deref() != Some(name)
        {
            return Err(invalid_data("created child receipt belongs to another parent or name"));
        }
        let current = open_directory_child_for_created_delete(parent, leaf)?;
        if !same_directory_object(current.handle.as_raw_handle(), expected.handle())? {
            return Err(invalid_data("created directory name now refers to another object"));
        }
        let mut visited = 0_usize;
        remove_tree_contents(&current, &mut visited, maximum_entries)?;
        mark_delete(current.handle.as_raw_handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Removes an empty child through its original, delete-capable creation
    /// handle. This path is used only before the caller can add contents. It
    /// deletes the held object itself even if its name has moved; it never
    /// reopens or removes whatever currently occupies the remembered name.
    pub(crate) fn remove_created_empty_self(&self) -> io::Result<()> {
        let parent = self
            .0
            ._parent
            .as_ref()
            .ok_or_else(|| invalid_data("created child has no pinned parent"))?;
        if self.0.name.is_none() {
            return Err(invalid_data("created child has no direct name"));
        }
        let _ = enumerate_names(self.handle(), 0)?;
        mark_delete(self.handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Opens a child directory beneath this pinned root after rejecting
    /// reparses, hard-linked directories, foreign owners, and non-private ACLs.
    pub fn open_dir_checked(&self, path: &[&str]) -> io::Result<Self> {
        let mut current = Arc::clone(&self.0);
        for component in path {
            current = open_directory_child(&current, ExistingName::parse(component)?)?;
        }
        ensure_private_handle(current.handle.as_raw_handle())?;
        Ok(Self(current))
    }

    /// Opens read-only source descendants without imposing a private DACL.
    pub fn open_dir_source_checked(&self, path: &[&str]) -> io::Result<Self> {
        let mut current = Arc::clone(&self.0);
        for component in path {
            current = open_directory_child_unchecked(&current, ExistingName::parse(component)?)?;
        }
        Ok(Self(current))
    }

    /// Creates a new regular file exclusively beneath the pinned root. The
    /// protected current-user DACL is applied before the file handle escapes.
    pub fn create_file_exclusive(&self, path: &[&str]) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = create_relative(
            parent.handle.as_raw_handle().cast(),
            NewName::new(leaf)?,
            FILE_ADD_FILE
                | FILE_READ_ATTRIBUTES
                | FILE_READ_DATA
                | FILE_WRITE_DATA
                | READ_CONTROL
                | WRITE_DAC
                | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
            Sharing::Pinned,
        )?;
        restrict_handle_to_current_user(&handle)?;
        ensure_regular_file_handle(handle.as_raw_handle())?;
        ensure_private_handle(handle.as_raw_handle())?;
        file_from_handle(handle)
    }

    /// Opens a regular file by components and verifies its identity properties
    /// and private DACL on that exact handle. It never repairs an existing ACL.
    pub fn open_file_read_checked(&self, path: &[&str]) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = open_admitted_file(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            IfUnlinked::Keep,
            |handle, linkage| {
                ensure_regular_file_handle_with(handle, linkage)?;
                ensure_private_handle(handle)
            },
        )?;
        file_from_handle(handle)
    }

    /// Opens a regular source file without requiring a private DACL.
    pub fn open_file_read_source_checked(&self, path: &[&str]) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf_source(path)?;
        let handle = open_admitted_file(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            IfUnlinked::Keep,
            ensure_regular_file_handle_with,
        )?;
        file_from_handle(handle)
    }

    /// Opens an existing regular private file for reading and writing, or
    /// creates it exclusively when requested.
    pub fn open_file_read_write_checked(&self, path: &[&str], create: bool) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = match open_admitted_file(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_ATTRIBUTES | FILE_READ_DATA | FILE_WRITE_DATA | READ_CONTROL | SYNCHRONIZE,
            IfUnlinked::Reopen,
            |handle, linkage| {
                ensure_regular_file_handle_with(handle, linkage)?;
                ensure_private_handle(handle)
            },
        ) {
            Ok(handle) => handle,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                return self.create_file_exclusive(path);
            }
            Err(error) => return Err(error),
        };
        file_from_handle(handle)
    }

    /// Atomically renames one checked regular file beneath this root. The
    /// destination's parent is opened component-by-component and the leaf is
    /// resolved by the kernel relative to that handle.
    pub fn rename_relative(
        &self,
        source: &[&str],
        destination: &[&str],
        replace: bool,
    ) -> io::Result<()> {
        self.rename_relative_with_outcome(source, destination, replace)
            .map_err(DirectoryRenameError::into_io_error)
    }

    /// Atomically renames a regular file while preserving whether parent
    /// flushing failed before or after the native rename committed.
    pub fn rename_relative_with_outcome(
        &self,
        source: &[&str],
        destination: &[&str],
        replace: bool,
    ) -> Result<(), DirectoryRenameError> {
        self.rename_checked_entry_with_outcome(source, destination, replace, false)
    }

    /// Atomically renames one checked private directory beneath this root.
    pub fn rename_directory_relative(
        &self,
        source: &[&str],
        destination: &[&str],
    ) -> io::Result<()> {
        self.rename_directory_relative_with_outcome(source, destination)
            .map_err(DirectoryRenameError::into_io_error)
    }

    /// Atomically renames a private directory with an explicit commit result.
    pub fn rename_directory_relative_with_outcome(
        &self,
        source: &[&str],
        destination: &[&str],
    ) -> Result<(), DirectoryRenameError> {
        self.rename_checked_entry_with_outcome(source, destination, false, true)
    }

    /// Returns whether a checked direct child is a directory.
    pub fn child_is_directory(&self, path: &[&str]) -> io::Result<bool> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = open_relative(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            0,
            Sharing::Transient,
        )?;
        let attributes = attributes(handle.as_raw_handle())?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_data("workspace child is a reparse point"));
        }
        let standard = standard_info(handle.as_raw_handle())?;
        if standard.NumberOfLinks != 1 {
            return Err(invalid_data("workspace child has multiple links"));
        }
        ensure_private_handle(handle.as_raw_handle())?;
        Ok(standard.Directory)
    }

    fn rename_checked_entry_with_outcome(
        &self,
        source: &[&str],
        destination: &[&str],
        replace: bool,
        directory: bool,
    ) -> Result<(), DirectoryRenameError> {
        self.rename_checked_entry_with_flush(
            source,
            destination,
            replace,
            directory,
            || {},
            flush_handle,
        )
    }

    /// The rename protocol with two test seams. `before_commit` runs after
    /// every check and immediately before the kernel rename, so a test can
    /// interleave an attacker at the one point that matters; `flush` makes the
    /// post-commit directory flush injectable.
    fn rename_checked_entry_with_flush(
        &self,
        source: &[&str],
        destination: &[&str],
        replace: bool,
        directory: bool,
        before_commit: impl FnOnce(),
        flush: impl FnMut(*mut c_void) -> io::Result<()>,
    ) -> Result<(), DirectoryRenameError> {
        self.rename_checked_entry_pausing(
            source,
            destination,
            replace,
            directory,
            before_commit,
            flush,
            &mut thread::sleep,
        )
        .map(|_| ())
    }

    /// [`Self::rename_checked_entry_with_flush`] with the pause between
    /// retries of a busy or replaced file injected as well. A test releases a
    /// scanner's hold from the pause that follows the refused attempt, so the
    /// wait is proven by ordering rather than by a sleep that a loaded
    /// machine can overrun.
    #[allow(
        clippy::too_many_arguments,
        reason = "the protocol's complete set of test seams"
    )]
    fn rename_checked_entry_pausing(
        &self,
        source: &[&str],
        destination: &[&str],
        replace: bool,
        directory: bool,
        before_commit: impl FnOnce(),
        mut flush: impl FnMut(*mut c_void) -> io::Result<()>,
        pause: &mut dyn FnMut(Duration),
    ) -> Result<Replacement, DirectoryRenameError> {
        let precommit = DirectoryRenameError::NotCommitted;
        let (source_parent, source_leaf) = self.parent_and_leaf(source).map_err(precommit)?;
        let (destination_parent, destination_leaf) = self
            .parent_and_leaf(destination)
            .map_err(DirectoryRenameError::NotCommitted)?;
        let destination_name =
            NewName::new(destination_leaf).map_err(DirectoryRenameError::NotCommitted)?;
        let source_handle = retry_while_busy_pausing(
            || {
                open_relative(
                    source_parent.handle.as_raw_handle().cast(),
                    source_leaf,
                    FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
                    if directory {
                        FILE_DIRECTORY_FILE
                    } else {
                        FILE_NON_DIRECTORY_FILE
                    },
                    Sharing::Transient,
                )
            },
            &mut *pause,
        )
        .map_err(DirectoryRenameError::NotCommitted)?;
        if directory {
            ensure_directory_handle(source_handle.as_raw_handle())
                .map_err(DirectoryRenameError::NotCommitted)?;
        } else {
            ensure_regular_file_handle(source_handle.as_raw_handle())
                .map_err(DirectoryRenameError::NotCommitted)?;
        }
        ensure_private_handle(source_handle.as_raw_handle())
            .map_err(DirectoryRenameError::NotCommitted)?;
        check_replace_destination(
            &destination_parent,
            destination_name.existing(),
            replace,
            &mut *pause,
        )
        .map_err(DirectoryRenameError::NotCommitted)?;
        let name = destination_name.wide();
        before_commit();
        let replacement = rename_into(
            source_handle.as_raw_handle().cast(),
            destination_parent.handle.as_raw_handle().cast(),
            &name,
            replace,
            &mut *pause,
        )
        .map_err(DirectoryRenameError::NotCommitted)?;
        flush(destination_parent.handle.as_raw_handle())
            .map_err(DirectoryRenameError::CommittedButNotDurable)?;
        if !Arc::ptr_eq(&source_parent, &destination_parent) {
            flush(source_parent.handle.as_raw_handle())
                .map_err(DirectoryRenameError::CommittedButNotDurable)?;
        }
        Ok(replacement)
    }

    /// Unlinks one checked regular file by its opened handle, then flushes its
    /// parent directory. A concurrent name replacement cannot redirect it.
    pub fn remove_file_relative(&self, path: &[&str]) -> io::Result<()> {
        self.remove_file_pausing(path, &mut thread::sleep)
    }

    /// [`Self::remove_file_relative`] with the pause between retries of a busy
    /// file injected, so a test can release a scanner's hold deterministically.
    fn remove_file_pausing(
        &self,
        path: &[&str],
        pause: &mut dyn FnMut(Duration),
    ) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = retry_while_busy_pausing(
            || {
                open_relative(
                    parent.handle.as_raw_handle().cast(),
                    leaf,
                    FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
                    FILE_NON_DIRECTORY_FILE,
                    Sharing::Transient,
                )
            },
            pause,
        )?;
        ensure_regular_file_handle(handle.as_raw_handle())?;
        ensure_private_handle(handle.as_raw_handle())?;
        mark_delete(handle.as_raw_handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Checks every direct child through an open, reparse-point handle and
    /// rejects links, foreign owners, special files, and non-private DACLs.
    pub fn read_dir_checked(&self, path: &[&str]) -> io::Result<Vec<DirEntry>> {
        self.read_dir_checked_limited(path, usize::MAX)
    }

    /// Checks direct children through open handles with a cardinality ceiling.
    pub fn read_dir_checked_limited(
        &self,
        path: &[&str],
        maximum: usize,
    ) -> io::Result<Vec<DirEntry>> {
        let directory = self.open_dir_checked(path)?;
        directory.read_open_directory_limited(maximum, WorkspacePurpose::PrivateState)
    }

    /// Lists a source directory without imposing workspace-private ACLs.
    pub fn read_dir_source_checked_limited(
        &self,
        path: &[&str],
        maximum: usize,
    ) -> io::Result<Vec<DirEntry>> {
        let directory = self.open_dir_source_checked(path)?;
        directory.read_open_directory_limited(maximum, WorkspacePurpose::ReadOnlySource)
    }

    fn read_open_directory_limited(
        &self,
        maximum: usize,
        purpose: WorkspacePurpose,
    ) -> io::Result<Vec<DirEntry>> {
        let directory = self.reopen_current_directory(purpose)?;
        let names = enumerate_names(directory.handle(), maximum)?;
        let mut entries = Vec::with_capacity(names.len());
        for EnumeratedEntry { name, id } in names {
            let handle = open_relative(
                directory.handle(),
                ExistingName::parse(&name)?,
                FILE_READ_ATTRIBUTES
                    | SYNCHRONIZE
                    | if purpose == WorkspacePurpose::PrivateState {
                        READ_CONTROL
                    } else {
                        0
                    },
                0,
                Sharing::Transient,
            )?;
            let attributes = attributes(handle.as_raw_handle())?;
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(invalid_data("workspace child is a reparse point"));
            }
            let standard = standard_info(handle.as_raw_handle())?;
            if standard.NumberOfLinks != 1 {
                return Err(invalid_data("workspace child has multiple hard links"));
            }
            if FileId128::of_handle(handle.as_raw_handle())? != id {
                return Err(invalid_data(
                    "workspace entry changed during directory enumeration",
                ));
            }
            if purpose == WorkspacePurpose::PrivateState {
                ensure_private_handle(handle.as_raw_handle())?;
            }
            let kind = if standard.Directory {
                EntryKind::Directory
            } else if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                EntryKind::File
            } else {
                return Err(invalid_data(
                    "workspace child has inconsistent file attributes",
                ));
            };
            entries.push(DirEntry { name, kind });
        }
        Ok(entries)
    }

    fn reopen_current_directory(&self, purpose: WorkspacePurpose) -> io::Result<Self> {
        let parent = self.0._parent.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "independent enumeration handle requires a named directory child",
            )
        })?;
        let name = self.0.name.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "independent enumeration handle has no held child name",
            )
        })?;
        let name = ExistingName::parse(name)?;
        let reopened = if purpose == WorkspacePurpose::PrivateState {
            open_directory_child(parent, name)?
        } else {
            open_directory_child_unchecked(parent, name)?
        };
        let original_id = identity_info(self.0.handle.as_raw_handle())?;
        let reopened_id = identity_info(reopened.handle.as_raw_handle())?;
        if original_id.VolumeSerialNumber != reopened_id.VolumeSerialNumber
            || original_id.FileId.Identifier[..] != reopened_id.FileId.Identifier[..]
        {
            return Err(invalid_data(
                "directory changed before independent enumeration opened",
            ));
        }
        Ok(Self(reopened))
    }

    /// Removes a private directory tree by recursively opening and deleting
    /// each child through pinned directory handles. Any unexpected entry or
    /// race fails closed; the directory itself is removed only when empty.
    pub fn remove_dir_tree(&self, path: &[&str]) -> io::Result<()> {
        self.remove_dir_tree_limited(path, 1_000_000)
    }

    /// Removes a private directory tree under an explicit total-entry ceiling.
    pub fn remove_dir_tree_limited(&self, path: &[&str], maximum_entries: usize) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let directory = match open_directory_child_with_delete(&parent, leaf) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut visited = 0_usize;
        remove_tree_contents(&directory, &mut visited, maximum_entries)?;
        mark_delete(directory.handle.as_raw_handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Removes one empty private child directory without following reparses.
    pub fn remove_empty_dir(&self, path: &[&str]) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let directory = open_directory_child_with_delete(&parent, leaf)?;
        let _ = enumerate_names(directory.handle.as_raw_handle().cast(), 0)?;
        mark_delete(directory.handle.as_raw_handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Flushes this directory handle so callers can require directory metadata
    /// durability. Filesystems that do not support directory flushes return an
    /// error; the limitation is never silently ignored.
    pub fn flush_dir(&self) -> io::Result<()> {
        flush_handle(self.handle())
    }

    /// Opens a checked descendant with directory-write rights and flushes its
    /// metadata. The empty slice addresses this root itself.
    pub fn flush_dir_relative(&self, path: &[&str]) -> io::Result<()> {
        self.open_dir_checked(path)?.flush_dir()
    }

    fn handle(&self) -> HANDLE {
        self.0.handle.as_raw_handle().cast()
    }

    fn parent_and_leaf<'path>(
        &self,
        path: &[&'path str],
    ) -> io::Result<(Arc<DirectoryNode>, ExistingName<'path>)> {
        let (leaf, parent_path) = path.split_last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "relative path must contain a file name",
            )
        })?;
        let leaf = ExistingName::parse(leaf)?;
        let mut parent = Arc::clone(&self.0);
        for part in parent_path {
            parent = open_directory_child(&parent, ExistingName::parse(part)?)?;
        }
        Ok((parent, leaf))
    }

    fn parent_and_leaf_source<'path>(
        &self,
        path: &[&'path str],
    ) -> io::Result<(Arc<DirectoryNode>, ExistingName<'path>)> {
        let (leaf, parent_path) = path.split_last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "relative path must contain a file name",
            )
        })?;
        let leaf = ExistingName::parse(leaf)?;
        let mut parent = Arc::clone(&self.0);
        for part in parent_path {
            parent = open_directory_child_unchecked(&parent, ExistingName::parse(part)?)?;
        }
        Ok((parent, leaf))
    }
}

fn absolute_drive_components(path: &Path) -> io::Result<(Vec<u16>, Vec<String>)> {
    let mut components = path.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix)) => prefix,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace path must be absolute",
            ));
        }
    };
    let drive = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only local drive paths are supported",
            ));
        }
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace path must be rooted",
        ));
    }
    let root = format!("\\??\\{}:\\", char::from(drive))
        .encode_utf16()
        .collect();
    let mut parts = Vec::new();
    for component in components {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "workspace path component is not UTF-8",
                    )
                })?;
                ExistingName::parse(value)?;
                parts.push(value.to_owned());
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace path contains a non-normal component",
                ));
            }
        }
    }
    Ok((root, parts))
}

fn open_drive_root(name: &[u16]) -> io::Result<Arc<DirectoryNode>> {
    open_drive_root_with_access(
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
    )
}

fn open_drive_root_source(name: &[u16]) -> io::Result<Arc<DirectoryNode>> {
    open_drive_root_with_access(
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
    )
}

fn open_drive_root_writable(name: &[u16]) -> io::Result<Arc<DirectoryNode>> {
    open_drive_root_with_access(
        name,
        FILE_GENERIC_WRITE
            | FILE_LIST_DIRECTORY
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_DELETE_CHILD
            | READ_CONTROL
            | SYNCHRONIZE,
    )
}

fn open_drive_root_with_access(name: &[u16], access: u32) -> io::Result<Arc<DirectoryNode>> {
    let handle = nt_open_absolute(
        name,
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ptr::null_mut(),
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: None,
        name: None,
    }))
}

fn open_directory_child(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
) -> io::Result<Arc<DirectoryNode>> {
    let node = open_directory_child_writable(parent, name)?;
    ensure_private_handle(node.handle.as_raw_handle())?;
    Ok(node)
}

fn open_directory_child_unchecked(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
) -> io::Result<Arc<DirectoryNode>> {
    let handle = open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
        Sharing::Pinned,
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
        name: Some(name.as_str().to_owned()),
    }))
}

fn open_directory_child_writable(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
) -> io::Result<Arc<DirectoryNode>> {
    open_directory_child_writable_with_sharing(parent, name, Sharing::Pinned, true)
}

fn open_directory_child_writable_with_sharing(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
    sharing: Sharing,
    can_delete_children: bool,
) -> io::Result<Arc<DirectoryNode>> {
    let mut access = FILE_LIST_DIRECTORY
        | FILE_GENERIC_WRITE
        | FILE_TRAVERSE
        | FILE_READ_ATTRIBUTES
        | FILE_ADD_FILE
        | FILE_ADD_SUBDIRECTORY
        | READ_CONTROL
        | SYNCHRONIZE;
    if can_delete_children {
        access |= FILE_DELETE_CHILD;
    }
    let handle = open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        access,
        FILE_DIRECTORY_FILE,
        sharing,
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
        name: Some(name.as_str().to_owned()),
    }))
}

fn open_directory_child_with_delete(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
) -> io::Result<Arc<DirectoryNode>> {
    let handle = retry_while_busy(|| {
        open_relative(
            parent.handle.as_raw_handle().cast(),
            name,
            FILE_LIST_DIRECTORY
                | FILE_GENERIC_WRITE
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_DELETE_CHILD
                | FILE_READ_ATTRIBUTES
                | READ_CONTROL
                | DELETE
                | SYNCHRONIZE,
            FILE_DIRECTORY_FILE,
            Sharing::Pinned,
        )
    })?;
    ensure_directory_handle(handle.as_raw_handle())?;
    ensure_private_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
        name: Some(name.as_str().to_owned()),
    }))
}

/// Opens the direct child with delete-pinning before a created-child receipt
/// compares its identity. The caller already checked the receipt's parent and
/// name, so this reuses the ordinary strict child admission path.
fn open_directory_child_for_created_delete(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
) -> io::Result<Arc<DirectoryNode>> {
    open_directory_child_with_delete(parent, name)
}

/// The number of names that reach the object `standard` describes, counting a delete-pending
/// object (whose name is already gone for every new lookup) as unlinked.
fn links_of(standard: &FILE_STANDARD_INFO) -> u64 {
    if standard.DeletePending {
        0
    } else {
        u64::from(standard.NumberOfLinks)
    }
}

/// Whether the object `handle` holds lost its last name since it was opened: a replacing rename
/// or a delete unlinked it (zero links) or left it delete-pending.
fn handle_is_unlinked(handle: *mut c_void) -> bool {
    standard_info(handle).is_ok_and(|standard| links_of(&standard) == 0)
}

/// Pauses before each further look at a name a reader could not find.
///
/// A replacing rename is not one step for a concurrent opener: the POSIX-semantics rename unlinks
/// the old name before it links the new one, so a lookup in between is told `NotFound`, and the
/// classic swap leaves the old file delete-pending for a moment, so a lookup then is told
/// `ACCESS_DENIED`. Measured with one publisher replacing continuously and four readers opening
/// continuously, in eight processes at once, about 0.3 % of lookups saw the first and about
/// 0.005 % the second, the first for up to tens of milliseconds when the publisher was
/// preempted between its two steps. Looking again for 7 ms removed all of the second and all but
/// about 0.01 % of the first.
const LOOKUP_CONFIRMATION: [Duration; 3] = [
    Duration::from_millis(1),
    Duration::from_millis(2),
    Duration::from_millis(4),
];

/// Whether a failed lookup could be a replacing rename caught half done.
fn is_replacement_transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

/// Opens the file `leaf` and runs `admit` on the opened handle.
///
/// A writer that publishes with a replacing rename unlinks the old object while
/// a reader may already hold it, so the object can lose its last name between
/// the open and the checks. `if_unlinked` decides what that means: a reader
/// keeps the generation it opened, a writer opens the name again. Any other
/// admission failure is returned unchanged.
fn open_admitted_file(
    parent: HANDLE,
    leaf: ExistingName<'_>,
    access: u32,
    if_unlinked: IfUnlinked,
    admit: impl Fn(*mut c_void, Linkage) -> io::Result<()>,
) -> io::Result<OwnedHandle> {
    open_admitted_file_pausing(parent, leaf, access, if_unlinked, admit, &mut thread::sleep)
}

/// [`open_admitted_file`] with its pauses injected.
///
/// A reader ([`IfUnlinked::Keep`]) that cannot find the name looks again for
/// [`LOOKUP_CONFIRMATION`] before it believes the name is absent, because the name may be mid
/// replacement. A writer does not: it is about to create the name or act on it, and a missing
/// name is its answer.
fn open_admitted_file_pausing(
    parent: HANDLE,
    leaf: ExistingName<'_>,
    access: u32,
    if_unlinked: IfUnlinked,
    admit: impl Fn(*mut c_void, Linkage) -> io::Result<()>,
    pause: &mut dyn FnMut(Duration),
) -> io::Result<OwnedHandle> {
    let pause = RefCell::new(pause);
    let lookup = || {
        open_relative(
            parent,
            leaf,
            access,
            FILE_NON_DIRECTORY_FILE,
            Sharing::Transient,
        )
    };
    open_admitted(
        if_unlinked,
        || match if_unlinked {
            IfUnlinked::Keep => retry_when(
                lookup,
                |outcome| matches!(outcome, Err(error) if is_replacement_transient(error)),
                &LOOKUP_CONFIRMATION,
                |delay| (pause.borrow_mut())(delay),
            ),
            IfUnlinked::Reopen => lookup(),
        },
        |handle, linkage| admit(handle.as_raw_handle(), linkage),
        |handle| handle_is_unlinked(handle.as_raw_handle()),
        |delay| (pause.borrow_mut())(delay),
    )
}

fn check_replace_destination(
    parent: &Arc<DirectoryNode>,
    name: ExistingName<'_>,
    replace: bool,
    pause: &mut dyn FnMut(Duration),
) -> io::Result<()> {
    reopen_while_replaced(
        || {
            let existing = match open_relative(
                parent.handle.as_raw_handle().cast(),
                name,
                FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
                0,
                Sharing::Transient,
            ) {
                Ok(handle) => handle,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Attempt::Done(()));
                }
                Err(error) => return Err(error),
            };
            if !replace {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "rename destination already exists",
                ));
            }
            match admit_replaceable(existing.as_raw_handle()) {
                Ok(()) => Ok(Attempt::Done(())),
                // Another replacement unlinked the object we probed: look again.
                Err(_) if handle_is_unlinked(existing.as_raw_handle()) => Ok(Attempt::Replaced),
                Err(error) => Err(error),
            }
        },
        pause,
        "rename destination was replaced faster than it could be checked",
    )
}

/// Whether the object a rename would replace is an ordinary private file.
fn admit_replaceable(existing: *mut c_void) -> io::Result<()> {
    let attrs = attributes(existing)?;
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("refusing to replace a reparse point"));
    }
    let standard = standard_info(existing)?;
    if standard.Directory || attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid_data("refusing to replace a directory with a file"));
    }
    if standard.NumberOfLinks != 1 {
        return Err(invalid_data("refusing to replace a multiply linked file"));
    }
    ensure_private_handle(existing)
}

fn create_relative(
    parent: HANDLE,
    name: NewName<'_>,
    access: u32,
    kind: u32,
    sharing: Sharing,
) -> io::Result<OwnedHandle> {
    let name = name.wide();
    let security = private_security_descriptor()?;
    nt_create_relative(
        parent,
        &name,
        access | SYNCHRONIZE,
        sharing.bits(),
        FILE_CREATE,
        kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        security.0,
    )
}

/// Opens an existing child of `parent` without following a final reparse
/// point. `kind` is the `FILE_DIRECTORY_FILE`/`FILE_NON_DIRECTORY_FILE` check
/// the kernel enforces, or zero to accept either.
///
/// Every open here is synchronous (`FILE_SYNCHRONOUS_IO_NONALERT`), and the
/// kernel rejects that option with `STATUS_INVALID_PARAMETER` unless the
/// access mask carries `SYNCHRONIZE`, so the right is added structurally
/// instead of being left to each caller to remember.
fn open_relative(
    parent: HANDLE,
    name: ExistingName<'_>,
    access: u32,
    kind: u32,
    sharing: Sharing,
) -> io::Result<OwnedHandle> {
    let name = name.wide();
    nt_create_relative(
        parent,
        &name,
        access | SYNCHRONIZE,
        sharing.bits(),
        FILE_OPEN,
        kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ptr::null_mut(),
    )
}

fn nt_create_relative(
    parent: HANDLE,
    name: &[u16],
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    security_descriptor: *mut c_void,
) -> io::Result<OwnedHandle> {
    let mut name = name.to_vec();
    let mut unicode = unicode_string(&mut name)?;
    let mut attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: parent,
        object_name: &raw mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor,
        security_qos: ptr::null_mut(),
    };
    nt_create(&mut attributes, access, share, disposition, options)
}

fn nt_open_absolute(
    name: &[u16],
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
    security_descriptor: *mut c_void,
) -> io::Result<OwnedHandle> {
    let mut name = name.to_vec();
    let mut unicode = unicode_string(&mut name)?;
    let mut attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: ptr::null_mut(),
        object_name: &raw mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor,
        security_qos: ptr::null_mut(),
    };
    nt_create(&mut attributes, access, share, disposition, options)
}

fn nt_create(
    attributes: &mut ObjectAttributes,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let mut raw: HANDLE = ptr::null_mut();
    let mut status_block = IoStatusBlock {
        value: IoStatusValue { status: 0 },
        information: 0,
    };
    // SAFETY: object attributes and Unicode storage are live for the call;
    // output handles and I/O status are valid stack out-parameters. NTSTATUS
    // is tested by its signed success rule, not by comparing to zero only.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            access,
            attributes,
            &raw mut status_block,
            ptr::null_mut(),
            0,
            share,
            disposition,
            options,
            ptr::null_mut(),
            0,
        )
    };
    if status < STATUS_SUCCESS {
        return Err(nt_status_error(status));
    }
    if raw.is_null() || raw as isize == -1 {
        return Err(invalid_data("NtCreateFile returned an invalid handle"));
    }
    // SAFETY: successful NtCreateFile transfers one owning HANDLE to us.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
}

/// Converts a failed `NTSTATUS` into the Win32 error the standard library maps
/// to an `io::ErrorKind` (`NotFound`, `AlreadyExists`, `PermissionDenied`, ...).
fn nt_status_error(status: i32) -> io::Error {
    // SAFETY: a pure table lookup over an integer; no pointers are involved.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code.cast_signed())
}

/// Moves the object `source` holds open into `destination_parent` under
/// `name`, replacing an existing file when `replace` is set.
///
/// This is `NtSetInformationFile` rather than `SetFileInformationByHandle`
/// because the Win32 wrapper rejects any rename whose `RootDirectory` is not
/// null (`ERROR_INVALID_PARAMETER`), and a null root means resolving a full
/// path, which would reintroduce the very name lookup this module avoids. The
/// kernel resolves `name` beneath the held destination directory.
///
/// A replacement tries the classic one-step swap first, which no concurrent
/// lookup can catch half done, and falls back to POSIX semantics only when the
/// kernel refuses the swap because another handle holds the destination open
/// (see [`Replacement`]). That keeps a reader that holds the old file open, with
/// delete sharing, from blocking the publication, as on Unix. Older systems fall
/// back to the classic rename for both. A holder that does not share delete (a
/// scanner, say) defeats both and is waited out for a bounded time.
///
/// # What a concurrent opener can see
/// Neither replacement is invisible to a lookup that races it. The POSIX-semantics
/// one unlinks the target name and then links the source name, so an opener in
/// between is told `NotFound` for a name that was never absent. The classic swap
/// leaves the replaced file delete-pending for a moment, so an opener then is told
/// `ACCESS_DENIED`. Measured with one publisher replacing continuously and four
/// readers opening continuously, eight such processes at once: about 0.3 % of
/// lookups saw the first (for up to tens of milliseconds when the publisher was
/// preempted between its two steps), about 0.005 % the second. Trying the swap
/// first therefore keeps the larger exposure to a destination that is held open
/// at the moment of the replacement. Readers of state that is replaced look
/// again before they believe either answer ([`LOOKUP_CONFIRMATION`]); a caller
/// that must never be misled holds the lock the publisher holds, or recovers the
/// name's state from a second source.
fn rename_into(
    source: HANDLE,
    destination_parent: HANDLE,
    name: &[u16],
    replace: bool,
    pause: &mut dyn FnMut(Duration),
) -> io::Result<Replacement> {
    let rename = |flags: u32| match set_rename_information(
        source,
        destination_parent,
        name,
        FILE_RENAME_INFORMATION_EX_CLASS,
        flags,
    ) {
        Err(error) if extended_class_unsupported(&error) => set_rename_information(
            source,
            destination_parent,
            name,
            FILE_RENAME_INFORMATION_CLASS,
            u32::from(replace),
        ),
        outcome => outcome,
    };
    retry_while_busy_pausing(
        || {
            if !replace {
                return rename(0).map(|()| Replacement::NewName);
            }
            let swap = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_IGNORE_READONLY_ATTRIBUTE;
            match rename(swap) {
                Ok(()) => Ok(Replacement::Swapped),
                Err(swap_error) if is_busy(&swap_error) => {
                    match rename(swap | FILE_RENAME_POSIX_SEMANTICS) {
                        Ok(()) => Ok(Replacement::Unlinked),
                        // Both refused for the same reason: report the swap's error.
                        Err(unlink_error) if is_busy(&unlink_error) => Err(swap_error),
                        Err(other) => Err(other),
                    }
                }
                Err(other) => Err(other),
            }
        },
        pause,
    )
}

/// Whether a failed extended request means "this system or file system does not
/// implement the extended information class" rather than a refusal of this
/// particular rename or delete.
fn extended_class_unsupported(error: &io::Error) -> bool {
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    matches!(
        error.raw_os_error(),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER)
    )
}

/// Byte length of a `FILE_RENAME_INFORMATION` request whose target name takes
/// `name_bytes`, or `None` when the length is not representable.
///
/// The structure already contains the first name unit, and the I/O manager
/// rejects a request shorter than the structure itself with
/// `STATUS_INFO_LENGTH_MISMATCH` (Win32 error 24). The header and name of a
/// one-character target add up to 22 bytes, two short of the 24-byte
/// structure, so the length is never allowed to fall below it.
fn rename_information_length(name_bytes: usize) -> Option<usize> {
    offset_of!(RenameInformation, file_name)
        .checked_add(name_bytes)
        .map(|content| content.max(size_of::<RenameInformation>()))
}

/// Issues one `FileRenameInformation`-family request with the target name
/// copied in after the header.
fn set_rename_information(
    source: HANDLE,
    destination_parent: HANDLE,
    name: &[u16],
    class: i32,
    flags: u32,
) -> io::Result<()> {
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(invalid_name)?;
    let name_length = u32::try_from(name_bytes).map_err(|_| invalid_name())?;
    let total = rename_information_length(name_bytes).ok_or_else(invalid_name)?;
    let total_length = u32::try_from(total).map_err(|_| invalid_name())?;
    // Zeroed, 8-byte-aligned storage covers the header and every name unit.
    let mut storage = vec![0_u64; total.div_ceil(size_of::<u64>())];
    let information = storage.as_mut_ptr().cast::<RenameInformation>();
    // SAFETY: `storage` is at least `total` bytes and aligned for the header,
    // so each field write and the name copy stay inside it; the name slice and
    // the storage do not overlap.
    unsafe {
        (&raw mut (*information).flags).write(flags);
        (&raw mut (*information).root_directory).write(destination_parent);
        (&raw mut (*information).file_name_length).write(name_length);
        ptr::copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*information).file_name).cast::<u16>(),
            name.len(),
        );
    }
    let mut status_block = IoStatusBlock {
        value: IoStatusValue { status: 0 },
        information: 0,
    };
    // SAFETY: both handles are live for the call, `information` points at
    // `total_length` initialised bytes, and the status block is a valid stack
    // out-parameter.
    let status = unsafe {
        NtSetInformationFile(
            source,
            &raw mut status_block,
            information.cast_const().cast(),
            total_length,
            class,
        )
    };
    if status < STATUS_SUCCESS {
        Err(nt_status_error(status))
    } else {
        Ok(())
    }
}

/// Replaces `destination` with `source`, both absolute paths to ordinary files, using POSIX
/// semantics: the destination is unlinked at once even while another handle holds it open (and
/// even if it carries the read-only attribute), then the source takes its name.
///
/// This is the second rung of [`super::file::replace`], used when the one-step swap is refused.
/// Neither file is followed through a final link: a link is renamed or replaced as itself.
///
/// # Errors
/// The kernel's error when either file cannot be opened or the rename is refused, including the
/// sharing violation of a holder that does not share delete access.
pub(crate) fn replace_unlinking(source: &Path, destination: &Path) -> io::Result<()> {
    let source_name = nt_path(source)?;
    let destination_name = nt_path(destination)?;
    let handle = nt_open_absolute(
        &source_name,
        DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        ptr::null_mut(),
    )?;
    set_rename_information(
        handle.as_raw_handle().cast(),
        ptr::null_mut(),
        &destination_name,
        FILE_RENAME_INFORMATION_EX_CLASS,
        FILE_RENAME_REPLACE_IF_EXISTS
            | FILE_RENAME_POSIX_SEMANTICS
            | FILE_RENAME_IGNORE_READONLY_ATTRIBUTE,
    )
    .map_err(|error| {
        if extended_class_unsupported(&error) {
            io::Error::from(io::ErrorKind::Unsupported)
        } else {
            error
        }
    })
}

/// The NT object-manager name (`\??\C:\dir\file`, `\??\UNC\server\share\file`) of an absolute
/// Win32 path, with `.` and `..` already resolved by [`std::path::absolute`].
fn nt_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let mut components = absolute.components();
    let mut name = String::from(r"\??\");
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                name.push(char::from(letter));
                name.push(':');
            }
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                name.push_str(r"UNC\");
                name.push_str(&server.to_string_lossy());
                name.push('\\');
                name.push_str(&share.to_string_lossy());
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "only drive and UNC paths can be renamed by name",
                ));
            }
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a rename by name needs an absolute path",
            ));
        }
    }
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                name.push('\\');
                name.push_str(part.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "path component is not Unicode")
                })?);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path holds a component that is not a plain name",
                ));
            }
        }
    }
    Ok(name.encode_utf16().collect())
}

fn unicode_string(buffer: &mut [u16]) -> io::Result<UnicodeString> {
    let bytes = buffer.len().checked_mul(2).ok_or_else(invalid_name)?;
    if bytes > u16::MAX as usize {
        return Err(invalid_name());
    }
    Ok(UnicodeString {
        length: bytes as u16,
        maximum_length: bytes as u16,
        buffer: buffer.as_mut_ptr(),
    })
}

/// The textual form (`S-1-5-21-...`) of a binary SID, the way a security
/// descriptor string names a trustee.
fn sid_text(bytes: &[u8]) -> io::Result<String> {
    if bytes.len() < 8 || bytes[1] as usize > (bytes.len() - 8) / 4 {
        return Err(invalid_data("current-user SID is malformed"));
    }
    let sub_authority_count = bytes[1] as usize;
    if bytes.len() != 8 + sub_authority_count * 4 {
        return Err(invalid_data("current-user SID has an invalid length"));
    }
    let identifier_authority = bytes[2..8]
        .iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
    let mut text = format!("S-{}-{identifier_authority}", bytes[0]);
    for index in 0..sub_authority_count {
        let offset = 8 + index * 4;
        let sub_authority = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| invalid_data("current-user SID is malformed"))?,
        );
        text.push('-');
        text.push_str(&sub_authority.to_string());
    }
    Ok(text)
}

fn private_security_descriptor() -> io::Result<PrivateSecurityDescriptor> {
    let trustee = sid_text(current_user()?.as_bytes())?;
    // `P` blocks inherited grants and the sole `GA` ACE names the current
    // token user. This descriptor is supplied to NtCreateFile, so the object
    // never appears under a broader inherited ACL, even briefly.
    let sddl = format!("D:P(A;;GA;;;{trustee})");
    let mut wide = sddl.encode_utf16().collect::<Vec<_>>();
    wide.push(0);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut descriptor_size = 0_u32;
    // SAFETY: `wide` is NUL-terminated UTF-16 and both output pointers are
    // writable. Windows allocates the returned descriptor with LocalAlloc.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &raw mut descriptor,
            &raw mut descriptor_size,
        )
    };
    if converted == 0 || descriptor.is_null() || descriptor_size == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PrivateSecurityDescriptor(descriptor.cast()))
}

struct PrivateSecurityDescriptor(*mut c_void);

impl Drop for PrivateSecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW
        // allocated the descriptor with LocalAlloc; LocalFree releases it.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}

fn ensure_directory_handle(handle: *mut c_void) -> io::Result<()> {
    let attributes = attributes(handle)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("workspace path contains a reparse point"));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(invalid_data("workspace path component is not a directory"));
    }
    let standard = standard_info(handle)?;
    if !standard.Directory {
        return Err(invalid_data("workspace object is not a directory"));
    }
    if standard.NumberOfLinks != 1 {
        return Err(invalid_data("workspace directory has multiple links"));
    }
    Ok(())
}

fn ensure_regular_file_handle(handle: *mut c_void) -> io::Result<()> {
    ensure_regular_file_handle_with(handle, Linkage::Named)
}

fn ensure_regular_file_handle_with(handle: *mut c_void, linkage: Linkage) -> io::Result<()> {
    let attributes = attributes(handle)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("workspace file is a reparse point"));
    }
    let standard = standard_info(handle)?;
    if standard.Directory || attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid_data("workspace object is not a regular file"));
    }
    if !linkage.admits(links_of(&standard)) {
        return Err(invalid_data("workspace file has multiple hard links"));
    }
    Ok(())
}

fn ensure_private_handle(handle: *mut c_void) -> io::Result<()> {
    if !is_owned_by_current_user(&owner_of(&HandleRef(handle))?)? {
        return Err(invalid_data("workspace object has a foreign owner"));
    }
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: GetSecurityInfo fills descriptor and DACL pointers owned by the
    // returned descriptor allocation; both are read only until LocalFree.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 || descriptor.is_null() {
        return Err(if status != 0 {
            io::Error::from_raw_os_error(status as i32)
        } else {
            invalid_data("Windows returned no security descriptor")
        });
    }
    let allocation = SecurityDescriptor(descriptor.cast());
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl_from_sd = ptr::null_mut();
    // SAFETY: descriptor is a live valid descriptor returned by GetSecurityInfo.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &raw mut present,
            &raw mut dacl_from_sd,
            &raw mut defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
    let mut revision = 0;
    // SAFETY: same descriptor lifetime as above; output pointers are valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || dacl_from_sd.is_null() || control & SE_DACL_PROTECTED == 0 {
        return Err(invalid_data(
            "workspace object does not have a protected DACL",
        ));
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl_from_sd` is valid for the lifetime of `allocation`.
    if unsafe {
        GetAclInformation(
            dacl_from_sd,
            (&raw mut info).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if info.AceCount != 1 {
        return Err(invalid_data(
            "workspace DACL grants access beyond the current user",
        ));
    }
    let mut ace = ptr::null_mut();
    // SAFETY: index zero is valid because AceCount was checked above.
    if unsafe { GetAce(dacl_from_sd, 0, &raw mut ace) } == 0 || ace.is_null() {
        return Err(io::Error::last_os_error());
    }
    // ACCESS_ALLOWED_ACE begins with ACE_HEADER and a mask followed by SID.
    // SAFETY: GetAce returned the sole ACE in a valid ACL.
    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    if u32::from(allowed.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || !is_full_control(allowed.Mask)
    {
        return Err(invalid_data("workspace DACL is not current-user-only"));
    }
    let sid = current_user()?;
    let sid_end = offset_of!(ACCESS_ALLOWED_ACE, SidStart)
        .checked_add(sid.as_bytes().len())
        .ok_or_else(|| invalid_data("workspace DACL ACE length overflow"))?;
    if (allowed.Header.AceSize as usize) < sid_end || allowed.Header.AceFlags != 0 {
        return Err(invalid_data(
            "workspace DACL ACE is malformed or inheritable",
        ));
    }
    // SAFETY: SidStart is the first byte of the SID embedded in this ACE and
    // GetSecurityInfo's descriptor remains live until this function returns.
    let ace_sid = unsafe {
        std::slice::from_raw_parts(
            (&raw const allowed.SidStart).cast::<u8>(),
            sid.as_bytes().len(),
        )
    };
    if ace_sid != sid.as_bytes() {
        return Err(invalid_data(
            "workspace DACL grants access to a different SID",
        ));
    }
    drop(allocation);
    Ok(())
}

/// Whether a stored ACE mask grants full control of a file object.
///
/// The private descriptor requests `GENERIC_ALL`, but the object manager maps
/// generic rights through the file generic mapping when it stores the ACE, so
/// a descriptor this module created reads back as `FILE_ALL_ACCESS`. An
/// unmapped `GENERIC_ALL` is equivalent and also accepted.
fn is_full_control(mask: u32) -> bool {
    matches!(mask, FILE_ALL_ACCESS | GENERIC_ALL)
}

struct HandleRef(*mut c_void);

impl AsRawHandle for HandleRef {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0
    }
}

struct SecurityDescriptor(*mut c_void);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: GetSecurityInfo allocated this descriptor with LocalAlloc;
        // LocalFree is the matching release and accepts null.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}

fn attributes(handle: *mut c_void) -> io::Result<u32> {
    let mut info = FileAttributeTagInfo {
        file_attributes: 0,
        reparse_tag: 0,
    };
    // SAFETY: `handle` is a live file handle and `info` is writable storage of
    // the exact size required by FileAttributeTagInfo.
    let read = unsafe {
        GetFileInformationByHandleEx(
            handle.cast(),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            size_of::<FileAttributeTagInfo>() as u32,
        )
    };
    if read == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info.file_attributes)
    }
}

fn standard_info(handle: *mut c_void) -> io::Result<FILE_STANDARD_INFO> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: `handle` is a live file handle and `info` is writable storage of
    // the exact size required by FileStandardInfo.
    let read = unsafe {
        GetFileInformationByHandleEx(
            handle.cast(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if read == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

fn identity_info(handle: *mut c_void) -> io::Result<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: `handle` is a live file handle and `info` is writable storage of
    // the exact size required by FileIdInfo.
    let read = unsafe {
        GetFileInformationByHandleEx(
            handle.cast(),
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if read == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

/// The 128-bit object id NTFS and ReFS report for a file or directory
/// (`FILE_ID_128`). ReFS ids use all 128 bits, so comparing only the low eight
/// bytes would let two distinct objects look identical; NTFS ids occupy the low
/// eight bytes and leave the rest zero, which compares exactly the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileId128([u8; 16]);

impl FileId128 {
    /// The id of the object an open handle holds.
    fn of_handle(handle: *mut c_void) -> io::Result<Self> {
        Ok(Self(identity_info(handle)?.FileId.Identifier))
    }
}

fn same_directory_object(left: *mut c_void, right: *mut c_void) -> io::Result<bool> {
    let left = identity_info(left)?;
    let right = identity_info(right)?;
    Ok(left.VolumeSerialNumber == right.VolumeSerialNumber
        && left.FileId.Identifier == right.FileId.Identifier)
}

/// One directory entry as enumeration reported it.
#[derive(Debug, Eq, PartialEq)]
struct EnumeratedEntry {
    name: String,
    id: FileId128,
}

fn enumerate_names(handle: HANDLE, maximum: usize) -> io::Result<Vec<EnumeratedEntry>> {
    let mut buffer = vec![0_u64; 8192];
    let mut restart = true;
    let mut names = Vec::new();
    loop {
        let class: FILE_INFO_BY_HANDLE_CLASS = if restart {
            FileIdExtdDirectoryRestartInfo
        } else {
            FileIdExtdDirectoryInfo
        };
        // SAFETY: `handle` is a live directory handle and `buffer` is aligned
        // writable storage passed with its exact byte capacity.
        let read = unsafe {
            GetFileInformationByHandleEx(
                handle,
                class,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
            )
        };
        if read == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error);
        }
        restart = false;
        decode_directory_records(&buffer, &mut names, maximum)?;
    }
    Ok(names)
}

/// Decodes the `FILE_ID_EXTD_DIR_INFO` records of one enumeration result into
/// `names`, refusing a malformed chain. `buffer` is the whole result buffer;
/// the chain ends at the first record whose `NextEntryOffset` is zero.
fn decode_directory_records(
    buffer: &[u64],
    names: &mut Vec<EnumeratedEntry>,
    maximum: usize,
) -> io::Result<()> {
    let bytes = std::mem::size_of_val(buffer);
    let name_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    let mut offset = 0usize;
    loop {
        if offset % align_of::<FILE_ID_EXTD_DIR_INFO>() != 0
            || offset
                .checked_add(size_of::<FILE_ID_EXTD_DIR_INFO>())
                .is_none_or(|end| end > bytes)
        {
            return Err(invalid_data("Windows returned a malformed directory entry"));
        }
        // SAFETY: the offset is aligned and the complete fixed-size structure,
        // including its first name unit and tail padding, lies inside `buffer`
        // (checked above); every bit pattern is a valid
        // `FILE_ID_EXTD_DIR_INFO`, which has only integer fields.
        let entry = unsafe {
            &*buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<FILE_ID_EXTD_DIR_INFO>()
        };
        let name_bytes = entry.FileNameLength as usize;
        let end = offset
            .checked_add(name_offset)
            .and_then(|start| start.checked_add(name_bytes))
            .ok_or_else(|| invalid_data("Windows returned an oversized directory name"))?;
        if name_bytes == 0
            || name_bytes % 2 != 0
            || end > bytes
            || (buffer.as_ptr() as usize + offset + name_offset) % align_of::<u16>() != 0
        {
            return Err(invalid_data(
                "Windows returned an invalid UTF-16 directory name",
            ));
        }
        let next_offset = if entry.NextEntryOffset == 0 {
            None
        } else {
            let next = usize::try_from(entry.NextEntryOffset)
                .map_err(|_| invalid_data("invalid directory offset"))?;
            let next_offset = offset
                .checked_add(next)
                .ok_or_else(|| invalid_data("invalid directory offset"))?;
            if next < size_of::<FILE_ID_EXTD_DIR_INFO>()
                || next % align_of::<FILE_ID_EXTD_DIR_INFO>() != 0
                || next_offset < end
                || next_offset >= bytes
            {
                return Err(invalid_data("Windows returned an invalid directory offset"));
            }
            Some(next_offset)
        };
        // SAFETY: validated byte length and aligned pointer within the result buffer.
        let wide = unsafe {
            std::slice::from_raw_parts(
                buffer
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset + name_offset)
                    .cast::<u16>(),
                name_bytes / 2,
            )
        };
        let name = String::from_utf16(wide)
            .map_err(|_| invalid_data("workspace child name is not valid UTF-16"))?;
        if name != "." && name != ".." {
            if names.len() >= maximum {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "workspace directory entry limit exceeded",
                ));
            }
            names.push(EnumeratedEntry {
                name,
                id: FileId128(entry.FileId.Identifier),
            });
        }
        match next_offset {
            Some(next) => offset = next,
            None => return Ok(()),
        }
    }
}

fn remove_tree_contents(
    directory: &Arc<DirectoryNode>,
    visited: &mut usize,
    maximum_entries: usize,
) -> io::Result<()> {
    let names = enumerate_names(
        directory.handle.as_raw_handle().cast(),
        maximum_entries.saturating_sub(*visited),
    )?;
    for EnumeratedEntry { name, id } in names {
        *visited = (*visited)
            .checked_add(1)
            .filter(|count| *count <= maximum_entries)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "workspace directory tree entry limit exceeded",
                )
            })?;
        let entry_name = ExistingName::parse(&name)?;
        let handle = retry_while_busy(|| {
            open_relative(
                directory.handle.as_raw_handle().cast(),
                entry_name,
                FILE_GENERIC_WRITE
                    | FILE_READ_ATTRIBUTES
                    | READ_CONTROL
                    | DELETE
                    | FILE_LIST_DIRECTORY
                    | SYNCHRONIZE,
                0,
                Sharing::Transient,
            )
        })?;
        let attrs = attributes(handle.as_raw_handle())?;
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid_data(
                "refusing to remove a reparse point from workspace",
            ));
        }
        let standard = standard_info(handle.as_raw_handle())?;
        if standard.NumberOfLinks != 1 {
            return Err(invalid_data(
                "refusing to remove a multiply linked workspace child",
            ));
        }
        if FileId128::of_handle(handle.as_raw_handle())? != id {
            return Err(invalid_data(
                "workspace entry changed during directory enumeration",
            ));
        }
        ensure_private_handle(handle.as_raw_handle())?;
        if standard.Directory {
            let child = Arc::new(DirectoryNode {
                handle,
                _parent: Some(Arc::clone(directory)),
                name: Some(name.clone()),
            });
            remove_tree_contents(&child, visited, maximum_entries)?;
            mark_delete(child.handle.as_raw_handle())?;
        } else {
            mark_delete(handle.as_raw_handle())?;
        }
    }
    flush_handle(directory.handle.as_raw_handle())
}

/// Unlinks the object `handle` holds open, waiting out a scanner that holds it
/// without sharing delete for a bounded time.
fn mark_delete(handle: *mut c_void) -> io::Result<()> {
    retry_while_busy(|| mark_delete_once(handle))
}

fn mark_delete_once(handle: *mut c_void) -> io::Result<()> {
    let extended = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `handle` is live and `extended` is readable input storage with
    // the documented information-class size.
    let deleted = unsafe {
        SetFileInformationByHandle(
            handle.cast(),
            FileDispositionInfoEx,
            (&raw const extended).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if deleted != 0 {
        return Ok(());
    }
    let refusal = io::Error::last_os_error();
    if !extended_class_unsupported(&refusal) {
        return Err(refusal);
    }
    // Older Windows filesystems may not implement the Ex information class.
    let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `handle` is live and `legacy` is readable input storage with the
    // documented information-class size.
    let deleted = unsafe {
        SetFileInformationByHandle(
            handle.cast(),
            FileDispositionInfo,
            (&raw const legacy).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if deleted == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn flush_handle(handle: *mut c_void) -> io::Result<()> {
    // SAFETY: `handle` remains live for the call; FlushFileBuffers reads no
    // caller-owned memory and reports unsupported directory flushes directly.
    if unsafe { FlushFileBuffers(handle.cast()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn file_from_handle(handle: OwnedHandle) -> io::Result<File> {
    // SAFETY: `handle` owns a valid synchronous file handle returned by
    // NtCreateFile; ownership transfers to `File` exactly once.
    Ok(unsafe { File::from_raw_handle(handle.into_raw_handle()) })
}

/// A path component to look up in a directory that already holds it, or that
/// may: the name satisfies the syntax every component must (no separators,
/// reserved characters, trailing dot or space, or more than 255 UTF-16 units)
/// and nothing more.
///
/// Whether the operating system would route a name to a DOS device matters
/// only when a name is *created*; an entry that already exists, an ancestor of
/// a root path, and every name an enumeration returns are looked up exactly as
/// they are spelled. Source trees routinely hold `aux.c` and `con.rs`.
#[derive(Clone, Copy, Debug)]
struct ExistingName<'a>(&'a str);

impl<'a> ExistingName<'a> {
    fn parse(component: &'a str) -> io::Result<Self> {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(' ')
            || component.ends_with('.')
            || component.chars().any(|ch| {
                ch == '\0'
                    || ch < ' '
                    || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            })
            || component.encode_utf16().count() > MAX_COMPONENT_UNITS
        {
            return Err(invalid_name());
        }
        Ok(Self(component))
    }

    const fn as_str(self) -> &'a str {
        self.0
    }

    fn wide(self) -> Vec<u16> {
        self.0.encode_utf16().collect()
    }
}

/// A path component about to be created, or renamed to.
///
/// On top of the syntax rules it must not be a name the running system routes
/// to a DOS device. A device name would create an entry that Win32 paths cannot
/// reach, so a tool that walks the workspace by path would list, open or
/// delete the device instead of the file. Only a [`NewName`] can be passed to
/// `create_relative`, so the check cannot be skipped where it matters and
/// cannot be applied where it must not be.
#[derive(Clone, Copy, Debug)]
struct NewName<'a>(ExistingName<'a>);

impl<'a> NewName<'a> {
    fn new(name: ExistingName<'a>) -> io::Result<Self> {
        if is_dos_device_name(name.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the name is a DOS device name on this system",
            ));
        }
        Ok(Self(name))
    }

    fn parse(component: &'a str) -> io::Result<Self> {
        Self::new(ExistingName::parse(component)?)
    }

    const fn existing(self) -> ExistingName<'a> {
        self.0
    }

    const fn as_str(self) -> &'a str {
        self.0.as_str()
    }

    fn wide(self) -> Vec<u16> {
        self.0.wide()
    }
}

/// The longest component, in UTF-16 units, that NTFS stores.
const MAX_COMPONENT_UNITS: usize = 255;

/// Whether the running system's own path normalization routes `component` to a
/// DOS device (`NUL`, ...), judged by `RtlIsDosDeviceName_U`.
///
/// The answer depends on the Windows build: older systems treat `aux.c` and
/// `CON.txt` as the `AUX` and `CON` devices, while Windows 11 builds such as
/// 26200 create them as ordinary files and treat only bare device names
/// specially. Asking the system, instead of a hard-coded list, refuses exactly
/// what this system would misroute, and nothing it would accept.
fn is_dos_device_name(component: &str) -> bool {
    let wide: Vec<u16> = component.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // which only reads it up to the terminator and returns an integer.
    unsafe { RtlIsDosDeviceName_U(wide.as_ptr()) != 0 }
}

fn invalid_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid Windows path component",
    )
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod adverse_tests;

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::{EntryKind, Sharing, WorkspaceDirectoryCreateFailure, WorkspaceRoot};
    use std::fs;
    use std::io::{self, Read as _, Write as _};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fresh_path(label: &str) -> PathBuf {
        let tick = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-workspace-{label}-{}-{tick}",
            std::process::id()
        ))
    }

    #[test]
    fn postcreate_setup_refusal_rolls_back_the_held_object_not_its_replacement_name() {
        let path = fresh_path("created-handle-rollback");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let result = root.create_child_dir_exclusive_with_sharing(
            "stage",
            Sharing::Transient,
            || {
                fs::rename(path.join("stage"), path.join("original-created-object"))?;
                fs::create_dir(path.join("stage"))?;
                fs::write(path.join("stage/payload"), b"replacement")?;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected setup refusal after FILE_CREATE",
                ))
            },
        );
        let Err(WorkspaceDirectoryCreateFailure::Created { directory, source }) = result else {
            panic!("post-create refusal lost the exact created handle");
        };
        assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
        directory
            .remove_created_empty_self()
            .expect("rollback deletes only the held created object");
        assert!(!path.join("original-created-object").exists());
        assert_eq!(
            fs::read(path.join("stage/payload")).expect("replacement survives"),
            b"replacement"
        );

        drop(root);
        fs::remove_dir_all(path).expect("remove fixture");
    }

    #[test]
    fn cold_reopen_and_checked_listing_work() {
        let path = fresh_path("reopen");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let mut file = root
            .create_file_exclusive(&["payload.bin"])
            .expect("create file");
        file.write_all(b"durable bytes").expect("write");
        file.sync_all().expect("flush file");
        drop(file);
        drop(root);

        let reopened = WorkspaceRoot::open(&path).expect("cold reopen");
        let mut file = reopened
            .open_file_read_checked(&["payload.bin"])
            .expect("checked read");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("read");
        assert_eq!(bytes, b"durable bytes");
        assert_eq!(
            reopened.read_dir_checked(&[]).expect("list")[0].kind,
            EntryKind::File
        );
        drop(reopened);
        fs::remove_dir_all(path).expect("test cleanup");
    }

    #[test]
    fn hard_link_and_symlink_children_are_rejected() {
        use std::os::windows::fs::symlink_file;

        let path = fresh_path("links");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let outside = fresh_path("outside");
        fs::write(&outside, b"outside").expect("outside file");
        fs::hard_link(&outside, path.join("hardlink")).expect("create hard link");
        assert!(root.open_file_read_checked(&["hardlink"]).is_err());
        let symlink = path.join("symlink");
        let has_symlink = match symlink_file(&outside, &symlink) {
            Ok(()) => {
                assert!(root.open_file_read_checked(&["symlink"]).is_err());
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(error) => panic!("create symlink: {error}"),
        };
        assert!(root.read_dir_checked(&[]).is_err());
        drop(root);
        fs::remove_file(path.join("hardlink")).expect("remove hard link");
        if has_symlink {
            fs::remove_file(symlink).expect("remove symlink");
        }
        fs::remove_dir(path).expect("remove root");
        fs::remove_file(outside).expect("remove outside");
    }

    #[test]
    fn directory_junction_is_rejected_without_traversing_its_target() {
        use std::process::Command;

        let path = fresh_path("junction-root");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let outside = fresh_path("junction-target");
        fs::create_dir(&outside).expect("create junction target");
        fs::write(outside.join("must-survive"), b"outside").expect("create outside marker");
        let junction = path.join("junction");
        let status = Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&junction)
            .arg(&outside)
            .status()
            .expect("create junction with cmd mklink");
        assert!(status.success(), "cmd mklink /J failed with {status}");
        assert!(root.open_dir_checked(&["junction"]).is_err());
        assert!(root.read_dir_checked(&[]).is_err());
        drop(root);
        fs::remove_dir(&junction).expect("remove junction itself");
        assert_eq!(
            fs::read(outside.join("must-survive")).expect("read marker"),
            b"outside"
        );
        fs::remove_dir_all(path).expect("remove workspace root");
        fs::remove_dir_all(outside).expect("remove target");
    }

    #[test]
    fn rename_is_relative_to_pinned_directory() {
        let path = fresh_path("rename");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let mut source = root.create_file_exclusive(&["from"]).expect("source");
        source.write_all(b"inside").expect("write source");
        source.sync_all().expect("flush source");
        drop(source);
        root.rename_relative(&["from"], &["to"], false)
            .expect("rename");
        assert!(root.open_file_read_checked(&["from"]).is_err());
        let mut target = root.open_file_read_checked(&["to"]).expect("target");
        let mut bytes = Vec::new();
        target.read_to_end(&mut bytes).expect("read target");
        assert_eq!(bytes, b"inside");
        drop(root);
        fs::remove_dir_all(path).expect("test cleanup");
    }

    #[test]
    fn rename_race_cannot_redirect_to_a_symlink_target() {
        use std::os::windows::fs::symlink_file;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;

        let path = fresh_path("rename-race");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let outside = fresh_path("rename-victim");
        let source = path.join("source");
        let parked = path.join("parked");
        let published = path.join("published");
        fs::write(&outside, b"outside must survive").expect("create outside victim");
        let mut file = root
            .create_file_exclusive(&["source"])
            .expect("create source");
        file.write_all(b"workspace bytes").expect("write source");
        file.sync_all().expect("flush source");
        drop(file);

        let running = Arc::new(AtomicBool::new(true));
        let race_flag = Arc::clone(&running);
        let race_source = source.clone();
        let race_parked = parked.clone();
        let race_outside = outside.clone();
        let racer = thread::spawn(move || {
            while race_flag.load(Ordering::Relaxed) {
                if fs::rename(&race_source, &race_parked).is_ok() {
                    if symlink_file(&race_outside, &race_source).is_ok() {
                        thread::yield_now();
                        let _ = fs::remove_file(&race_source);
                    }
                    let _ = fs::rename(&race_parked, &race_source);
                }
                thread::yield_now();
            }
        });

        let mut moved = false;
        for _ in 0..2_000 {
            if root
                .rename_relative(&["source"], &["published"], true)
                .is_ok()
            {
                moved = true;
                break;
            }
            thread::yield_now();
        }
        running.store(false, Ordering::Relaxed);
        racer.join().expect("join rename racer");
        if !moved {
            if parked.exists() {
                if source.exists() {
                    fs::remove_file(&source).expect("remove raced symlink");
                }
                fs::rename(&parked, &source).expect("restore source after race");
            }
            root.rename_relative(&["source"], &["published"], true)
                .expect("rename after racer stopped");
        }
        assert_eq!(
            fs::read(&outside).expect("read outside victim"),
            b"outside must survive"
        );
        // The racer renames the very object the workspace renamed, through a
        // handle it opened earlier, so the object may legitimately end under
        // any of the three names; `adverse_tests` pins the interleaving
        // deterministically. What must hold under every interleaving is that
        // the workspace bytes survive exactly once, and that the name this
        // module publishes never became a link or the victim's bytes.
        let survivors = [&source, &parked, &published]
            .into_iter()
            .filter(|name| {
                fs::symlink_metadata(name).is_ok_and(|metadata| metadata.is_file())
                    && fs::read(name).is_ok_and(|bytes| bytes == b"workspace bytes")
            })
            .count();
        assert_eq!(survivors, 1, "the workspace file must survive exactly once");
        if let Ok(metadata) = fs::symlink_metadata(&published) {
            assert!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "the published name is never a link"
            );
            assert_ne!(
                fs::read(&published).expect("read published file"),
                b"outside must survive",
                "the victim's bytes were never published"
            );
        }
        drop(root);
        for candidate in [&source, &parked] {
            if candidate.exists() {
                let _ = fs::remove_file(candidate);
            }
        }
        fs::remove_dir_all(path).expect("remove workspace root");
        fs::remove_file(outside).expect("remove outside victim");
    }

    #[test]
    fn rename_outcome_keeps_native_commit_stage_when_parent_flush_fails() {
        use super::DirectoryRenameError;

        let path = fresh_path("rename-flush-failure");
        let root = WorkspaceRoot::create(&path).expect("create private root");
        let mut source = root
            .create_file_exclusive(&["staged"])
            .expect("create staged file");
        source
            .write_all(b"new selected bytes")
            .expect("write staged file");
        source.sync_all().expect("flush staged file");
        drop(source);

        let outcome = root.rename_checked_entry_with_flush(
            &["staged"],
            &["selected"],
            false,
            false,
            || {},
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected parent flush failure after native rename",
                ))
            },
        );
        let Err(DirectoryRenameError::CommittedButNotDurable(error)) = outcome else {
            panic!("native post-rename flush failure lost commit state: {outcome:?}");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(root.open_file_read_checked(&["staged"]).is_err());
        let mut selected = root
            .open_file_read_checked(&["selected"])
            .expect("read committed selected file");
        let mut bytes = Vec::new();
        selected
            .read_to_end(&mut bytes)
            .expect("read selected bytes");
        assert_eq!(bytes, b"new selected bytes");
        drop(selected);
        drop(root);
        fs::remove_dir_all(path).expect("test cleanup");
    }
}
