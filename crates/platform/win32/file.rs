//! Atomic file replacement through the Windows write-through rename API.
#![allow(
    unsafe_code,
    reason = "one reviewed MoveFileExW call over owned NUL-terminated path buffers"
)]

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// Atomically replaces `destination` and requests write-through publication.
pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // SAFETY: both vectors are owned, NUL-terminated UTF-16 buffers that stay
    // alive for the duration of the call. The flags contain no borrowed data.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
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
