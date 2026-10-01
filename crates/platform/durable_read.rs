//! Bounded reads of ordinary state and cache files published atomically.

use crate::directory::DirectoryCapability;
use std::io::{self, Read, Write};
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

/// A borrowed serialization sink whose total output cannot exceed a byte limit.
///
/// Serializers write directly into the caller's reusable allocation instead of
/// first producing an unbounded temporary encoding. An over-budget write leaves
/// that entire chunk out. On a serializer error the caller may truncate to its
/// own checkpoint, so a rejected record cannot consume later records' budget.
pub struct BoundedWriter<'a> {
    output: &'a mut Vec<u8>,
    maximum: usize,
}

impl std::fmt::Debug for BoundedWriter<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedWriter")
            .field("length", &self.output.len())
            .field("maximum", &self.maximum)
            .finish()
    }
}

impl<'a> BoundedWriter<'a> {
    /// Borrows an output buffer, including any bytes already in it in the limit.
    ///
    /// # Errors
    /// Returns `InvalidData` when the existing buffer already exceeds the limit.
    pub fn new(output: &'a mut Vec<u8>, maximum: usize) -> io::Result<Self> {
        if output.len() > maximum {
            return Err(oversized(maximum));
        }
        Ok(Self { output, maximum })
    }
}

impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum - self.output.len() {
            return Err(oversized(self.maximum));
        }
        let needed = self.output.len() + bytes.len();
        if needed > self.output.capacity() {
            let capacity = self
                .output
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .min(self.maximum);
            self.output
                .try_reserve_exact(capacity - self.output.len())
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
        }
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
    fn borrowed_writer_counts_existing_bytes_and_rejects_whole_over_budget_chunks() {
        let mut output = vec![1; 7];
        {
            let mut writer = BoundedWriter::new(&mut output, 31).expect("bounded output");
            writer.write_all(&[2; 13]).expect("fits");
            assert_eq!(
                writer
                    .write_all(&[3; 12])
                    .expect_err("one byte over")
                    .kind(),
                io::ErrorKind::InvalidData
            );
            writer
                .write_all(&[4; 11])
                .expect("rejection consumed no room");
        }
        assert_eq!(&output[..7], &[1; 7]);
        assert_eq!(&output[7..20], &[2; 13]);
        assert_eq!(&output[20..], &[4; 11]);
        assert_eq!(output.len(), 31);
        assert!(
            output.capacity() <= 31,
            "requested growth stays inside the budget"
        );
        assert!(BoundedWriter::new(&mut output, 30).is_err());
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
