//! Private, crash-safe state files on Windows.
#![allow(
    unsafe_code,
    reason = "handle-based file deletion and protected DACL changes are the reviewed Win32 boundary"
)]

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Publishes a private state file. `parent` must already exist.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent(path);
    let root = super::workspace_fs::WorkspaceRoot::open(parent)?;
    let file_name = component(path.file_name())?;

    loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(".{file_name}.{}.{}.tmp", std::process::id(), sequence);
        let mut file = match root.create_file_exclusive(&[&temporary]) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = root.remove_file_relative(&[&temporary]);
            return Err(error);
        }
        if let Err(error) = root.rename_relative(&[&temporary], &[file_name], true) {
            let _ = root.remove_file_relative(&[&temporary]);
            return Err(error);
        }
        return Ok(());
    }
}

/// Opens a private state file after checking its opened object's ACL and type.
pub(crate) fn open_private_read(path: &Path) -> io::Result<File> {
    let parent = parent(path);
    let root = super::workspace_fs::WorkspaceRoot::open(parent)?;
    root.open_file_read_checked(&[component(path.file_name())?])
}

/// Deletes a checked private state file through its handle and flushes parent.
pub(crate) fn remove_private(path: &Path) -> io::Result<()> {
    let parent = parent(path);
    let root = super::workspace_fs::WorkspaceRoot::open(parent)?;
    root.remove_file_relative(&[component(path.file_name())?])?;
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn component(path: Option<&OsStr>) -> io::Result<&str> {
    path.and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private Windows state paths need Unicode file names",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win32::identity::{
        LocalAllocation, current_user, is_owned_by_current_user, owner_of,
    };
    use crate::win32::security::{restrict_handle_to_current_user, restrict_to_current_user};
    use std::ffi::OsString;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, GENERIC_READ};
    use windows_sys::Win32::Security::Authorization::{
        DENY_ACCESS, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
        SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
        TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
    };

    fn fixture(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!("bpp-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create private state fixture");
        restrict_to_current_user(&path).expect("protect fixture directory");
        path
    }

    #[test]
    fn private_state_survives_close_and_reopen_with_current_user_owner() {
        let directory = fixture("reopen");
        let path = directory.join("journal.bin");
        write_private_atomic(&path, b"durable journal bytes").expect("publish journal");
        let first = open_private_read(&path).expect("open journal");
        assert!(
            is_owned_by_current_user(&owner_of(&first).expect("journal owner"))
                .expect("check owner")
        );
        drop(first);

        let mut reopened = open_private_read(&path).expect("cold reopen journal");
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut reopened, &mut bytes).expect("read journal");
        assert_eq!(bytes, b"durable journal bytes");
        drop(reopened);

        write_private_atomic(&path, b"replacement journal after atomic rename")
            .expect("atomically replace journal");
        let mut reopened = open_private_read(&path).expect("cold reopen replacement");
        bytes.clear();
        io::Read::read_to_end(&mut reopened, &mut bytes).expect("read replacement");
        assert_eq!(bytes, b"replacement journal after atomic rename");

        remove_private(&path).expect("durably remove journal");
        assert!(!path.exists());
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn private_journal_directory_is_created_and_cold_reopened() {
        let parent = fixture("private-dir");
        let directory = parent.join("owner-acks");
        crate::durable::ensure_private_directory(&directory)
            .expect("create private owner-ack directory");
        crate::durable::ensure_private_directory(&directory)
            .expect("verify cold-reopened owner-ack directory");

        let journal = directory.join("record.bin");
        write_private_atomic(&journal, b"durable owner acknowledgement")
            .expect("publish journal record");
        let mut reopened = open_private_read(&journal).expect("cold reopen journal record");
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut reopened, &mut bytes).expect("read journal record");
        assert_eq!(bytes, b"durable owner acknowledgement");
        drop(reopened);

        remove_private(&journal).expect("durably remove journal record");
        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn app_data_child_is_protected_and_existing_unprotected_child_is_rejected() {
        let parent = fixture("app-data-child");
        let private = parent.join("Nudox");
        crate::durable::ensure_private_child_directory(&private)
            .expect("create owner-only app-data child");
        crate::durable::ensure_private_child_directory(&private)
            .expect("cold reopen owner-only app-data child");

        let unprotected = parent.join("unprotected");
        fs::create_dir(&unprotected).expect("create inherited-DACL child");
        assert!(crate::durable::ensure_private_child_directory(&unprotected).is_err());

        fs::remove_dir_all(parent).expect("remove fixture");
    }

    #[test]
    fn replacement_failure_preserves_the_old_file_and_cleans_the_temporary() {
        let directory = fixture("boundary");
        let path = directory.join("blocked");
        fs::create_dir(&path).expect("create replacement obstacle");
        let error =
            write_private_atomic(&path, b"new state").expect_err("directory is not replaceable");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        let names = fs::read_dir(&directory)
            .expect("read fixture")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("blocked")]);
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn cold_reopen_after_synced_temporary_before_rename_keeps_old_record() {
        let directory = fixture("pre-rename-crash");
        let path = directory.join("journal.bin");
        write_private_atomic(&path, b"last committed record").expect("publish old record");

        // Model process death after the replacement contents reached the file
        // but before its same-directory rename published the new record.
        let root = super::super::workspace_fs::WorkspaceRoot::open(&directory)
            .expect("pin journal directory");
        let mut temporary = root
            .create_file_exclusive(&[".journal.bin.simulated-crash.tmp"])
            .expect("create replacement temporary");
        temporary
            .write_all(b"uncommitted replacement")
            .expect("write replacement temporary");
        temporary.sync_all().expect("flush replacement contents");
        drop(temporary);
        drop(root);

        let mut reopened = open_private_read(&path).expect("cold reopen committed record");
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut reopened, &mut bytes).expect("read committed record");
        assert_eq!(bytes, b"last committed record");
        drop(reopened);

        let root = super::super::workspace_fs::WorkspaceRoot::open(&directory)
            .expect("reopen journal directory for orphan cleanup");
        root.remove_file_relative(&[".journal.bin.simulated-crash.tmp"])
            .expect("remove unpublished temporary");
        drop(root);
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn final_reparse_point_is_rejected_without_opening_its_target() {
        let directory = fixture("reparse");
        let target = directory.join("target");
        fs::write(&target, b"target").expect("create target");
        let link = directory.join("link");
        let listener =
            crate::win32::socket::LocalListener::bind(&link).expect("create reparse endpoint");
        assert!(open_private_read(&link).is_err());
        assert!(crate::durable::ensure_private_directory(&link).is_err());
        assert!(write_private_atomic(&link, b"must not replace the endpoint").is_err());
        assert!(write_private_atomic(&link.join("nested.bin"), b"state").is_err());
        assert!(link.exists(), "the reparse endpoint itself was preserved");
        drop(listener);
        fs::remove_file(&link).expect("remove endpoint");
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    #[test]
    fn a_parent_that_denies_dacl_changes_fails_closed_before_journal_creation() {
        let directory = fixture("permission");
        let handle = OpenOptions::new()
            .read(true)
            .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&directory)
            .expect("open directory before applying denying ACL");
        set_dacl_change_denied(&handle).expect("install denying ACL");

        let path = directory.join("journal.bin");
        let result = write_private_atomic(&path, b"must never be written");
        restrict_handle_to_current_user(&handle).expect("restore private ACL");
        let error = result.expect_err("a parent without WRITE_DAC cannot be made private");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(&directory)
                .expect("read restored fixture")
                .count(),
            0,
            "permission failure created no visible journal temporary"
        );
        drop(handle);
        fs::remove_dir_all(directory).expect("remove fixture");
    }

    fn set_dacl_change_denied(handle: &File) -> io::Result<()> {
        let mut user_sid = current_user()?.as_bytes().to_vec();
        // OWNER RIGHTS (S-1-3-4) suppresses the owner's implicit WRITE_DAC
        // grant, allowing this test to exercise a real Windows access denial.
        let mut owner_rights_sid = [1_u8, 1, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0];
        let entries = [
            EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_READ_ATTRIBUTES | READ_CONTROL,
                grfAccessMode: SET_ACCESS,
                grfInheritance: 0,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_USER,
                    ptstrName: user_sid.as_mut_ptr().cast(),
                },
            },
            EXPLICIT_ACCESS_W {
                grfAccessPermissions: WRITE_DAC,
                grfAccessMode: DENY_ACCESS,
                grfInheritance: 0,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                    ptstrName: owner_rights_sid.as_mut_ptr().cast(),
                },
            },
        ];
        let mut acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: two initialised entries are passed with their exact count;
        // both SID buffers outlive the call, and `acl` is an output pointer.
        let status =
            unsafe { SetEntriesInAclW(2, entries.as_ptr(), std::ptr::null(), &raw mut acl) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.cast_signed()));
        }
        let acl = LocalAllocation(acl.cast());
        // SAFETY: `handle` was opened with WRITE_DAC and `acl` remains
        // allocated until the synchronous call returns.
        let status = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.0.cast_const().cast(),
                std::ptr::null(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.cast_signed()));
        }
        Ok(())
    }
}
