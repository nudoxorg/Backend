//! Handle-relative, reparse-safe operations for private worker workspaces.
//!
//! Every name passed to this module is one validated UTF-8 path component.
//! Directory traversal and mutation use NT handles with `RootDirectory`, so
//! the kernel resolves each name beneath an already-open directory object.
#![allow(
    unsafe_code,
    reason = "reviewed NT relative-open and handle metadata boundary for Windows workspace files"
)]

use super::identity::{current_user, is_owned_by_current_user, owner_of};
use super::security::restrict_handle_to_current_user;
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::offset_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle as _, IntoRawHandle as _, OwnedHandle};
use std::path::{Component, Path, Prefix};
use std::ptr;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, GetAce,
    GetAclInformation, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
    PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR_CONTROL,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_GENERIC_WRITE, FILE_ID_BOTH_DIR_INFO,
    FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_STANDARD_INFO, FILE_TRAVERSE,
    FILE_WRITE_DATA, FileAttributeTagInfo, FileDispositionInfo, FileDispositionInfoEx,
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdInfo,
    FileStandardInfo, FlushFileBuffers, GetFileInformationByHandleEx, READ_CONTROL, SYNCHRONIZE,
    SetFileInformationByHandle, WRITE_DAC,
};

/// Native `FILE_INFORMATION_CLASS` value for `FileRenameInformation`, used
/// with `NtSetInformationFile` directly. Distinct from (and numbered
/// differently than) the Win32 `FILE_INFO_BY_HANDLE_CLASS` `FileRenameInfo`
/// constant used by `SetFileInformationByHandle`, which does not reliably
/// honor a non-null `RootDirectory`.
const FILE_RENAME_INFORMATION_CLASS: u32 = 10;
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
const ACL_INFORMATION_CLASS_SIZE: i32 = 2;

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
        file_information: *mut c_void,
        length: u32,
        file_information_class: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

#[repr(C)]
struct FileAttributeTagInfo {
    file_attributes: u32,
    reparse_tag: u32,
}

struct DirectoryNode {
    handle: OwnedHandle,
    _parent: Option<Arc<DirectoryNode>>,
}

/// A pinned private directory. Its ancestor handles stay open for its lifetime,
/// preventing a concurrent rename from moving a descendant out of the root.
#[derive(Clone)]
pub struct WorkspaceRoot(Arc<DirectoryNode>);

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
        let (drive_root, parts) = absolute_drive_components(path)?;
        let mut current = open_drive_root(&drive_root)?;
        if parts.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root cannot be the drive root",
            ));
        }
        for part in &parts[..parts.len() - 1] {
            current = open_directory_child_unchecked(&current, part)?;
        }
        let final_part = parts.last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root cannot be the drive root",
            )
        })?;
        current = open_directory_child_writable(&current, final_part)?;
        ensure_private_handle(current.handle.as_raw_handle())?;
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
        validate_component(name)?;
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
                current = open_directory_child_unchecked(&current, part)?;
            }
            open_directory_child_writable(&current, parts.last().ok_or_else(invalid_name)?)?
        };
        let parent = Self(parent);
        parent.create_child_dir_exclusive(name)
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
        validate_component(name)?;
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
            current = open_directory_child_unchecked(&current, part)?;
        }
        let parent = Self(open_directory_child_writable(
            &current,
            parts.last().ok_or_else(invalid_name)?,
        )?);
        if !is_owned_by_current_user(&owner_of(&HandleRef(parent.0.handle.as_raw_handle()))?)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "application data parent has a foreign owner",
            ));
        }

        match open_directory_child(&parent.0, name) {
            Ok(child) => {
                let child = Self(child);
                child.flush_dir()?;
                parent.flush_dir()
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match parent.create_child_dir_exclusive(name) {
                    Ok(child) => {
                        child.flush_dir()?;
                        parent.flush_dir()
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let child = Self(open_directory_child(&parent.0, name)?);
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
        validate_component(name)?;
        let handle = create_relative(
            self.handle(),
            name,
            FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_DELETE_CHILD
                | FILE_GENERIC_WRITE
                | FILE_LIST_DIRECTORY
                | FILE_TRAVERSE
                | FILE_READ_ATTRIBUTES
                | READ_CONTROL
                | WRITE_DAC
                | DELETE
                | SYNCHRONIZE,
            FILE_DIRECTORY_FILE,
        )?;
        restrict_handle_to_current_user(&handle)?;
        ensure_directory_handle(handle.as_raw_handle())?;
        ensure_private_handle(handle.as_raw_handle())?;
        Ok(Self(Arc::new(DirectoryNode {
            handle,
            _parent: Some(Arc::clone(&self.0)),
        })))
    }

    /// Opens a child directory beneath this pinned root after rejecting
    /// reparses, hard-linked directories, foreign owners, and non-private ACLs.
    pub fn open_dir_checked(&self, path: &[&str]) -> io::Result<Self> {
        let mut current = Arc::clone(&self.0);
        for component in path {
            validate_component(component)?;
            current = open_directory_child(&current, component)?;
        }
        ensure_private_handle(current.handle.as_raw_handle())?;
        Ok(Self(current))
    }

    /// Creates a new regular file exclusively beneath the pinned root. The
    /// protected current-user DACL is applied before the file handle escapes.
    pub fn create_file_exclusive(&self, path: &[&str]) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = create_relative(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_ADD_FILE
                | FILE_READ_ATTRIBUTES
                | FILE_READ_DATA
                | FILE_WRITE_DATA
                | READ_CONTROL
                | WRITE_DAC
                | DELETE
                | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
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
        let handle = open_relative_with_share(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )?;
        ensure_regular_file_handle(handle.as_raw_handle())?;
        ensure_private_handle(handle.as_raw_handle())?;
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
        let (source_parent, source_leaf) = self.parent_and_leaf(source)?;
        let (destination_parent, destination_leaf) = self.parent_and_leaf(destination)?;
        let source_handle = open_relative(
            source_parent.handle.as_raw_handle().cast(),
            source_leaf,
            FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        ensure_regular_file_handle(source_handle.as_raw_handle())?;
        ensure_private_handle(source_handle.as_raw_handle())?;
        check_replace_destination(&destination_parent, destination_leaf, replace)?;
        let name = wide_component(destination_leaf)?;
        let header_size = offset_of!(FILE_RENAME_INFO, FileName);
        let total_size = header_size
            .checked_add(name.len().checked_mul(2).ok_or_else(invalid_name)?)
            .ok_or_else(invalid_name)?;
        let mut storage = vec![0_u64; total_size.div_ceil(size_of::<u64>())];
        let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `rename` points into aligned, sufficiently sized storage;
        // each field write stays within the documented FILE_RENAME_INFO
        // header and the UTF-16 payload is copied below.
        unsafe {
            // `FILE_RENAME_INFO_0` shares storage with a `Flags: u32` field
            // used by the FileRenameInformationEx native class; zero it first
            // so the bytes beyond the single `ReplaceIfExists` byte this
            // (non-Ex) class reads are deterministic.
            let mut anonymous = FILE_RENAME_INFO_0::default();
            anonymous.ReplaceIfExists = replace;
            ptr::addr_of_mut!((*rename).Anonymous).write(anonymous);
            ptr::addr_of_mut!((*rename).RootDirectory)
                .write(destination_parent.handle.as_raw_handle().cast());
            ptr::addr_of_mut!((*rename).FileNameLength)
                .write(u32::try_from(name.len() * 2).map_err(|_| invalid_name())?);
            ptr::copy_nonoverlapping(
                name.as_ptr(),
                ptr::addr_of_mut!((*rename).FileName).cast::<u16>(),
                name.len(),
            );
        }
        // The Win32 SetFileInformationByHandle wrapper does not reliably
        // honor a non-null RootDirectory (handle-relative rename) even
        // through FileRenameInfoEx; call NtSetInformationFile directly with
        // the native FileRenameInformation class instead, matching this
        // module's existing direct use of NtCreateFile for the same reason.
        let mut io_status = IoStatusBlock {
            value: IoStatusValue { status: 0 },
            information: 0,
        };
        // SAFETY: `source_handle` and the destination root handle remain
        // live, and `storage` contains a correctly-sized FILE_RENAME_INFO
        // buffer matching the native FILE_RENAME_INFORMATION layout.
        let status = unsafe {
            NtSetInformationFile(
                source_handle.as_raw_handle().cast(),
                &raw mut io_status,
                rename.cast(),
                u32::try_from(total_size).map_err(|_| invalid_name())?,
                FILE_RENAME_INFORMATION_CLASS,
            )
        };
        if status < STATUS_SUCCESS {
            // SAFETY: this converts the NTSTATUS returned by the preceding call.
            let error = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        flush_handle(destination_parent.handle.as_raw_handle())?;
        if !Arc::ptr_eq(&source_parent, &destination_parent) {
            flush_handle(source_parent.handle.as_raw_handle())?;
        }
        Ok(())
    }

    /// Unlinks one checked regular file by its opened handle, then flushes its
    /// parent directory. A concurrent name replacement cannot redirect it.
    pub fn remove_file_relative(&self, path: &[&str]) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = open_relative(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        ensure_regular_file_handle(handle.as_raw_handle())?;
        ensure_private_handle(handle.as_raw_handle())?;
        mark_delete(handle.as_raw_handle())?;
        flush_handle(parent.handle.as_raw_handle())
    }

    /// Checks every direct child through an open, reparse-point handle and
    /// rejects links, foreign owners, special files, and non-private DACLs.
    pub fn read_dir_checked(&self, path: &[&str]) -> io::Result<Vec<DirEntry>> {
        let directory = self.open_dir_checked(path)?;
        let names = enumerate_names(directory.handle())?;
        let mut entries = Vec::with_capacity(names.len());
        for (name, enumerated_file_id) in names {
            validate_component(&name)?;
            let handle = open_relative(
                directory.handle(),
                &name,
                FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
                0,
            )?;
            let attributes = attributes(handle.as_raw_handle())?;
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(invalid_data("workspace child is a reparse point"));
            }
            let standard = standard_info(handle.as_raw_handle())?;
            if standard.NumberOfLinks != 1 {
                return Err(invalid_data("workspace child has multiple hard links"));
            }
            if identity_info(handle.as_raw_handle())?.FileId.Identifier[..8]
                != enumerated_file_id.to_le_bytes()
            {
                return Err(invalid_data(
                    "workspace entry changed during directory enumeration",
                ));
            }
            ensure_private_handle(handle.as_raw_handle())?;
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

    /// Removes a private directory tree by recursively opening and deleting
    /// each child through pinned directory handles. Any unexpected entry or
    /// race fails closed; the directory itself is removed only when empty.
    pub fn remove_dir_tree(&self, path: &[&str]) -> io::Result<()> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let directory = match open_directory_child_with_delete(&parent, leaf) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        remove_tree_contents(&directory)?;
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

    fn parent_and_leaf<'a>(&self, path: &[&'a str]) -> io::Result<(Arc<DirectoryNode>, &'a str)> {
        let (leaf, parent_path) = path.split_last().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "relative path must contain a file name",
            )
        })?;
        validate_component(leaf)?;
        let mut parent = Arc::clone(&self.0);
        for part in parent_path {
            validate_component(part)?;
            parent = open_directory_child(&parent, part)?;
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
                validate_component(value)?;
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
    }))
}

fn open_directory_child(parent: &Arc<DirectoryNode>, name: &str) -> io::Result<Arc<DirectoryNode>> {
    let node = open_directory_child_writable(parent, name)?;
    ensure_private_handle(node.handle.as_raw_handle())?;
    Ok(node)
}

fn open_directory_child_unchecked(
    parent: &Arc<DirectoryNode>,
    name: &str,
) -> io::Result<Arc<DirectoryNode>> {
    let handle = open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
    }))
}

fn open_directory_child_writable(
    parent: &Arc<DirectoryNode>,
    name: &str,
) -> io::Result<Arc<DirectoryNode>> {
    let handle = open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        FILE_LIST_DIRECTORY
            | FILE_GENERIC_WRITE
            | FILE_TRAVERSE
            | FILE_READ_ATTRIBUTES
            | FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_DELETE_CHILD
            | READ_CONTROL
            | DELETE
            | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
    }))
}

fn open_directory_child_with_delete(
    parent: &Arc<DirectoryNode>,
    name: &str,
) -> io::Result<Arc<DirectoryNode>> {
    let handle = open_relative(
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
    )?;
    ensure_directory_handle(handle.as_raw_handle())?;
    ensure_private_handle(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
    }))
}

fn check_replace_destination(
    parent: &Arc<DirectoryNode>,
    name: &str,
    replace: bool,
) -> io::Result<()> {
    let existing = match open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE | SYNCHRONIZE,
        0,
    ) {
        Ok(handle) => handle,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !replace {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "rename destination already exists",
        ));
    }
    let attrs = attributes(existing.as_raw_handle())?;
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("refusing to replace a reparse point"));
    }
    let standard = standard_info(existing.as_raw_handle())?;
    if standard.Directory || attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid_data("refusing to replace a directory with a file"));
    }
    if standard.NumberOfLinks != 1 {
        return Err(invalid_data("refusing to replace a multiply linked file"));
    }
    ensure_private_handle(existing.as_raw_handle())
}

fn create_relative(parent: HANDLE, name: &str, access: u32, kind: u32) -> io::Result<OwnedHandle> {
    let name = wide_component(name)?;
    let security = private_security_descriptor()?;
    nt_create_relative(
        parent,
        &name,
        access,
        // Callers request DELETE in `access`; share it too so a concurrent
        // DELETE-requesting open elsewhere (every handle-relative open in
        // this module) isn't rejected while this handle is still live.
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_CREATE,
        kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        security.0,
    )
}

fn open_relative(parent: HANDLE, name: &str, access: u32, kind: u32) -> io::Result<OwnedHandle> {
    // Every caller of this function requests DELETE in `access`, so its
    // share mode must offer FILE_SHARE_DELETE too: Windows' sharing check is
    // symmetric, and an existing handle holding DELETE access (as every
    // concurrent handle here does) conflicts with a newcomer that doesn't
    // share it, regardless of what the newcomer itself requests.
    open_relative_with_share(
        parent,
        name,
        access,
        kind,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

fn open_relative_with_share(
    parent: HANDLE,
    name: &str,
    access: u32,
    kind: u32,
    share: u32,
) -> io::Result<OwnedHandle> {
    let name = wide_component(name)?;
    nt_create_relative(
        parent,
        &name,
        access,
        share,
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

/// A file just created or written can be briefly held open by antivirus
/// real-time scanning (observed with Windows Defender), which fails an
/// immediately-following open for `DELETE` access with
/// `STATUS_SHARING_VIOLATION`. Retry a bounded number of times with a short
/// backoff instead of failing closed on a transient external lock.
const STATUS_SHARING_VIOLATION: i32 = 0xC000_0043_u32 as i32;
const SHARING_VIOLATION_RETRIES: u32 = 20;
const SHARING_VIOLATION_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

fn nt_create(
    attributes: &mut ObjectAttributes,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let mut attempt = 0_u32;
    loop {
        let mut raw: HANDLE = ptr::null_mut();
        let mut status_block = IoStatusBlock {
            value: IoStatusValue { status: 0 },
            information: 0,
        };
        // SAFETY: object attributes and Unicode storage are live for the
        // call; output handles and I/O status are valid stack
        // out-parameters. NTSTATUS is tested by its signed success rule, not
        // by comparing to zero only.
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
        if status == STATUS_SHARING_VIOLATION && attempt < SHARING_VIOLATION_RETRIES {
            attempt += 1;
            std::thread::sleep(SHARING_VIOLATION_RETRY_DELAY);
            continue;
        }
        if status < STATUS_SUCCESS {
            // SAFETY: this converts the NTSTATUS returned by the preceding call.
            let error = unsafe { RtlNtStatusToDosError(status) };
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        if raw.is_null() || raw as isize == -1 {
            return Err(invalid_data("NtCreateFile returned an invalid handle"));
        }
        // SAFETY: successful NtCreateFile transfers one owning HANDLE to us.
        return Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) });
    }
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

fn private_security_descriptor() -> io::Result<PrivateSecurityDescriptor> {
    let sid = current_user()?;
    let bytes = sid.as_bytes();
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
    let mut sid_text = format!("S-{}-{identifier_authority}", bytes[0]);
    for index in 0..sub_authority_count {
        let offset = 8 + index * 4;
        let sub_authority = u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| invalid_data("current-user SID is malformed"))?,
        );
        sid_text.push('-');
        sid_text.push_str(&sub_authority.to_string());
    }
    // `P` blocks inherited grants and the sole `GA` ACE names the current
    // token user. This descriptor is supplied to NtCreateFile, so the object
    // never appears under a broader inherited ACL, even briefly.
    let sddl = format!("D:P(A;;GA;;;{sid_text})");
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
    let attributes = attributes(handle)?;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("workspace file is a reparse point"));
    }
    let standard = standard_info(handle)?;
    if standard.Directory || attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err(invalid_data("workspace object is not a regular file"));
    }
    if standard.NumberOfLinks != 1 {
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
            ACL_INFORMATION_CLASS_SIZE,
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
    // `restrict_handle_to_current_user` grants GENERIC_ALL, but SetSecurityInfo
    // maps generic rights to the object type's specific rights before storing
    // the ACE, so the persisted mask reads back as FILE_ALL_ACCESS, not the
    // raw generic bit.
    if u32::from(allowed.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || allowed.Mask != FILE_ALL_ACCESS
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

fn enumerate_names(handle: HANDLE) -> io::Result<Vec<(String, i64)>> {
    let mut buffer = vec![0_u64; 8192];
    let mut restart = true;
    let mut names = Vec::new();
    loop {
        let class: FILE_INFO_BY_HANDLE_CLASS = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
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
        let bytes = buffer.len() * size_of::<u64>();
        let mut offset = 0usize;
        loop {
            if offset % align_of::<FILE_ID_BOTH_DIR_INFO>() != 0
                || offset
                    .checked_add(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName))
                    .is_none_or(|end| end > bytes)
            {
                return Err(invalid_data("Windows returned a malformed directory entry"));
            }
            // SAFETY: offset bounds are checked and each record is returned by
            // GetFileInformationByHandleEx in this aligned buffer.
            let entry = unsafe {
                &*buffer
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_ID_BOTH_DIR_INFO>()
            };
            let name_bytes = entry.FileNameLength as usize;
            let name_offset = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            let end = offset
                .checked_add(name_offset)
                .and_then(|start| start.checked_add(name_bytes))
                .ok_or_else(|| invalid_data("Windows returned an oversized directory name"))?;
            if name_bytes % 2 != 0
                || end > bytes
                || (buffer.as_ptr() as usize + offset + name_offset) % align_of::<u16>() != 0
            {
                return Err(invalid_data(
                    "Windows returned an invalid UTF-16 directory name",
                ));
            }
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
                names.push((name, entry.FileId));
            }
            if entry.NextEntryOffset == 0 {
                break;
            }
            let next = usize::try_from(entry.NextEntryOffset)
                .map_err(|_| invalid_data("invalid directory offset"))?;
            if next == 0
                || next % align_of::<FILE_ID_BOTH_DIR_INFO>() != 0
                || offset.checked_add(next).is_none_or(|end| end >= bytes)
            {
                return Err(invalid_data("Windows returned an invalid directory offset"));
            }
            offset += next;
        }
    }
    Ok(names)
}

fn remove_tree_contents(directory: &Arc<DirectoryNode>) -> io::Result<()> {
    let names = enumerate_names(directory.handle.as_raw_handle().cast())?;
    for (name, enumerated_file_id) in names {
        validate_component(&name)?;
        let handle = open_relative(
            directory.handle.as_raw_handle().cast(),
            &name,
            FILE_GENERIC_WRITE
                | FILE_READ_ATTRIBUTES
                | READ_CONTROL
                | DELETE
                | FILE_LIST_DIRECTORY
                | SYNCHRONIZE,
            0,
        )?;
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
        if identity_info(handle.as_raw_handle())?.FileId.Identifier[..8]
            != enumerated_file_id.to_le_bytes()
        {
            return Err(invalid_data(
                "workspace entry changed during directory enumeration",
            ));
        }
        ensure_private_handle(handle.as_raw_handle())?;
        if standard.Directory {
            let child = Arc::new(DirectoryNode {
                handle,
                _parent: Some(Arc::clone(directory)),
            });
            remove_tree_contents(&child)?;
            mark_delete(child.handle.as_raw_handle())?;
        } else {
            mark_delete(handle.as_raw_handle())?;
        }
    }
    flush_handle(directory.handle.as_raw_handle())
}

fn mark_delete(handle: *mut c_void) -> io::Result<()> {
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

fn wide_component(component: &str) -> io::Result<Vec<u16>> {
    validate_component(component)?;
    let wide = component.encode_utf16().collect::<Vec<_>>();
    if wide.len() > 255 {
        return Err(invalid_name());
    }
    Ok(wide)
}

fn validate_component(component: &str) -> io::Result<()> {
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
        || component.encode_utf16().count() > 255
    {
        return Err(invalid_name());
    }
    Ok(())
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

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::{EntryKind, WorkspaceRoot};
    use std::fs;
    use std::io::{Read as _, Write as _};
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
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) =>
            {
                false
            }
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
        assert_eq!(
            fs::read(&published).expect("read published file"),
            b"workspace bytes"
        );
        drop(root);
        for candidate in [&source, &parked] {
            if candidate.exists() {
                let _ = fs::remove_file(candidate);
            }
        }
        fs::remove_dir_all(path).expect("remove workspace root");
        fs::remove_file(outside).expect("remove outside victim");
    }
}
