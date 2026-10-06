//! Bounded physical allocation admission for the immutable CAS and indexes.

use super::{FileStore, StoreError};

/// Allocated filesystem blocks, including durable index nodes and descriptors.
/// Logical payload lengths and live memory reservations cannot substitute for
/// this policy. Both selected and unselected immutable files remain charged
/// until normal store GC reclaims them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalAllocationBudget {
    max_allocated_bytes: u64,
    max_files: usize,
}

impl PhysicalAllocationBudget {
    /// Creates an explicit immutable-store allocation and scan policy.
    #[must_use]
    pub const fn new(max_allocated_bytes: u64, max_files: usize) -> Self {
        Self {
            max_allocated_bytes,
            max_files,
        }
    }

    /// Checks actual allocated blocks with constant scan memory.
    /// Unsupported platforms refuse rather than using logical file lengths.
    pub fn admit(self, store: &FileStore) -> Result<u64, StoreError> {
        if self.max_allocated_bytes == 0 || self.max_files == 0 {
            return Err(StoreError::Bounds);
        }
        #[cfg(unix)]
        {
            self.admit_unix(store)
        }
        #[cfg(not(unix))]
        {
            let _ = store;
            Err(StoreError::Bounds)
        }
    }

    #[cfg(unix)]
    fn admit_unix(self, store: &FileStore) -> Result<u64, StoreError> {
        use rustix::fs::{Dir, Mode, OFlags, open, openat};
        use std::fs::File;
        use std::os::unix::fs::MetadataExt;
        let error = |error: rustix::io::Errno| super::io_error(&std::io::Error::from(error));
        let root = open(
            &store.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?;
        let mut bytes = 0_u64;
        let mut count = 0_usize;
        for directory_name in ["objects", "closures", "packs"] {
            let directory = openat(
                &root,
                directory_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(error)?;
            bytes = bytes
                .checked_add(
                    directory
                        .metadata()
                        .map_err(|e| super::io_error(&e))?
                        .blocks()
                        .checked_mul(512)
                        .ok_or(StoreError::Bounds)?,
                )
                .ok_or(StoreError::Bounds)?;
            let mut entries = Dir::read_from(&directory).map_err(error)?;
            for entry in &mut entries {
                let entry = entry.map_err(error)?;
                let name = entry.file_name();
                if matches!(name.to_bytes(), b"." | b"..") {
                    continue;
                }
                count = count.checked_add(1).ok_or(StoreError::Bounds)?;
                if count > self.max_files {
                    return Err(StoreError::Bounds);
                }
                let file = openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(error)?;
                let metadata = file.metadata().map_err(|e| super::io_error(&e))?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    return Err(StoreError::UnsafePath);
                }
                bytes = bytes
                    .checked_add(
                        metadata
                            .blocks()
                            .checked_mul(512)
                            .ok_or(StoreError::Bounds)?,
                    )
                    .ok_or(StoreError::Bounds)?;
                if bytes > self.max_allocated_bytes {
                    return Err(StoreError::Bounds);
                }
            }
        }
        if bytes > self.max_allocated_bytes {
            return Err(StoreError::Bounds);
        }
        Ok(bytes)
    }
}
