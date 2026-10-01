//! Handle-relative access to ordinary Windows project trees.
//!
//! Unlike `workspace_fs`, this API does not require private ACLs: source trees
//! are commonly shared with the current user or inherited from a parent
//! directory. It still pins each ancestor handle and rejects reparse points,
//! special files, and hard-link aliases before exposing a source file.
#![allow(
    unsafe_code,
    reason = "reviewed NT handle-relative traversal and file revision boundary for Windows project trees"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle as _, IntoRawHandle as _, OwnedHandle};
use std::path::{Component, Path, Prefix};
use std::ptr;
use std::sync::Arc;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE,
    FileAttributeTagInfo, FileBasicInfo, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx, SYNCHRONIZE,
};

const STATUS_SUCCESS: i32 = 0;
const FILE_OPEN: u32 = 1;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

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

#[repr(C)]
struct FileAttributeTagInfo {
    file_attributes: u32,
    reparse_tag: u32,
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
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

struct DirectoryNode {
    handle: OwnedHandle,
    _parent: Option<Arc<DirectoryNode>>,
}

/// A stable revision read from one opened Windows file or directory handle.
/// `change_time` is the NTFS change time, distinct from the user-settable last
/// write time. Filesystems that cannot provide a stable file identity and
/// change time are rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileRevision {
    /// Byte length reported by the opened object.
    pub length: u64,
    /// The volume serial number from `FILE_ID_INFO`.
    pub volume_serial_number: u64,
    /// The persistent file identifier from `FILE_ID_INFO`.
    pub file_id: [u8; 16],
    /// NT `FILE_BASIC_INFO.ChangeTime`, in 100 ns ticks from 1601-01-01 UTC.
    pub change_time: i64,
    /// NT `FILE_BASIC_INFO.LastWriteTime`, in 100 ns ticks from 1601-01-01 UTC.
    pub last_write_time: i64,
    /// Whether the opened object is a directory.
    pub is_directory: bool,
    /// Link count reported by the pinned object; only single-link entries are
    /// admitted into a project workspace.
    pub number_of_links: u32,
}

/// A pinned project directory. Ancestor handles remain open so relative opens
/// cannot be redirected by renaming a parent or inserting a junction.
#[derive(Clone)]
pub struct ProjectRoot(Arc<DirectoryNode>);

impl ProjectRoot {
    /// Opens an absolute local-drive directory without following reparse
    /// points in any component. UNC, device, relative, and ambiguous paths fail.
    pub fn open(path: &Path) -> io::Result<Self> {
        let (drive_root, parts) = absolute_drive_components(path)?;
        let mut current = open_drive_root(&drive_root)?;
        if parts.is_empty() {
            return Err(invalid_name("project root cannot be the drive root"));
        }
        for part in &parts {
            current = open_directory_child(&current, part)?;
        }
        Ok(Self(current))
    }

    /// Returns the revision of this pinned directory handle.
    pub fn revision(&self) -> io::Result<FileRevision> {
        revision_for_handle(self.0.handle.as_raw_handle())
    }

    /// Opens and pins a descendant directory, validating every path component.
    pub fn open_dir(&self, path: &[&str]) -> io::Result<Self> {
        let mut current = Arc::clone(&self.0);
        for component in path {
            validate_component(component)?;
            current = open_directory_child(&current, component)?;
        }
        Ok(Self(current))
    }

    /// Returns the revision of an existing regular file or directory beneath
    /// this root. Every intermediate component is opened relative to a pinned
    /// handle and every reparse point or hard-link alias fails closed.
    pub fn revision_relative(&self, path: &[&str]) -> io::Result<FileRevision> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = open_relative(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            0,
        )?;
        validate_open_object(handle.as_raw_handle())?;
        revision_for_handle(handle.as_raw_handle())
    }

    /// Opens one regular file beneath this root without following reparse
    /// points, and rejects files with more than one hard link.
    pub fn open_file_read(&self, path: &[&str]) -> io::Result<File> {
        let (parent, leaf) = self.parent_and_leaf(path)?;
        let handle = open_relative(
            parent.handle.as_raw_handle().cast(),
            leaf,
            FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_NON_DIRECTORY_FILE,
        )?;
        validate_regular_file(handle.as_raw_handle())?;
        revision_for_handle(handle.as_raw_handle())?;
        file_from_handle(handle)
    }

    fn parent_and_leaf(&self, path: &[&str]) -> io::Result<(Arc<DirectoryNode>, &str)> {
        let (leaf, parents) = path
            .split_last()
            .ok_or_else(|| invalid_name("relative path must contain at least one component"))?;
        validate_component(leaf)?;
        let mut current = Arc::clone(&self.0);
        for parent in parents {
            validate_component(parent)?;
            current = open_directory_child(&current, parent)?;
        }
        Ok((current, leaf))
    }
}

/// Reads revision metadata from an already-opened handle. This lets a caller
/// compare the same pinned file both before and after streaming its contents.
pub fn revision_for_file(file: &File) -> io::Result<FileRevision> {
    revision_for_handle(file.as_raw_handle())
}

fn absolute_drive_components(path: &Path) -> io::Result<(Vec<u16>, Vec<String>)> {
    let mut components = path.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix)) => prefix,
        _ => return Err(invalid_name("project path must be absolute")),
    };
    let drive = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
        _ => return Err(invalid_name("only local drive project paths are supported")),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(invalid_name("project path must be rooted"));
    }
    let mut parts = Vec::new();
    for component in components {
        let Component::Normal(value) = component else {
            return Err(invalid_name("project path contains a non-normal component"));
        };
        let value = value
            .to_str()
            .ok_or_else(|| invalid_name("project path component is not valid UTF-8"))?;
        validate_component(value)?;
        parts.push(value.to_owned());
    }
    let drive_root = format!("\\??\\{}:\\", char::from(drive))
        .encode_utf16()
        .collect();
    Ok((drive_root, parts))
}

fn open_drive_root(name: &[u16]) -> io::Result<Arc<DirectoryNode>> {
    let handle = nt_open_absolute(
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN_REPARSE_POINT | FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
    )?;
    validate_directory(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: None,
    }))
}

fn open_directory_child(parent: &Arc<DirectoryNode>, name: &str) -> io::Result<Arc<DirectoryNode>> {
    let handle = open_relative(
        parent.handle.as_raw_handle().cast(),
        name,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    )?;
    validate_directory(handle.as_raw_handle())?;
    Ok(Arc::new(DirectoryNode {
        handle,
        _parent: Some(Arc::clone(parent)),
    }))
}

fn validate_open_object(handle: *mut c_void) -> io::Result<()> {
    let attrs = attributes(handle)?;
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid_data("project path contains a reparse point"));
    }
    let standard = standard_info(handle)?;
    if standard.DeletePending {
        return Err(invalid_data("project object is pending deletion"));
    }
    if standard.Directory {
        if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 || standard.NumberOfLinks != 1 {
            return Err(invalid_data(
                "project directory has an alias or invalid type",
            ));
        }
    } else if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 || standard.NumberOfLinks != 1 {
        return Err(invalid_data("project file has an alias or invalid type"));
    }
    Ok(())
}

fn validate_directory(handle: *mut c_void) -> io::Result<()> {
    validate_open_object(handle)?;
    if !standard_info(handle)?.Directory {
        return Err(invalid_data("project path component is not a directory"));
    }
    Ok(())
}

fn validate_regular_file(handle: *mut c_void) -> io::Result<()> {
    validate_open_object(handle)?;
    if standard_info(handle)?.Directory {
        return Err(invalid_data("project path is not a regular file"));
    }
    Ok(())
}

fn revision_for_handle(handle: *mut c_void) -> io::Result<FileRevision> {
    let standard = standard_info(handle)?;
    let identity = identity_info(handle)?;
    let basic = basic_info(handle)?;
    if standard.EndOfFile < 0
        || standard.NumberOfLinks != 1
        || identity.VolumeSerialNumber == 0
        || identity.FileId.Identifier.iter().all(|byte| *byte == 0)
        || basic.ChangeTime == 0
    {
        return Err(invalid_data(
            "filesystem does not provide a stable project revision",
        ));
    }
    Ok(FileRevision {
        length: standard.EndOfFile as u64,
        volume_serial_number: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
        change_time: basic.ChangeTime,
        last_write_time: basic.LastWriteTime,
        is_directory: standard.Directory,
        number_of_links: standard.NumberOfLinks,
    })
}

fn open_relative(parent: HANDLE, name: &str, access: u32, kind: u32) -> io::Result<OwnedHandle> {
    let mut wide = wide_component(name)?;
    let mut unicode = unicode_string(&mut wide)?;
    let mut attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: parent,
        object_name: &raw mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: ptr::null_mut(),
        security_qos: ptr::null_mut(),
    };
    nt_create(
        &mut attributes,
        access,
        kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
}

fn nt_open_absolute(name: &[u16], access: u32, options: u32) -> io::Result<OwnedHandle> {
    let mut wide = name.to_vec();
    let mut unicode = unicode_string(&mut wide)?;
    let mut attributes = ObjectAttributes {
        length: mem::size_of::<ObjectAttributes>() as u32,
        root_directory: ptr::null_mut(),
        object_name: &raw mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: ptr::null_mut(),
        security_qos: ptr::null_mut(),
    };
    nt_create(&mut attributes, access, options)
}

fn nt_create(
    attributes: &mut ObjectAttributes,
    access: u32,
    options: u32,
) -> io::Result<OwnedHandle> {
    let mut raw: HANDLE = ptr::null_mut();
    let mut status_block = IoStatusBlock {
        value: IoStatusValue { status: 0 },
        information: 0,
    };
    // SAFETY: object attributes, Unicode storage, output handles, and the I/O
    // status block remain valid for this synchronous system call.
    let status = unsafe {
        NtCreateFile(
            &raw mut raw,
            access,
            attributes,
            &raw mut status_block,
            ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            options,
            ptr::null_mut(),
            0,
        )
    };
    if status < STATUS_SUCCESS {
        // SAFETY: this converts the NTSTATUS produced by the preceding call.
        let error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    if raw.is_null() || raw as isize == -1 {
        return Err(invalid_data("NtCreateFile returned an invalid handle"));
    }
    // SAFETY: a successful NtCreateFile transfers one owning HANDLE to us.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
}

fn attributes(handle: *mut c_void) -> io::Result<u32> {
    let mut info = FileAttributeTagInfo {
        file_attributes: 0,
        reparse_tag: 0,
    };
    query_info(
        handle,
        FileAttributeTagInfo,
        (&raw mut info).cast(),
        mem::size_of::<FileAttributeTagInfo>(),
    )?;
    Ok(info.file_attributes)
}

fn standard_info(handle: *mut c_void) -> io::Result<FILE_STANDARD_INFO> {
    let mut info = FILE_STANDARD_INFO::default();
    query_info(
        handle,
        FileStandardInfo,
        (&raw mut info).cast(),
        mem::size_of::<FILE_STANDARD_INFO>(),
    )?;
    Ok(info)
}

fn identity_info(handle: *mut c_void) -> io::Result<FILE_ID_INFO> {
    let mut info = FILE_ID_INFO::default();
    query_info(
        handle,
        FileIdInfo,
        (&raw mut info).cast(),
        mem::size_of::<FILE_ID_INFO>(),
    )?;
    Ok(info)
}

fn basic_info(handle: *mut c_void) -> io::Result<FILE_BASIC_INFO> {
    let mut info = FILE_BASIC_INFO::default();
    query_info(
        handle,
        FileBasicInfo,
        (&raw mut info).cast(),
        mem::size_of::<FILE_BASIC_INFO>(),
    )?;
    Ok(info)
}

fn query_info(
    handle: *mut c_void,
    class: FILE_INFO_BY_HANDLE_CLASS,
    buffer: *mut c_void,
    size: usize,
) -> io::Result<()> {
    // SAFETY: handle is owned by the caller and buffer points to the exact
    // writable structure corresponding to `class` for `size` bytes.
    let read = unsafe { GetFileInformationByHandleEx(handle.cast(), class, buffer, size as u32) };
    if read == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wide_component(component: &str) -> io::Result<Vec<u16>> {
    validate_component(component)?;
    let value = component.encode_utf16().collect::<Vec<_>>();
    if value
        .len()
        .checked_mul(2)
        .is_none_or(|bytes| bytes > u16::MAX as usize)
    {
        return Err(invalid_name("project path component is too long"));
    }
    Ok(value)
}

fn unicode_string(buffer: &mut [u16]) -> io::Result<UnicodeString> {
    let bytes = buffer
        .len()
        .checked_mul(2)
        .filter(|bytes| *bytes <= u16::MAX as usize)
        .ok_or_else(|| invalid_name("project path component is too long"))?;
    Ok(UnicodeString {
        length: bytes as u16,
        maximum_length: bytes as u16,
        buffer: buffer.as_mut_ptr(),
    })
}

fn validate_component(component: &str) -> io::Result<()> {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.contains(['/', '\\', '\0', ':', '*', '?', '<', '>', '|', '"'])
        || component.ends_with(' ')
        || component.ends_with('.')
        || component.chars().any(char::is_control)
    {
        return Err(invalid_name("project path component is invalid"));
    }
    Ok(())
}

fn file_from_handle(handle: OwnedHandle) -> io::Result<File> {
    let raw = handle.into_raw_handle();
    // SAFETY: the OwnedHandle transfers exactly one live handle to File.
    Ok(unsafe { File::from_raw_handle(raw) })
}

fn invalid_name(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::ProjectRoot;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn fixture(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("project-fs-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create project fixture");
        path
    }

    #[test]
    fn ordinary_project_file_has_stable_identity_and_change_time() {
        let directory = fixture("revision");
        fs::write(directory.join("source.rs"), b"before").expect("write fixture source");
        let root = ProjectRoot::open(&directory).expect("open project root");
        let before = root
            .revision_relative(&["source.rs"])
            .expect("read project revision");
        let after = root
            .revision_relative(&["source.rs"])
            .expect("read stable revision");
        assert_eq!(before, after);
        assert_ne!(before.volume_serial_number, 0);
        assert_ne!(before.file_id, [0; 16]);
        assert_ne!(before.change_time, 0);
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn project_root_rejects_junction_ancestors() {
        let parent = fixture("junction");
        let target = parent.join("target");
        let junction = parent.join("junction");
        fs::create_dir(&target).expect("create junction target");
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .expect("run junction fixture command");
        assert!(output.status.success(), "mklink /J failed: {output:?}");
        assert!(ProjectRoot::open(&junction).is_err());
        fs::remove_dir_all(parent).expect("remove project fixture");
    }
}
