//! Whole-file operations by name or open handle: atomic replacement through the write-through
//! rename API, and the stable volume and object identity of an open file.
#![allow(
    unsafe_code,
    reason = "reviewed MoveFileExW and GetFileInformationByHandleEx calls over owned buffers and live handles"
)]

use super::busy::retry_while_busy;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// Atomically replaces `destination` and requests write-through publication.
///
/// A scanner or indexer that briefly holds either file without sharing delete
/// access makes the move fail with a sharing error; see [`super::busy`]. The
/// move is retried for a bounded time before that error is reported.
pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    retry_while_busy(|| move_replacing(&source, &destination))
}

fn move_replacing(source: &[u16], destination: &[u16]) -> io::Result<()> {
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
    let size = u32::try_from(size_of::<FILE_ID_INFO>())
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

#[cfg(test)]
mod tests {
    use super::{identity_of, replace};
    use std::fs;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    fn scratch(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("bp-file-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create scratch directory");
        path
    }

    #[test]
    fn replace_publishes_over_an_existing_file() {
        let directory = scratch("replace");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");
        replace(&source, &destination).expect("replace");
        assert_eq!(fs::read(&destination).expect("read"), b"new");
        assert!(!source.exists());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn replace_waits_out_a_scanner_that_briefly_holds_the_destination() {
        let directory = scratch("scanner");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");

        let (held, ready) = std::sync::mpsc::channel();
        let scanned = destination.clone();
        let scanner = std::thread::spawn(move || {
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&scanned)
                .expect("scanner opens the destination");
            held.send(()).expect("announce the hold");
            std::thread::sleep(Duration::from_millis(60));
            drop(handle);
        });
        ready.recv().expect("scanner holds the destination");
        replace(&source, &destination).expect("replace waits for the scanner");
        scanner.join().expect("join scanner");

        assert_eq!(fs::read(&destination).expect("read"), b"new");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn replace_reports_a_missing_source_without_waiting() {
        let directory = scratch("missing");
        let started = std::time::Instant::now();
        let error = replace(&directory.join("absent"), &directory.join("state"))
            .expect_err("nothing to publish");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(started.elapsed() < Duration::from_millis(200));
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn identity_survives_a_rename_and_differs_between_files() {
        let directory = scratch("identity");
        let (first, second) = (directory.join("first"), directory.join("second"));
        fs::write(&first, b"1").expect("write first");
        fs::write(&second, b"2").expect("write second");
        let handle = fs::File::open(&first).expect("open first");
        let before = identity_of(&handle).expect("identity before");
        fs::rename(&first, directory.join("renamed")).expect("rename while open");
        assert_eq!(identity_of(&handle).expect("identity after"), before);
        let other = fs::File::open(&second).expect("open second");
        assert_ne!(identity_of(&other).expect("other identity"), before);
        drop((handle, other));
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
