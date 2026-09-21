//! Same-user identity for Windows peers and files, expressed as security identifiers.
//! A peer's user comes from its process token, found through the AF_UNIX peer process ID the kernel reports.
//! A file's owner comes from its security descriptor, read through a handle that never follows a reparse point.
#![allow(
    unsafe_code,
    reason = "Win32 token, socket ioctl, and security descriptor calls are the reviewed identity boundary"
)]

use super::socket::LocalStream;
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::{AsRawHandle, AsRawSocket as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Networking::WinSock::{
    SIO_AF_UNIX_GETPEERPID, SOCKET, SOCKET_ERROR, WSAIoctl,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, TOKEN_INFORMATION_CLASS, TOKEN_OWNER,
    TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Largest encoded security identifier Windows can produce.
const MAX_SID_BYTES: usize = SECURITY_MAX_SID_SIZE as usize;

/// Largest token information buffer this module will allocate. A `TOKEN_USER`
/// or `TOKEN_OWNER` is one pointer-bearing header plus one SID, far below this.
const MAX_TOKEN_INFORMATION_BYTES: u32 = 4096;

/// One security identifier, copied out of whatever Win32 buffer produced it.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct UserSid {
    bytes: [u8; MAX_SID_BYTES],
    len: usize,
}

impl UserSid {
    /// Returns the SID's binary encoding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Copies a SID out of a Win32-owned buffer.
    ///
    /// # Safety
    /// `sid` must be null or point to readable memory holding a SID, and that
    /// memory must stay valid for the duration of this call.
    unsafe fn copy_from(sid: PSID) -> io::Result<Self> {
        // SAFETY: the caller guarantees `sid` is null or readable; `IsValidSid`
        // only reads the structure it is given.
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an invalid security identifier",
            ));
        }
        // SAFETY: `sid` was validated above.
        let len = unsafe { GetLengthSid(sid) } as usize;
        if len == 0 || len > MAX_SID_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an oversized security identifier",
            ));
        }
        let mut bytes = [0_u8; MAX_SID_BYTES];
        // SAFETY: a valid SID of `len` bytes starts at `sid`, and `bytes` has
        // room for `MAX_SID_BYTES >= len`. The regions cannot overlap.
        unsafe { ptr::copy_nonoverlapping(sid.cast::<u8>(), bytes.as_mut_ptr(), len) };
        Ok(Self { bytes, len })
    }
}

impl fmt::Debug for UserSid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UserSid(")?;
        for byte in self.as_bytes() {
            write!(formatter, "{byte:02x}")?;
        }
        formatter.write_str(")")
    }
}

/// Returns the user this process runs as.
///
/// # Errors
/// Returns an error when the process token cannot be read.
pub fn current_user() -> io::Result<UserSid> {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid
    // for this process and must not be closed.
    token_sid(unsafe { GetCurrentProcess() }, TokenUser)
}

/// Returns the default owner this process assigns to new objects.
///
/// For an elevated administrator this is the Administrators group rather than
/// the user, so a file this process created legitimately carries that owner.
///
/// # Errors
/// Returns an error when the process token cannot be read.
pub fn current_default_owner() -> io::Result<UserSid> {
    // SAFETY: as in `current_user`.
    token_sid(unsafe { GetCurrentProcess() }, TokenOwner)
}

/// Returns whether `owner` is an owner this process could have assigned: its
/// user, or its token's default owner.
///
/// # Errors
/// Returns an error when the process token cannot be read.
pub fn is_owned_by_current_user(owner: &UserSid) -> io::Result<bool> {
    Ok(*owner == current_user()? || *owner == current_default_owner()?)
}

/// Returns the process ID of the other end of a connected AF_UNIX stream.
///
/// # Errors
/// Returns an error when the socket is not a connected AF_UNIX stream.
pub fn peer_process_id(stream: &LocalStream) -> io::Result<u32> {
    let socket = SOCKET::try_from(stream.as_raw_socket())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket handle out of range"))?;
    let mut process_id = 0_u32;
    let mut returned = 0_u32;
    // SAFETY: `socket` is a live socket owned by `stream` for the duration of
    // the call. The output buffer is a `u32` on this stack frame with its exact
    // size passed, no input buffer is used, and the call is synchronous (no
    // overlapped structure or completion routine).
    let status = unsafe {
        WSAIoctl(
            socket,
            SIO_AF_UNIX_GETPEERPID,
            ptr::null(),
            0,
            (&raw mut process_id).cast(),
            std::mem::size_of::<u32>() as u32,
            &raw mut returned,
            ptr::null_mut(),
            None,
        )
    };
    if status == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    // Windows fills the output buffer for this ioctl but leaves the returned
    // byte count at zero, so a nonzero process ID is the only usable proof.
    if process_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "AF_UNIX peer process ID was not reported ({returned} bytes, process {process_id})"
            ),
        ));
    }
    Ok(process_id)
}

/// Returns the user the process on the other end of `stream` runs as.
///
/// The peer is identified by process ID, then its token is read. A process ID
/// can in principle be reused between those two steps; the window is the two
/// system calls while the peer still holds the connection open.
///
/// # Errors
/// Returns an error when the peer cannot be identified or its token cannot be
/// read, including when the peer belongs to a user whose process this user
/// may not inspect. Callers treat every error as a rejection.
pub fn peer_user(stream: &LocalStream) -> io::Result<UserSid> {
    let process_id = peer_process_id(stream)?;
    // SAFETY: `OpenProcess` has no pointer arguments; a null return is checked.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `process` is a newly opened handle owned by nobody else.
    let process = unsafe { OwnedHandle::from_raw_handle(process.cast()) };
    token_sid(process.as_raw_handle().cast(), TokenUser)
}

/// Returns the owner recorded in the security descriptor of `path`.
///
/// The path is opened without following a reparse point, so a symbolic link
/// reports its own owner and an AF_UNIX socket file can be inspected at all.
///
/// # Errors
/// Returns an error when the path cannot be opened or has no readable owner.
pub fn file_owner(path: &Path) -> io::Result<UserSid> {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    owner_of(&file)
}

/// Returns the owner recorded in the security descriptor of an open handle.
///
/// The handle needs `READ_CONTROL` access; a file opened for reading has it.
///
/// # Errors
/// Returns an error when the descriptor cannot be read or has no valid owner.
pub fn owner_of(handle: &impl AsRawHandle) -> io::Result<UserSid> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is open for the duration of the call. Only the owner
    // and descriptor out-pointers are requested; the others are null as the
    // API allows.
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let _descriptor = LocalAllocation(descriptor);
    // SAFETY: on success `owner` points into `descriptor`, which stays
    // allocated until `_descriptor` drops at the end of this function.
    unsafe { UserSid::copy_from(owner) }
}

/// Reads one SID-bearing token information class from a process.
fn token_sid(process: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<UserSid> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `process` is a valid process handle for this call and `token`
    // is a stack out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a newly opened handle owned by nobody else.
    let token = unsafe { OwnedHandle::from_raw_handle(token.cast()) };
    let mut needed = 0_u32;
    // SAFETY: a size query with a null buffer of length zero is the documented
    // way to learn the required length; the expected failure is ignored.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle().cast(),
            class,
            ptr::null_mut(),
            0,
            &raw mut needed,
        );
    }
    if needed == 0 || needed > MAX_TOKEN_INFORMATION_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process token information has an unexpected size",
        ));
    }
    // A `usize` buffer keeps the pointer fields of the returned structure
    // correctly aligned.
    let mut buffer = vec![0_usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: `buffer` holds at least `needed` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle().cast(),
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with the structure for `class`,
    // aligned because the buffer is `usize`-aligned, and any SID it points to
    // lives inside `buffer`, which outlives this read.
    unsafe {
        let sid = if class == TokenOwner {
            (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner
        } else {
            (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid
        };
        UserSid::copy_from(sid)
    }
}

/// Frees a `LocalAlloc`-owned buffer returned by a Win32 security call.
pub(crate) struct LocalAllocation(pub(crate) *mut core::ffi::c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 call documented to return
            // `LocalAlloc` memory and is freed exactly once, here.
            unsafe { LocalFree(self.0) };
        }
    }
}
