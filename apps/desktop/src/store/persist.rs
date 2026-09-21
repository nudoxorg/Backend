//! Small durable-file primitive shared by reader-owned state.
//!
//! Preferences and marks are independent of the engine's authenticated object
//! store, but they still need the same crash property: a restart must observe
//! the complete previous value or the complete replacement.  A per-process
//! temporary name avoids the old fixed-name race between GPUI background
//! writes, `sync_all` makes the bytes durable before publication, and syncing
//! the containing directory makes the rename itself durable.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_STAGING_SCAN: usize = 256;
const STAGING_RETENTION: Duration = Duration::from_secs(60 * 60);

/// Writes one complete file using a sibling temporary and an atomic rename.
///
/// The parent is created first because reader state is allowed to be saved on
/// the first interaction after a workspace is discovered.  A stale temporary
/// from a killed process is never opened or truncated: every attempt uses
/// `create_new` and advances the process-local sequence until it owns a fresh
/// name.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    reap_old_staging_files(parent, path);

    let (temporary, mut file) = loop {
        let temporary = temporary_path(parent, path);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
        return Err(error);
    }
    Ok(())
}

/// Removes only clearly abandoned staging files, with both age and count
/// bounds.  A writer that is still active has a fresh mtime and is therefore
/// left alone; a killed writer's file is eventually reclaimed on the next
/// save without making startup scan an unbounded directory.
fn reap_old_staging_files(parent: &Path, path: &Path) {
    let Some(prefix) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let prefix = format!(".{prefix}.");
    let now = SystemTime::now();
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.take(MAX_STAGING_SCAN).flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(".tmp") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() >= STAGING_RETENTION {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn temporary_path(parent: &Path, path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nudox-desktop-persist-{name}-{}-{}",
            std::process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("persistence fixture");
        root
    }

    #[test]
    fn a_restarted_reader_sees_a_complete_value_and_no_staging_file() {
        let root = fixture("roundtrip");
        let path = root.join("state");
        atomic_write(&path, b"complete value\n").expect("atomic write");
        assert_eq!(fs::read(&path).expect("read target"), b"complete value\n");
        let temporary = fs::read_dir(&root)
            .expect("read fixture")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(temporary.is_empty(), "temporary leaked: {temporary:?}");
        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn concurrent_background_writes_publish_only_complete_values() {
        let root = Arc::new(fixture("concurrent"));
        let path = root.join("state");
        let mut writers = Vec::new();
        for index in 0..16_u8 {
            let root = Arc::clone(&root);
            writers.push(thread::spawn(move || {
                let path = root.join("state");
                let value = vec![index; 512];
                atomic_write(&path, &value).expect("concurrent atomic write");
            }));
        }
        for writer in writers {
            writer.join().expect("writer");
        }
        let bytes = fs::read(&path).expect("read concurrent target");
        assert_eq!(bytes.len(), 512, "a torn write was published");
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
        fs::remove_dir_all(&*root).expect("remove fixture");
    }

    #[test]
    fn an_interrupted_staging_file_cannot_replace_the_last_target() {
        let root = fixture("interrupted");
        let path = root.join("state");
        atomic_write(&path, b"old\n").expect("initial value");
        fs::write(root.join(".state.interrupted.tmp"), b"truncated").expect("staging bytes");
        atomic_write(&path, b"new\n").expect("replacement value");
        assert_eq!(fs::read(&path).expect("read target"), b"new\n");
        fs::remove_dir_all(root).expect("remove fixture");
    }
}
