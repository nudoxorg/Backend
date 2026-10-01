//! Bounded reads of ordinary state and cache files published atomically.

use crate::DirectoryCapability;
use std::io::{self, Read};
use std::path::Path;

/// Reads at most `maximum` bytes from an opened regular file.
///
/// The parent is pinned before the final component is opened. Links and special
/// files are rejected through the opened handle; Unix opens are nonblocking so
/// a FIFO cannot stall startup before the type check. Ordinary inherited file
/// permissions remain valid. Use [`super::open_private_read`] for private state
/// whose ownership and permissions are also part of its admission contract.
///
/// The opened file's size is checked before allocating, and growth after that
/// check is bounded independently. A replaced pathname cannot redirect the read.
///
/// # Errors
/// Returns the filesystem error, `InvalidData` for an oversized file, or
/// `InvalidInput` if the final component or limit cannot be represented.
pub fn read_regular_bounded(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "state path needs a Unicode file name",
            )
        })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = DirectoryCapability::open_read_only_source(parent)?;
    let file = directory.open_file_read(name)?;
    let length = file.metadata()?.len();
    read_bounded(file, length, maximum)
}

fn read_bounded(reader: impl Read, length: u64, maximum: usize) -> io::Result<Vec<u8>> {
    let limit = maximum
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "state byte limit cannot be represented",
            )
        })?;
    if length >= limit {
        return Err(oversized(maximum));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
    reader.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(oversized(maximum));
    }
    Ok(bytes)
}

fn oversized(maximum: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("state file exceeds {maximum} bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-bounded-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("scratch directory");
        path
    }

    #[test]
    fn ordinary_state_round_trips_at_the_exact_byte_limit() {
        let root = scratch();
        let path = root.join("state");
        super::super::write_atomic(&path, b"state").expect("publish ordinary state");
        assert_eq!(
            read_regular_bounded(&path, 5).expect("bounded read"),
            b"state"
        );
        assert_eq!(
            read_regular_bounded(&path, 4)
                .expect_err("too large")
                .kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn growth_after_metadata_is_bounded_by_the_stream_limit() {
        let mut reader = std::io::Cursor::new([7_u8; 1024]);
        assert_eq!(
            read_bounded(&mut reader, 0, 31)
                .expect_err("grew after metadata")
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            reader.position(),
            32,
            "only the limit plus one sentinel byte is read"
        );
        assert!(read_bounded(&b""[..], 0, 0).expect("empty file").is_empty());
    }

    #[test]
    fn oversized_sparse_file_is_refused_before_reading() {
        let root = scratch();
        let path = root.join("sparse");
        fs::File::create(&path)
            .expect("create sparse file")
            .set_len(16_u64 << 30)
            .expect("set sparse length");
        assert_eq!(
            read_regular_bounded(&path, 4096)
                .expect_err("oversized sparse file")
                .kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_and_fifo_are_refused_without_waiting_for_a_writer() {
        let root = scratch();
        fs::write(root.join("target"), b"state").expect("target");
        std::os::unix::fs::symlink("target", root.join("link")).expect("symlink");
        assert!(read_regular_bounded(&root.join("link"), 4096).is_err());
        let status = std::process::Command::new("/usr/bin/mkfifo")
            .arg(root.join("fifo"))
            .status()
            .expect("mkfifo");
        assert!(status.success());
        let start = std::time::Instant::now();
        assert!(read_regular_bounded(&root.join("fifo"), 4096).is_err());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "FIFO must not wait for a writer"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
