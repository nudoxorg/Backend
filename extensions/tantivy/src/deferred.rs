//! A Tantivy directory that leaves durability to one batched flush.
//!
//! Tantivy's own `MmapDirectory` flushes every file it writes and rewrites its `managed.json`
//! (flush plus rename) each time a file is registered. A projection build creates dozens of
//! files, so on a device whose flushes are slow and serialized the build waits for a long chain
//! of barriers even though the projection is written into a private stage that nothing can name
//! until it is published.
//!
//! [`DeferredSyncDirectory`] performs the same operations with the same atomic-replacement
//! semantics but without any flush. The builder must call [`DeferredSyncDirectory::flush_all`]
//! before the stage is published: it flushes every file concurrently and then the directory
//! once, which is what makes the stage durable. Nothing may publish, rename or record a stage
//! built through this directory before that call returns.

use backend_platform::durability::{flush_concurrently_bounded, open_directory};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, TerminatingWrite,
    WatchCallback, WatchHandle, WritePtr,
};

/// The most file flushes the final barrier keeps in flight.
const FLUSHES_IN_FLIGHT: usize = 32;

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// A directory over `root` whose writes are made durable by [`Self::flush_all`], not one by one.
#[derive(Clone)]
pub(crate) struct DeferredSyncDirectory {
    root: Arc<PathBuf>,
    inner: MmapDirectory,
}

impl fmt::Debug for DeferredSyncDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeferredSyncDirectory")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl DeferredSyncDirectory {
    /// Opens the existing directory `root`.
    pub(crate) fn open(root: &Path) -> Result<Self, io::Error> {
        let inner = MmapDirectory::open(root)
            .map_err(|error| io::Error::other(format!("open projection directory: {error}")))?;
        Ok(Self {
            root: Arc::new(root.to_path_buf()),
            inner,
        })
    }

    /// Flushes every file of the directory concurrently, then the directory itself.
    ///
    /// Returns only after all of them are durable. Tantivy writes a flat directory; anything
    /// that is not a regular file was not written through this directory and is left alone.
    pub(crate) fn flush_all(&self) -> Result<(), io::Error> {
        let mut files = Vec::new();
        for entry in fs::read_dir(self.root.as_path())? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                files.push(entry.path());
            }
        }
        flush_concurrently_bounded(&files, FLUSHES_IN_FLIGHT, |path| {
            // A file the builder already replaced or removed is no longer part of the stage.
            match File::open(path) {
                Ok(file) => file.sync_all(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        })?;
        open_directory(self.root.as_path())?.sync_all()
    }
}

struct DeferredWriter(File);

impl Write for DeferredWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminatingWrite for DeferredWriter {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        // Flushed by `DeferredSyncDirectory::flush_all` together with every other file.
        self.0.flush()
    }
}

impl Directory for DeferredSyncDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(path))
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    OpenWriteError::FileAlreadyExists(path.to_path_buf())
                } else {
                    OpenWriteError::wrap_io_error(error, path.to_path_buf())
                }
            })?;
        Ok(BufWriter::new(Box::new(DeferredWriter(file))))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        // Same replacement semantics as the stock directory: readers see the old file or the
        // new one, never a mixture. The flush is `flush_all`'s.
        let target = self.root.join(path);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?;
        let temporary = self.root.join(format!(
            ".{name}.{}.{}.deferred",
            std::process::id(),
            NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(data)?;
            file.flush()?;
            fs::rename(&temporary, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn sync_directory(&self) -> io::Result<()> {
        // Deferred to `flush_all`.
        Ok(())
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(watch_callback)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::DeferredSyncDirectory;
    use tantivy::schema::{Schema, TEXT};
    use tantivy::{Index, IndexSettings, doc};

    fn scratch(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "backend-tantivy-deferred-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        path
    }

    #[test]
    fn an_index_built_without_per_file_flushes_reopens_complete_after_the_batched_flush() {
        let path = scratch("reopen");
        let directory = DeferredSyncDirectory::open(&path).expect("open deferred directory");
        let mut builder = Schema::builder();
        let text = builder.add_text_field("text", TEXT);
        let index = Index::create(directory.clone(), builder.build(), IndexSettings::default())
            .expect("create index");
        let mut writer = index.writer_with_num_threads(2, 30_000_000).expect("writer");
        for ordinal in 0..64 {
            writer
                .add_document(doc!(text => format!("alpha beta gamma {ordinal}")))
                .expect("add document");
        }
        writer.commit().expect("commit");
        drop(writer);
        directory.flush_all().expect("flush the stage");
        drop(index);

        let reopened = Index::open_in_dir(&path).expect("reopen with the stock directory");
        let reader = reopened.reader().expect("reader");
        assert_eq!(reader.searcher().num_docs(), 64);
        // No replacement temporary survives, and the directory stays flat.
        for entry in std::fs::read_dir(&path).expect("list") {
            let entry = entry.expect("entry");
            assert!(entry.file_type().expect("type").is_file());
            assert!(!entry.file_name().to_string_lossy().ends_with(".deferred"));
        }
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn entries_that_are_not_regular_files_are_left_alone() {
        let path = scratch("subdirectory");
        let directory = DeferredSyncDirectory::open(&path).expect("open deferred directory");
        std::fs::create_dir(path.join("nested")).expect("create nested directory");
        std::fs::write(path.join("plain"), b"data").expect("write file");
        directory.flush_all().expect("flush the regular files");
        let _ = std::fs::remove_dir_all(path);
    }
}
