//! Atomic file replacement through the Windows write-through rename API.
#![allow(
    unsafe_code,
    reason = "one reviewed MoveFileExW call over owned NUL-terminated path buffers"
)]

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::time::Duration;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// A destination just replaced or read can be briefly held open by
/// antivirus real-time scanning (observed with Windows Defender), which
/// fails an immediately-following `MoveFileExW` replacement with
/// `ERROR_ACCESS_DENIED`. Retry a bounded number of times with a short
/// backoff instead of failing closed on a transient external lock.
const ACCESS_DENIED_RETRIES: u32 = 20;
const ACCESS_DENIED_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Atomically replaces `destination` and requests write-through publication.
pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    let mut attempt = 0_u32;
    loop {
        // SAFETY: both vectors are owned, NUL-terminated UTF-16 buffers that
        // stay alive for the duration of the call. The flags contain no
        // borrowed data.
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
            && attempt < ACCESS_DENIED_RETRIES
        {
            attempt += 1;
            std::thread::sleep(ACCESS_DENIED_RETRY_DELAY);
            continue;
        }
        return Err(error);
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows path contains an embedded NUL",
        ));
    }
    value.push(0);
    Ok(value)
}
