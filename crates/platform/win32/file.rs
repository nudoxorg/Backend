//! Whole-file operations by name or open handle: atomic replacement through the write-through
//! rename API, and the stable volume and object identity of an open file.
#![allow(
    unsafe_code,
    reason = "reviewed MoveFileExW and GetFileInformationByHandleEx calls over owned buffers and live handles"
)]

use std::fs::File;
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW,
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

/// Returns the volume serial number and 128-bit file id of an open handle.
///
/// The handle needs no access right beyond the ability to be queried, so an
/// attribute-only handle suffices. Filesystems that report no file id fail
/// with the operating-system error rather than a fabricated identity.
pub(crate) fn identity_of(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    let size = u32::try_from(mem::size_of::<FILE_ID_INFO>())
        .map_err(|_| io::Error::other("FILE_ID_INFO exceeds the u32 information size"))?;
    // SAFETY: `file` keeps its handle open for the call, and `info` is
    // writable storage of exactly the size `FileIdInfo` requires.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileIdInfo,
            (&raw mut info).cast(),
            size,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
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
