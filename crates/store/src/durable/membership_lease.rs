//! Per-closure reader roots, registered under the global admission barrier.

use super::{ClosureId, FileStore, GcRoot, GcRoots, StoreError};

/// A reachability lease shared by all snapshots retaining one durable index.
#[derive(Debug)]
pub(crate) struct ClosureMembershipLease {
    #[cfg(unix)]
    _file: std::fs::File,
}

#[cfg(unix)]
mod platform {
    use super::*;
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, mkdirat, open, openat, unlinkat};
    use rustix::io::Errno;
    use std::fs::{File, TryLockError};
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const DIRECTORY: &str = "closure-reader-leases";
    const MAX_LEASES: usize = 4096;
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn error(error: Errno) -> StoreError {
        if matches!(error, Errno::LOOP | Errno::NOTDIR) {
            StoreError::UnsafePath
        } else {
            super::super::io_error(&std::io::Error::from(error))
        }
    }

    fn directory(store: &FileStore, create: bool) -> Result<Option<File>, StoreError> {
        let root = open(
            &store.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?;
        if create {
            match mkdirat(&root, DIRECTORY, Mode::RWXU) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(other) => return Err(error(other)),
            }
        }
        match openat(
            &root,
            DIRECTORY,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => Ok(Some(File::from(directory))),
            Err(Errno::NOENT) if !create => Ok(None),
            Err(other) => Err(error(other)),
        }
    }

    fn names(
        directory: &File,
        mut visit: impl FnMut(&str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut entries = Dir::read_from(directory).map_err(error)?;
        let mut count = 0_usize;
        for entry in &mut entries {
            let entry = entry.map_err(error)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| StoreError::UnsafePath)?;
            if matches!(name, "." | "..") {
                continue;
            }
            count = count.checked_add(1).ok_or(StoreError::Bounds)?;
            if count > MAX_LEASES {
                return Err(StoreError::Bounds);
            }
            visit(name)?;
        }
        Ok(())
    }

    fn validate(file: &File) -> Result<(), StoreError> {
        let metadata = file.metadata().map_err(|e| super::super::io_error(&e))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(StoreError::UnsafePath);
        }
        Ok(())
    }

    pub(super) fn register(
        store: &FileStore,
        closure: ClosureId,
    ) -> Result<ClosureMembershipLease, StoreError> {
        // The caller still owns the affine admission pin. GC cannot snapshot
        // this directory between file installation and the held lease.
        let directory = directory(store, true)?.ok_or(StoreError::Corrupt)?;
        let mut count = 0_usize;
        names(&directory, |_| {
            count += 1;
            Ok(())
        })?;
        if count >= MAX_LEASES {
            return Err(StoreError::Bounds);
        }
        for _ in 0..MAX_LEASES {
            let ordinal = NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| StoreError::Bounds)?;
            let name = format!(
                "{}-{}-{ordinal}.lease",
                super::super::hex(closure.as_bytes()),
                std::process::id()
            );
            let fd = match openat(
                &directory,
                name.as_str(),
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => fd,
                Err(Errno::EXIST) => continue,
                Err(other) => return Err(error(other)),
            };
            let mut file = File::from(fd);
            validate(&file)?;
            file.lock().map_err(|e| super::super::io_error(&e))?;
            file.write_all(closure.as_bytes())
                .map_err(|e| super::super::io_error(&e))?;
            file.sync_all().map_err(|e| super::super::io_error(&e))?;
            directory
                .sync_all()
                .map_err(|e| super::super::io_error(&e))?;
            return Ok(ClosureMembershipLease { _file: file });
        }
        Err(StoreError::Bounds)
    }

    pub(super) fn resolve(store: &FileStore, roots: &mut GcRoots) -> Result<(), StoreError> {
        let Some(directory) = directory(store, false)? else {
            return Ok(());
        };
        // Collection holds the exclusive global admission barrier. New
        // leases cannot appear while this bounded root snapshot is taken.
        names(&directory, |name| {
            let fd = openat(
                &directory,
                name,
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(error)?;
            let mut file = File::from(fd);
            validate(&file)?;
            match file.try_lock() {
                Ok(()) => {
                    // A dead process or dropped reader owns no reachability.
                    // Only its lease metadata is removed; normal GC owns CAS.
                    unlinkat(&directory, name, AtFlags::empty()).map_err(error)?;
                }
                Err(TryLockError::WouldBlock) => {
                    let mut id = [0_u8; 32];
                    file.read_exact(&mut id)
                        .map_err(|e| super::super::io_error(&e))?;
                    if file
                        .metadata()
                        .map_err(|e| super::super::io_error(&e))?
                        .len()
                        != 32
                        || !name.starts_with(&format!("{}-", super::super::hex(&id)))
                        || !name.ends_with(".lease")
                    {
                        return Err(StoreError::Corrupt);
                    }
                    roots.add(GcRoot::Closure(ClosureId::from_bytes(id)));
                }
                Err(TryLockError::Error(error)) => return Err(super::super::io_error(&error)),
            }
            Ok(())
        })?;
        directory.sync_all().map_err(|e| super::super::io_error(&e))
    }
}

impl FileStore {
    pub(crate) fn lease_closure_membership(
        &self,
        closure: ClosureId,
    ) -> Result<ClosureMembershipLease, StoreError> {
        #[cfg(unix)]
        {
            platform::register(self, closure)
        }
        #[cfg(not(unix))]
        {
            let _ = closure;
            Err(StoreError::Bounds)
        }
    }

    pub(super) fn add_leased_closure_roots(&self, roots: &mut GcRoots) -> Result<(), StoreError> {
        #[cfg(unix)]
        {
            platform::resolve(self, roots)
        }
        #[cfg(not(unix))]
        {
            let _ = roots;
            Ok(())
        }
    }
}
