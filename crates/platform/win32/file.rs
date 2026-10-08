//! Whole-file operations by name or open handle: atomic replacement through the write-through
//! rename API, and the stable volume and object identity of an open file.
#![allow(
    unsafe_code,
    reason = "reviewed MoveFileExW and GetFileInformationByHandleEx calls over owned buffers and live handles"
)]

use super::busy::{is_busy, retry_while_busy_pausing};
use super::workspace_fs::replace_unlinking;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_INFO, FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

/// How a replacing rename dealt with the name it replaced.
///
/// Windows has two replacements and neither is the other's superset:
///
/// * the classic one swaps the destination name for the source name in one step, but the kernel
///   refuses it while any other handle holds the destination open (even with delete sharing). A
///   lookup racing the swap can be told `ACCESS_DENIED`, because the replaced file is
///   delete-pending for a moment;
/// * the POSIX-semantics one unlinks the destination at once, whoever holds it, and then links
///   the source name. It is what a Unix `rename` does, but a lookup that lands between the two
///   steps is told the name does not exist, about sixty times as often as the classic transient
///   (see `workspace_fs::rename_into` for the measurement).
///
/// Every replacement here tries the swap first and falls back to the unlink only when the swap is
/// refused, so the larger exposure is limited to a destination that is held open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Replacement {
    /// The destination did not exist: a plain rename.
    NewName,
    /// The classic one-step swap.
    Swapped,
    /// The destination was unlinked while held open; a concurrent lookup may have missed the name.
    Unlinked,
}

/// Atomically replaces `destination` and requests write-through publication.
///
/// The swap is tried first and the POSIX-semantics rename second; see [`Replacement`]. A scanner
/// or indexer that briefly holds either file without sharing delete access makes both fail with a
/// sharing error; see [`super::busy`]. The pair is retried for a bounded time before that error is
/// reported.
pub(crate) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    replace_pausing(source, destination, std::thread::sleep).map(|_| ())
}

/// [`replace`] with the pause between retries injected, so a test can release a scanner's hold at
/// the exact moment the first attempt was refused, reporting which replacement succeeded.
fn replace_pausing(
    source: &Path,
    destination: &Path,
    pause: impl FnMut(std::time::Duration),
) -> io::Result<Replacement> {
    let wide_source = wide_path(source)?;
    let wide_destination = wide_path(destination)?;
    retry_while_busy_pausing(
        || replace_once(source, destination, &wide_source, &wide_destination),
        pause,
    )
}

/// One attempt at each replacement, the gap-free one first.
fn replace_once(
    source: &Path,
    destination: &Path,
    wide_source: &[u16],
    wide_destination: &[u16],
) -> io::Result<Replacement> {
    match move_replacing(wide_source, wide_destination) {
        Ok(()) => Ok(Replacement::Swapped),
        Err(swap_error) if is_busy(&swap_error) => match replace_unlinking(source, destination) {
            Ok(()) => Ok(Replacement::Unlinked),
            // Both refused for the same reason (or this system has no POSIX rename): report the
            // swap's error, which is the holder's.
            Err(unlink_error)
                if is_busy(&unlink_error) || unlink_error.kind() == io::ErrorKind::Unsupported =>
            {
                Err(swap_error)
            }
            Err(other) => Err(other),
        },
        Err(other) => Err(other),
    }
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

pub(crate) fn number_of_links(file: &File) -> io::Result<u64> {
    let mut info = FILE_STANDARD_INFO::default();
    let size = u32::try_from(size_of::<FILE_STANDARD_INFO>())
        .map_err(|_| io::Error::other("FILE_STANDARD_INFO exceeds the u32 information size"))?;
    // SAFETY: the owned file handle stays open, and the output buffer has
    // exactly the size and alignment required by FileStandardInfo.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle().cast(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(info.NumberOfLinks))
}

/// The NUL-terminated extended-length (`\\?\`) form of `path`.
///
/// Win32 file functions refuse a path past `MAX_PATH` unless it is extended-length, and the
/// extended form turns off normalization, so the path is made absolute (resolving `.` and `..`)
/// first. A path that is already extended-length, or names a device, is left as it is.
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let text = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    if text.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows path contains an embedded NUL",
        ));
    }
    let mut value = Vec::with_capacity(text.len() + 8);
    if text.starts_with(&units(r"\\?\")) || text.starts_with(&units(r"\\.\")) {
        value.extend_from_slice(&text);
    } else if text.starts_with(&units(r"\\")) {
        // \\server\share\... becomes \\?\UNC\server\share\...
        value.extend(units(r"\\?\UNC\"));
        value.extend_from_slice(&text[2..]);
    } else {
        value.extend(units(r"\\?\"));
        value.extend_from_slice(&text);
    }
    value.push(0);
    Ok(value)
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[cfg(test)]
mod tests {
    use super::{Replacement, identity_of, replace, replace_pausing};
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
    fn replace_waits_out_a_scanner_that_holds_the_destination_until_the_first_refusal() {
        let directory = scratch("scanner");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");

        // The scanner holds the destination until the replacement has been refused once, so the
        // wait is ordered by handshake and not by a sleep a loaded machine can overrun.
        let (held, ready) = std::sync::mpsc::channel();
        let (release, release_requested) = std::sync::mpsc::channel::<()>();
        let (closed, released) = std::sync::mpsc::channel();
        let scanned = destination.clone();
        let scanner = std::thread::spawn(move || {
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&scanned)
                .expect("scanner opens the destination");
            held.send(()).expect("announce the hold");
            let _ = release_requested.recv();
            drop(handle);
            let _ = closed.send(());
        });
        ready.recv().expect("scanner holds the destination");

        let mut refusals = 0_usize;
        replace_pausing(&source, &destination, |_| {
            refusals += 1;
            if refusals == 1 {
                release.send(()).expect("scanner is still holding");
                released.recv().expect("scanner closed its handle");
            }
        })
        .expect("replace waits for the scanner");
        assert_eq!(refusals, 1, "refused once while held, then published");
        drop(release);
        scanner.join().expect("join scanner");

        assert_eq!(fs::read(&destination).expect("read"), b"new");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    /// Replaces `destination` with `source` through the injected pause, returning which
    /// replacement succeeded and how many times an attempt was refused first.
    fn replace_counting_pauses(
        source: &std::path::Path,
        destination: &std::path::Path,
    ) -> (std::io::Result<Replacement>, usize) {
        let mut pauses = 0;
        let outcome = replace_pausing(source, destination, |_| pauses += 1);
        (outcome, pauses)
    }

    #[test]
    fn replace_swaps_the_name_when_nothing_holds_the_destination() {
        let directory = scratch("swap");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");
        let (outcome, pauses) = replace_counting_pauses(&source, &destination);
        assert_eq!(outcome.expect("replace"), Replacement::Swapped);
        assert_eq!(pauses, 0);
        assert_eq!(fs::read(&destination).expect("read"), b"new");

        // A destination that does not exist yet is created by the same move.
        let fresh = directory.join("fresh");
        fs::write(&source, b"again").expect("write source again");
        let (outcome, pauses) = replace_counting_pauses(&source, &fresh);
        assert_eq!(outcome.expect("create"), Replacement::Swapped);
        assert_eq!(pauses, 0);
        assert_eq!(fs::read(&fresh).expect("read"), b"again");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    /// The swap is refused while any handle holds the destination open, even one that shares
    /// delete. Unix `rename` is not, so the replacement falls back to POSIX semantics at once
    /// instead of waiting out the schedule and failing.
    #[test]
    fn replace_unlinks_a_destination_another_handle_holds_open() {
        use std::io::Read as _;

        let directory = scratch("held");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");
        let mut held = fs::File::open(&destination).expect("hold the destination open");

        let (outcome, pauses) = replace_counting_pauses(&source, &destination);
        assert_eq!(
            outcome.expect("replace over a held file"),
            Replacement::Unlinked
        );
        assert_eq!(pauses, 0, "no wait for a holder that shares delete");
        assert_eq!(fs::read(&destination).expect("read the name"), b"new");
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes)
            .expect("the holder keeps its generation");
        assert_eq!(bytes, b"old");
        drop(held);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    /// Unix `rename` consults the directory and never the replaced file's mode.
    #[test]
    fn replace_overwrites_a_read_only_destination_without_waiting() {
        let directory = scratch("read-only");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        let mut permissions = fs::metadata(&destination).expect("stat").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&destination, permissions).expect("mark read-only");
        fs::write(&source, b"new").expect("write source");

        let (outcome, pauses) = replace_counting_pauses(&source, &destination);
        outcome.expect("replace over a read-only destination");
        assert_eq!(
            pauses, 0,
            "a permanent refusal is not a scanner to wait out"
        );
        assert_eq!(fs::read(&destination).expect("read"), b"new");
        fs::remove_dir_all(directory).expect("cleanup");
    }

    /// Win32 path functions refuse names past `MAX_PATH` unless they are extended-length; the
    /// standard library adds that prefix for its own calls, so a replacement made with
    /// `MoveFileExW` has to as well, or a state file deep in a project tree cannot be published.
    #[test]
    fn replace_publishes_beyond_max_path() {
        let mut directory = scratch("long");
        let root = directory.clone();
        while directory.as_os_str().len() < 400 {
            directory.push("a-deep-directory-name-that-adds-length");
        }
        fs::create_dir_all(&directory).expect("create a path beyond MAX_PATH");
        let (source, destination) = (directory.join("next"), directory.join("state"));
        fs::write(&destination, b"old").expect("write destination");
        fs::write(&source, b"new").expect("write source");
        assert!(destination.as_os_str().len() > 260);

        let (outcome, pauses) = replace_counting_pauses(&source, &destination);
        assert_eq!(
            outcome.expect("replace beyond MAX_PATH"),
            Replacement::Swapped
        );
        assert_eq!(pauses, 0);
        assert_eq!(fs::read(&destination).expect("read"), b"new");

        // The fallback names its files the same way.
        let held = fs::File::open(&destination).expect("hold the destination");
        fs::write(&source, b"newer").expect("write source again");
        let (outcome, _) = replace_counting_pauses(&source, &destination);
        assert_eq!(
            outcome.expect("replace a held file beyond MAX_PATH"),
            Replacement::Unlinked
        );
        assert_eq!(fs::read(&destination).expect("read"), b"newer");
        drop(held);
        fs::remove_dir_all(root).expect("cleanup");
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
