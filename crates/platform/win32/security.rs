//! Owner-only access for files and AF_UNIX socket paths, the Windows counterpart of mode `0600`.
//! The replacement DACL is protected, so nothing inherited from a shared project directory survives.
//! Endpoint classification lives here too, because a Windows AF_UNIX socket is a reparse point rather than a file type.
#![allow(
    unsafe_code,
    reason = "building and applying a protected DACL is the reviewed access-control boundary"
)]

use super::identity::{LocalAllocation, current_user};
use std::fs::{Metadata, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::ptr;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_ALL};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
    SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    READ_CONTROL, WRITE_DAC,
};

/// Replaces the DACL of `path` with one entry granting full access to the
/// current user, and blocks inheritance from the parent directory.
///
/// The path is opened without following a reparse point, so a link planted at
/// `path` is restricted itself rather than redirecting the change elsewhere.
///
/// # Errors
/// Returns an error when the path cannot be opened for `WRITE_DAC` or the
/// security descriptor cannot be replaced.
pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let user = current_user()?;
    let mut sid = user.as_bytes().to_vec();
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            // With `TRUSTEE_IS_SID` this field carries a SID pointer, typed as
            // a wide string only because the structure is a union in C.
            ptstrName: sid.as_mut_ptr().cast(),
        },
    };
    let mut acl: *mut ACL = ptr::null_mut();
    // SAFETY: one initialised entry is passed with its count, no existing ACL
    // is merged, and `acl` is a stack out-pointer. `sid` outlives the call.
    let status = unsafe { SetEntriesInAclW(1, &raw const entry, ptr::null(), &raw mut acl) };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let acl = LocalAllocation(acl.cast());
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: `file` is open with `WRITE_DAC`; `acl` is the ACL built above
    // and stays allocated until it drops after this call. Owner, group, and
    // SACL are left unchanged by passing null and omitting their flags.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl.0.cast_const().cast(),
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    Ok(())
}

/// Returns whether `metadata`, read with `symlink_metadata`, describes an
/// AF_UNIX endpoint: a reparse point that is not a symbolic link.
///
/// This is the Windows counterpart of `FileTypeExt::is_socket`. It cannot
/// tell a socket from another non-link reparse point; callers confirm a live
/// endpoint by connecting to it.
#[must_use]
pub fn is_endpoint_metadata(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && !metadata.file_type().is_symlink()
}
