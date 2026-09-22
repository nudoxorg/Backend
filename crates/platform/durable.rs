//! Crash-safe publication of small durable state files.
//!
//! Writers create a unique sibling, flush its contents, atomically replace the
//! canonical path, and finally flush the containing directory. Keeping this
//! protocol here gives every process surface one reviewed operating-system
//! boundary instead of subtly different rename sequences.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Publishes `bytes` at `path` without exposing a partial state file.
///
/// # Errors
///
/// Returns an I/O error when the parent cannot be created, the sibling cannot
/// be written and flushed, the atomic replacement fails, or the publication
/// fence cannot be flushed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = parent(path);
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = create_temporary(parent, path.file_name())?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    sync_parent(path)
}

fn create_temporary(parent: &Path, file_name: Option<&OsStr>) -> io::Result<(PathBuf, File)> {
    loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".");
        name.push(file_name.unwrap_or_else(|| OsStr::new("state")));
        name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let path = parent.join(name);
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Atomically replaces `destination` with `source` on the same filesystem.
///
/// # Errors
///
/// Returns an I/O error when the operating system cannot replace the path.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    replace_file_platform(source, destination)
}

#[cfg(not(windows))]
fn replace_file_platform(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file_platform(source: &Path, destination: &Path) -> io::Result<()> {
    crate::win32::file::replace(source, destination)
}

/// Flushes the directory entry containing `path` after publication.
///
/// # Errors
///
/// Returns an I/O error when the containing directory cannot be opened or
/// flushed on Unix. Windows replacement already requests write-through.
#[cfg(unix)]
pub fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(parent(path))?.sync_all()
}

/// Windows replacement uses `MOVEFILE_WRITE_THROUGH`, so no separate portable
/// directory handle is required.
#[cfg(windows)]
pub fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-platform-durable-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    #[test]
    fn repeated_publication_never_leaves_sibling_temporaries() {
        let root = fixture("replace");
        let path = root.join("state.json");
        for generation in 0_u64..64 {
            let bytes = generation.to_le_bytes();
            write_atomic(&path, &bytes).expect("publish generation");
            assert_eq!(fs::read(&path).expect("read generation"), bytes);
        }
        let names = fs::read_dir(&root)
            .expect("read fixture")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("state.json")]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_replacement_removes_the_private_temporary() {
        let root = fixture("cleanup");
        let destination = root.join("occupied");
        fs::create_dir(&destination).expect("occupied destination");
        assert!(write_atomic(&destination, b"state").is_err());
        let names = fs::read_dir(&root)
            .expect("read fixture")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [OsString::from("occupied")]);
        let _ = fs::remove_dir_all(root);
    }
}
