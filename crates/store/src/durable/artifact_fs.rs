//! Descriptor-relative filesystem operations for the artifact stream.
//!
//! Artifact admission treats the store root as a configured trust boundary,
//! then resolves every mutable child relative to an opened directory with
//! symlink following disabled. CAS members must be regular files with exactly
//! one link; a link count above one would let another name mutate supposedly
//! immutable bytes.

use super::{ClosureId, FileStore, ObjectId, StoreError, io_error};
use crate::UntrustedObjectId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const LOCK_FILE: &str = "ACTIVE.lock";
const CLOSURE_TEMP_FILE: &str = "closure-descriptor.tmp";
static NEXT_SESSION: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static TEST_FAULTS: std::sync::Mutex<Vec<(std::thread::ThreadId, u8)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
static TEST_LEAVE_SESSION: std::sync::Mutex<Vec<std::thread::ThreadId>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
static TEST_LINK_BARRIERS: std::sync::Mutex<
    Vec<(
        std::thread::ThreadId,
        std::sync::mpsc::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn set_test_fault(point: u8) {
    if let Ok(mut faults) = TEST_FAULTS.lock() {
        let owner = std::thread::current().id();
        faults.retain(|(thread, _)| *thread != owner);
        faults.push((owner, point));
    }
}

#[cfg(test)]
pub(super) fn set_test_link_barrier(
    reached: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) {
    if let Ok(mut barriers) = TEST_LINK_BARRIERS.lock() {
        let owner = std::thread::current().id();
        barriers.retain(|(thread, _, _)| *thread != owner);
        barriers.push((owner, reached, release));
    }
}

#[cfg(test)]
fn wait_test_link_barrier() {
    let barrier = TEST_LINK_BARRIERS.lock().ok().and_then(|mut barriers| {
        let owner = std::thread::current().id();
        barriers
            .iter()
            .position(|(thread, _, _)| *thread == owner)
            .map(|index| barriers.remove(index))
    });
    if let Some((_, reached, release)) = barrier {
        let _ = reached.send(());
        let _ = release.recv();
    }
}

#[cfg(not(test))]
fn wait_test_link_barrier() {}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut faults) = TEST_FAULTS.lock() else {
        return false;
    };
    let owner = std::thread::current().id();
    let Some(index) = faults
        .iter()
        .position(|(thread, pending)| *thread == owner && *pending == point)
    else {
        return false;
    };
    faults.remove(index);
    if std::env::var("BACKEND_STORE_TEST_CRASH_POINT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        == Some(point)
    {
        std::process::exit(80 + i32::from(point));
    }
    if let Ok(mut leave) = TEST_LEAVE_SESSION.lock() {
        leave.push(owner);
    }
    true
}

#[cfg(not(test))]
fn take_test_fault(_: u8) -> bool {
    false
}

#[cfg(test)]
pub(super) fn maybe_test_fault(point: u8) -> bool {
    take_test_fault(point)
}

#[cfg(not(test))]
pub(super) fn maybe_test_fault(_: u8) -> bool {
    false
}

#[cfg(test)]
fn simulate_process_crash() -> bool {
    let Ok(mut leave) = TEST_LEAVE_SESSION.lock() else {
        return false;
    };
    let owner = std::thread::current().id();
    if let Some(index) = leave.iter().position(|thread| *thread == owner) {
        leave.remove(index);
        true
    } else {
        false
    }
}

#[cfg(not(test))]
fn simulate_process_crash() -> bool {
    false
}

fn next_session_name() -> String {
    let ordinal = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    format!("session-{}-{ordinal}", std::process::id())
}

pub(super) struct ArtifactDirectory {
    file: File,
    path: PathBuf,
}

impl ArtifactDirectory {
    pub(super) fn sync_all(&self) -> Result<(), StoreError> {
        self.file.sync_all().map_err(|error| io_error(&error))
    }

    fn try_clone(&self) -> Result<Self, StoreError> {
        Ok(Self {
            file: self.file.try_clone().map_err(|error| io_error(&error))?,
            path: self.path.clone(),
        })
    }
}

pub(super) struct SessionDirectory {
    pub(super) name: String,
    pub(super) parent: ArtifactDirectory,
    pub(super) directory: ArtifactDirectory,
    pub(super) lease: File,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, fchmod, linkat, mkdirat, open, openat, unlinkat};
    use rustix::io::Errno;
    use std::os::unix::fs::MetadataExt;

    fn error(error: Errno) -> StoreError {
        StoreError::Io(error.to_string())
    }

    fn is_missing(error: Errno) -> bool {
        error == Errno::NOENT
    }

    fn store_root(store: &FileStore) -> Result<File, StoreError> {
        open(
            &store.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(error)
    }

    fn open_dir_at(
        parent: &ArtifactDirectory,
        name: &str,
    ) -> Result<ArtifactDirectory, StoreError> {
        openat(
            &parent.file,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(|fd| ArtifactDirectory {
            file: File::from(fd),
            path: parent.path.join(name),
        })
        .map_err(error)
    }

    fn ensure_dir_at(
        parent: &ArtifactDirectory,
        name: &str,
    ) -> Result<ArtifactDirectory, StoreError> {
        match mkdirat(&parent.file, name, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(other) => return Err(error(other)),
        }
        open_dir_at(parent, name)
    }

    fn store_dir(store: &FileStore, name: &str) -> Result<ArtifactDirectory, StoreError> {
        let root = ArtifactDirectory {
            file: store_root(store)?,
            path: store.root.clone(),
        };
        open_dir_at(&root, name)
    }

    fn validate_single_link_file(file: &File) -> Result<(), StoreError> {
        let metadata = file.metadata().map_err(|error| io_error(&error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }

    fn seal_immutable_file(file: &File) -> Result<(), StoreError> {
        validate_single_link_file(file)?;
        fchmod(file, Mode::RUSR).map_err(error)?;
        file.sync_all().map_err(|error| io_error(&error))
    }

    fn open_file_at(
        directory: &File,
        name: &str,
        flags: OFlags,
    ) -> Result<Option<File>, StoreError> {
        const LINK_SETTLE_ATTEMPTS: usize = 100;
        for attempt in 0..LINK_SETTLE_ATTEMPTS {
            match openat(
                directory,
                name,
                flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => {
                    let file = File::from(fd);
                    let metadata = file.metadata().map_err(|error| io_error(&error))?;
                    if !metadata.is_file() {
                        return Err(StoreError::Corrupt);
                    }
                    if metadata.nlink() == 1 {
                        return Ok(Some(file));
                    }
                    if metadata.nlink() != 2 || attempt + 1 == LINK_SETTLE_ATTEMPTS {
                        return Err(StoreError::Corrupt);
                    }
                }
                Err(error) if is_missing(error) => return Ok(None),
                Err(error) => {
                    return Err(if error == Errno::LOOP || error == Errno::NOTDIR {
                        StoreError::Corrupt
                    } else {
                        self::error(error)
                    });
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(StoreError::Corrupt)
    }

    fn open_immutable_child_file(
        store: &FileStore,
        directory: &str,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        let parent = store_dir(store, directory)?;
        let file = open_file_at(&parent.file, name, OFlags::RDONLY)?;
        if let Some(file) = &file {
            seal_immutable_file(file)?;
        }
        Ok(file)
    }

    pub(in crate::durable) fn open_store_file(
        store: &FileStore,
        directory: &str,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        open_immutable_child_file(store, directory, name)
    }

    pub(in crate::durable) fn open_object(
        store: &FileStore,
        id: ObjectId,
    ) -> Result<Option<File>, StoreError> {
        let name = format!("{}.object", super::super::hex(id.as_bytes()));
        open_immutable_child_file(store, "objects", &name)
    }

    pub(in crate::durable) fn open_closure(
        store: &FileStore,
        id: ClosureId,
    ) -> Result<File, StoreError> {
        let name = format!("{}.closure", super::super::hex(id.as_bytes()));
        open_immutable_child_file(store, "closures", &name)?.ok_or(StoreError::Corrupt)
    }

    pub(in crate::durable) fn prepare_staging_root(
        store: &FileStore,
    ) -> Result<ArtifactDirectory, StoreError> {
        let root = ArtifactDirectory {
            file: store_root(store)?,
            path: store.root.clone(),
        };
        let staging = ensure_dir_at(&root, "staging")?;
        let artifacts = ensure_dir_at(&staging, "artifacts")?;
        // Existing artifact directories are tightened through their open file
        // descriptors. No path re-resolution is involved in the permission
        // change.
        fchmod(&staging.file, Mode::RWXU).map_err(error)?;
        fchmod(&artifacts.file, Mode::RWXU).map_err(error)?;
        root.sync_all()?;
        staging.sync_all()?;
        Ok(artifacts)
    }

    pub(in crate::durable) fn create_session(
        parent: &ArtifactDirectory,
    ) -> Result<SessionDirectory, StoreError> {
        loop {
            let name = super::next_session_name();
            let parent_copy = parent.try_clone()?;
            match mkdirat(&parent.file, &name, Mode::RWXU) {
                Ok(()) => {}
                Err(Errno::EXIST) => continue,
                Err(error) => return Err(self::error(error)),
            }
            let directory = match open_dir_at(parent, &name) {
                Ok(directory) => directory,
                Err(error) => return Err(error),
            };
            let result = (|| {
                fchmod(&directory.file, Mode::RWXU).map_err(error)?;
                let lease_fd = openat(
                    &directory.file,
                    LOCK_FILE,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(error)?;
                let lease = File::from(lease_fd);
                validate_single_link_file(&lease)?;
                lease
                    .try_lock()
                    .map_err(|error| StoreError::Io(error.to_string()))?;
                directory.sync_all()?;
                parent.sync_all()?;
                Ok(lease)
            })();
            match result {
                Ok(lease) => {
                    return Ok(SessionDirectory {
                        name,
                        parent: parent_copy,
                        directory,
                        lease,
                    });
                }
                Err(error) => {
                    let _ = remove_session_at(&parent.file, &name, &directory.file, None);
                    return Err(error);
                }
            }
        }
    }

    fn valid_temp_name(name: &str) -> bool {
        if name == CLOSURE_TEMP_FILE {
            return true;
        }
        let Some(index) = name
            .strip_prefix("object-")
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            return false;
        };
        !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
    }

    fn directory_names(directory: &File) -> Result<Vec<String>, StoreError> {
        let entries = Dir::read_from(directory).map_err(error)?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(error)?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| StoreError::Corrupt)?;
            if name != "." && name != ".." {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    fn recover_session_aliases(store: &FileStore, directory: &File) -> Result<(), StoreError> {
        let mut repairs = Vec::new();
        let names = directory_names(directory)?;
        for name in &names {
            if name != LOCK_FILE && !valid_temp_name(name) {
                return Err(StoreError::Corrupt);
            }
            let file = openat(
                directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(self::error)?;
            let metadata = file.metadata().map_err(|error| io_error(&error))?;
            let linked_stage = name == CLOSURE_TEMP_FILE || name.starts_with("object-");
            if !metadata.is_file()
                || (metadata.nlink() != 1 && !(linked_stage && metadata.nlink() == 2))
            {
                return Err(StoreError::Corrupt);
            }
        }
        for name in names {
            if name != CLOSURE_TEMP_FILE && !name.starts_with("object-") {
                continue;
            }
            if !valid_temp_name(&name) {
                return Err(StoreError::Corrupt);
            }
            let staged = match openat(
                directory,
                &name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => File::from(fd),
                Err(error) if is_missing(error) => return Err(StoreError::Corrupt),
                Err(error) => return Err(self::error(error)),
            };
            let staged_metadata = staged.metadata().map_err(|error| io_error(&error))?;
            if !staged_metadata.is_file() {
                return Err(StoreError::Corrupt);
            }
            if staged_metadata.nlink() == 1 {
                continue;
            }
            if staged_metadata.nlink() != 2 {
                return Err(StoreError::Corrupt);
            }

            let (destination_directory_name, destination_name) = if name == CLOSURE_TEMP_FILE {
                const MAX_DESCRIPTOR_BYTES: u64 = 256;
                if staged_metadata.len() > MAX_DESCRIPTOR_BYTES {
                    return Err(StoreError::Corrupt);
                }
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(
                        usize::try_from(staged_metadata.len()).map_err(|_| StoreError::Bounds)?,
                    )
                    .map_err(|_| StoreError::Bounds)?;
                let mut bounded = (&staged).take(MAX_DESCRIPTOR_BYTES + 1);
                bounded
                    .read_to_end(&mut bytes)
                    .map_err(|error| io_error(&error))?;
                if bytes.len()
                    > usize::try_from(MAX_DESCRIPTOR_BYTES).map_err(|_| StoreError::Bounds)?
                    || !super::super::nodes::is_manifest_descriptor(&bytes)
                {
                    return Err(StoreError::Corrupt);
                }
                let magic_len = if bytes.starts_with(super::super::nodes::MANIFEST_INDEX_MAGIC) {
                    super::super::nodes::MANIFEST_INDEX_MAGIC.len()
                } else if bytes.starts_with(super::super::nodes::MANIFEST_INDEX_MAGIC_V1) {
                    super::super::nodes::MANIFEST_INDEX_MAGIC_V1.len()
                } else {
                    return Err(StoreError::Corrupt);
                };
                let end = magic_len.checked_add(32).ok_or(StoreError::Bounds)?;
                let id_bytes: [u8; 32] = bytes
                    .get(magic_len..end)
                    .ok_or(StoreError::Corrupt)?
                    .try_into()
                    .map_err(|_| StoreError::Corrupt)?;
                let id = ClosureId::from_bytes(id_bytes);
                super::super::nodes::decode_manifest_descriptor(&bytes, id)?;
                (
                    "closures",
                    format!("{}.closure", super::super::hex(id.as_bytes())),
                )
            } else {
                let mut staged_reader = &staged;
                let mut id_header = vec![0_u8; super::super::OBJECT_MAGIC.len() + 32];
                staged_reader
                    .read_exact(&mut id_header)
                    .map_err(|error| io_error(&error))?;
                if id_header.get(..super::super::OBJECT_MAGIC.len())
                    != Some(super::super::OBJECT_MAGIC)
                {
                    return Err(StoreError::Corrupt);
                }
                let id_bytes: [u8; 32] = id_header
                    .get(super::super::OBJECT_MAGIC.len()..)
                    .ok_or(StoreError::Corrupt)?
                    .try_into()
                    .map_err(|_| StoreError::Corrupt)?;
                if id_bytes == [0; 32] {
                    return Err(StoreError::Corrupt);
                }
                staged_reader
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| io_error(&error))?;
                let verified = super::super::admit_object_envelope(
                    &mut staged_reader,
                    store.object_envelope_limit()?,
                    store.relation_registry.as_ref(),
                    UntrustedObjectId::from_bytes(id_bytes),
                )?;
                if verified.id().as_bytes() != &id_bytes {
                    return Err(StoreError::Corrupt);
                }
                let id = ObjectId::from_bytes(id_bytes);
                (
                    "objects",
                    format!("{}.object", super::super::hex(id.as_bytes())),
                )
            };

            let destination_directory = store_dir(store, destination_directory_name)?;
            let destination = openat(
                &destination_directory.file,
                &destination_name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(self::error)?;
            let destination_metadata = destination.metadata().map_err(|error| io_error(&error))?;
            if !destination_metadata.is_file()
                || destination_metadata.nlink() != 2
                || staged_metadata.dev() != destination_metadata.dev()
                || staged_metadata.ino() != destination_metadata.ino()
            {
                return Err(StoreError::Corrupt);
            }
            repairs.push((
                name,
                destination_directory_name.to_owned(),
                destination_name,
                staged_metadata.dev(),
                staged_metadata.ino(),
            ));
        }
        for (stage_name, destination_directory_name, destination_name, dev, ino) in repairs {
            let staged = openat(
                directory,
                &stage_name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(self::error)?;
            let destination_directory = store_dir(store, &destination_directory_name)?;
            let destination = openat(
                &destination_directory.file,
                &destination_name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(self::error)?;
            let staged_metadata = staged.metadata().map_err(|error| io_error(&error))?;
            let destination_metadata = destination.metadata().map_err(|error| io_error(&error))?;
            if !staged_metadata.is_file()
                || !destination_metadata.is_file()
                || staged_metadata.nlink() != 2
                || destination_metadata.nlink() != 2
                || staged_metadata.dev() != dev
                || destination_metadata.dev() != dev
                || staged_metadata.ino() != ino
                || destination_metadata.ino() != ino
            {
                return Err(StoreError::Corrupt);
            }
            drop(destination);
            drop(staged);
            unlinkat(directory, &stage_name, AtFlags::empty()).map_err(self::error)?;
        }
        directory.sync_all().map_err(|error| io_error(&error))?;
        Ok(())
    }

    fn remove_session_contents(
        directory: &File,
        store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        if let Some(store) = store {
            recover_session_aliases(store, directory)?;
        }
        let names = directory_names(directory)?;
        for name in &names {
            if name != LOCK_FILE && !valid_temp_name(name) {
                return Err(StoreError::Corrupt);
            }
            let file = open_file_at(directory, name, OFlags::RDONLY)?.ok_or(StoreError::Corrupt)?;
            drop(file);
        }
        for name in names {
            unlinkat(directory, &name, AtFlags::empty()).map_err(error)?;
        }
        directory.sync_all().map_err(|error| io_error(&error))
    }

    fn remove_session_at(
        parent: &File,
        name: &str,
        directory: &File,
        store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        remove_session_contents(directory, store)?;
        unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(error)?;
        parent.sync_all().map_err(|error| io_error(&error))
    }

    pub(in crate::durable) fn reap_stale_sessions(
        store: &FileStore,
        parent: &ArtifactDirectory,
    ) -> Result<(), StoreError> {
        for name in directory_names(&parent.file)? {
            if !name.starts_with("session-") {
                return Err(StoreError::Corrupt);
            }
            let directory = open_dir_at(parent, &name)?;
            let lease = open_file_at(&directory.file, LOCK_FILE, OFlags::RDWR)?
                .ok_or(StoreError::Corrupt)?;
            match lease.try_lock() {
                Ok(()) => {
                    remove_session_at(&parent.file, &name, &directory.file, Some(store))?;
                    drop(lease);
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(StoreError::Io(error.to_string()));
                }
            }
        }
        Ok(())
    }

    pub(in crate::durable) fn create_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
    ) -> Result<File, StoreError> {
        let file = openat(
            &directory.file,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(error)?;
        validate_single_link_file(&file)?;
        Ok(file)
    }

    pub(in crate::durable) fn unlink_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
    ) -> Result<(), StoreError> {
        unlinkat(&directory.file, name, AtFlags::empty()).map_err(error)
    }

    pub(in crate::durable) fn link_stage_object(
        staging: &ArtifactDirectory,
        staging_name: &str,
        source: &File,
        store: &FileStore,
        id: ObjectId,
    ) -> Result<bool, StoreError> {
        let staged = open_file_at(&staging.file, staging_name, OFlags::RDONLY)?
            .ok_or(StoreError::Corrupt)?;
        validate_single_link_file(&staged)?;
        let staged_metadata = staged.metadata().map_err(|error| io_error(&error))?;
        let source_metadata = source.metadata().map_err(|error| io_error(&error))?;
        if staged_metadata.dev() != source_metadata.dev()
            || staged_metadata.ino() != source_metadata.ino()
        {
            return Err(StoreError::Corrupt);
        }
        seal_immutable_file(source)?;
        let objects = store_dir(store, "objects")?;
        let name = format!("{}.object", super::super::hex(id.as_bytes()));
        match linkat(
            &staging.file,
            staging_name,
            &objects.file,
            &name,
            AtFlags::empty(),
        ) {
            Ok(()) => {
                objects.sync_all()?;
                super::wait_test_link_barrier();
                if take_test_fault(12) {
                    return Err(StoreError::Io(
                        "injected interruption after object link".to_owned(),
                    ));
                }
                unlink_stage_file(staging, staging_name)?;
                staging.sync_all()?;
                Ok(true)
            }
            Err(Errno::EXIST) => Ok(false),
            Err(error) => Err(self::error(error)),
        }
    }

    pub(in crate::durable) fn write_closure_descriptor(
        staging: &ArtifactDirectory,
        store: &FileStore,
        id: ClosureId,
        bytes: &[u8],
    ) -> Result<bool, StoreError> {
        let mut temp = create_stage_file(staging, CLOSURE_TEMP_FILE)?;
        let write_result = temp.write_all(bytes);
        if let Err(error) = write_result {
            let _ = unlink_stage_file(staging, CLOSURE_TEMP_FILE);
            return Err(io_error(&error));
        }
        seal_immutable_file(&temp)?;
        drop(temp);

        let closures = store_dir(store, "closures")?;
        let name = format!("{}.closure", super::super::hex(id.as_bytes()));
        let created = match linkat(
            &staging.file,
            CLOSURE_TEMP_FILE,
            &closures.file,
            &name,
            AtFlags::empty(),
        ) {
            Ok(()) => true,
            Err(Errno::EXIST) => {
                let existing = open_file_at(&closures.file, &name, OFlags::RDONLY)?
                    .ok_or(StoreError::Corrupt)?;
                seal_immutable_file(&existing)?;
                let mut existing_bytes = Vec::new();
                existing
                    .take(
                        u64::try_from(bytes.len())
                            .map_err(|_| StoreError::Bounds)?
                            .saturating_add(1),
                    )
                    .read_to_end(&mut existing_bytes)
                    .map_err(|error| io_error(&error))?;
                if existing_bytes != bytes {
                    return Err(StoreError::Corrupt);
                }
                false
            }
            Err(error) => return Err(self::error(error)),
        };
        if created {
            closures.sync_all()?;
            super::wait_test_link_barrier();
            if take_test_fault(13) {
                return Err(StoreError::Io(
                    "injected interruption after closure link".to_owned(),
                ));
            }
        }
        unlink_stage_file(staging, CLOSURE_TEMP_FILE)?;
        staging.sync_all()?;
        Ok(created)
    }

    pub(in crate::durable) fn cleanup_session(
        store: &FileStore,
        parent: &ArtifactDirectory,
        name: &str,
        directory: &ArtifactDirectory,
    ) -> Result<(), StoreError> {
        if simulate_process_crash() {
            return Ok(());
        }
        remove_session_at(&parent.file, name, &directory.file, Some(store))
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::ErrorKind;

    fn check_regular(path: &Path, file: &File) -> Result<(), StoreError> {
        let path_metadata = fs::symlink_metadata(path).map_err(|error| io_error(&error))?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !file.metadata().map_err(|error| io_error(&error))?.is_file()
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }

    fn check_directory(path: &Path) -> Result<File, StoreError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| io_error(&error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoreError::Corrupt);
        }
        File::open(path).map_err(|error| io_error(&error))
    }

    fn ensure_directory(path: &Path) -> Result<File, StoreError> {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(&error)),
        }
        check_directory(path)
    }

    fn read_directory_names(directory: &ArtifactDirectory) -> Result<Vec<String>, StoreError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&directory.path).map_err(|error| io_error(&error))? {
            let entry = entry.map_err(|error| io_error(&error))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| StoreError::Corrupt)?;
            if name != "." && name != ".." {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn valid_temp_name(name: &str) -> bool {
        name == CLOSURE_TEMP_FILE
            || name
                .strip_prefix("object-")
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                })
    }

    fn create_lease(path: &Path) -> Result<File, StoreError> {
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| io_error(&error))?;
        check_regular(path, &lease)?;
        lease
            .try_lock()
            .map_err(|error| StoreError::Io(error.to_string()))?;
        Ok(lease)
    }

    fn remove_session_contents(
        directory: &ArtifactDirectory,
        _store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        let names = read_directory_names(directory)?;
        for name in &names {
            if name != LOCK_FILE && !valid_temp_name(name) {
                return Err(StoreError::Corrupt);
            }
            let path = directory.path.join(&name);
            let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&error))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StoreError::Corrupt);
            }
        }
        for name in names {
            let path = directory.path.join(&name);
            fs::remove_file(path).map_err(|error| io_error(&error))?;
        }
        directory.sync_all()
    }

    fn remove_session_at(
        parent: &ArtifactDirectory,
        name: &str,
        directory: &ArtifactDirectory,
        store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        remove_session_contents(directory, store)?;
        fs::remove_dir(&directory.path).map_err(|error| io_error(&error))?;
        parent.sync_all()?;
        let _ = name;
        Ok(())
    }

    fn open_path(path: &Path) -> Result<Option<File>, StoreError> {
        let file = match OpenOptions::new().read(true).open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&error)),
        };
        check_regular(path, &file)?;
        Ok(Some(file))
    }

    pub(in crate::durable) fn open_object(
        store: &FileStore,
        id: ObjectId,
    ) -> Result<Option<File>, StoreError> {
        open_path(&store.object_path(id))
    }

    pub(in crate::durable) fn open_store_file(
        store: &FileStore,
        directory: &str,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        open_path(&store.root.join(directory).join(name))
    }

    pub(in crate::durable) fn open_closure(
        store: &FileStore,
        id: ClosureId,
    ) -> Result<File, StoreError> {
        open_path(&store.closure_path(id))?.ok_or(StoreError::Corrupt)
    }

    pub(in crate::durable) fn prepare_staging_root(
        store: &FileStore,
    ) -> Result<ArtifactDirectory, StoreError> {
        let root = check_directory(&store.root)?;
        let root_dir = ArtifactDirectory {
            file: root,
            path: store.root.clone(),
        };
        let staging_path = store.root.join("staging");
        let staging_file = ensure_directory(&staging_path)?;
        let staging = ArtifactDirectory {
            file: staging_file,
            path: staging_path,
        };
        let artifacts_path = staging.path.join("artifacts");
        let artifacts_file = ensure_directory(&artifacts_path)?;
        let artifacts = ArtifactDirectory {
            file: artifacts_file,
            path: artifacts_path,
        };
        root_dir.sync_all()?;
        staging.sync_all()?;
        Ok(artifacts)
    }

    pub(in crate::durable) fn create_session(
        parent: &ArtifactDirectory,
    ) -> Result<SessionDirectory, StoreError> {
        loop {
            let name = super::next_session_name();
            let path = parent.path.join(&name);
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_error(&error)),
            }
            let directory = ArtifactDirectory {
                file: check_directory(&path)?,
                path: path.clone(),
            };
            let lease = create_lease(&path.join(LOCK_FILE))?;
            directory.sync_all()?;
            parent.sync_all()?;
            return Ok(SessionDirectory {
                name,
                parent: parent.try_clone()?,
                directory,
                lease,
            });
        }
    }

    pub(in crate::durable) fn reap_stale_sessions(
        store: &FileStore,
        parent: &ArtifactDirectory,
    ) -> Result<(), StoreError> {
        for name in read_directory_names(parent)? {
            if !name.starts_with("session-") {
                return Err(StoreError::Corrupt);
            }
            let path = parent.path.join(&name);
            let directory = ArtifactDirectory {
                file: check_directory(&path)?,
                path: path.clone(),
            };
            let lease_path = path.join(LOCK_FILE);
            let metadata = fs::symlink_metadata(&lease_path).map_err(|error| io_error(&error))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StoreError::Corrupt);
            }
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lease_path)
                .map_err(|error| io_error(&error))?;
            match lease.try_lock() {
                Ok(()) => {
                    remove_session_at(parent, &name, &directory, Some(store))?;
                    drop(lease);
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(StoreError::Io(error.to_string()));
                }
            }
        }
        Ok(())
    }

    pub(in crate::durable) fn create_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
    ) -> Result<File, StoreError> {
        let path = directory.path.join(name);
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| io_error(&error))?;
        check_regular(&path, &file)?;
        Ok(file)
    }

    pub(in crate::durable) fn unlink_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
    ) -> Result<(), StoreError> {
        fs::remove_file(directory.path.join(name)).map_err(|error| io_error(&error))?;
        directory.sync_all()?;
        Ok(())
    }

    pub(in crate::durable) fn link_stage_object(
        staging: &ArtifactDirectory,
        staging_name: &str,
        source: &File,
        store: &FileStore,
        id: ObjectId,
    ) -> Result<bool, StoreError> {
        let staged_path = staging.path.join(staging_name);
        check_regular(&staged_path, source)?;
        let mut permissions = source
            .metadata()
            .map_err(|error| io_error(&error))?
            .permissions();
        permissions.set_readonly(true);
        source
            .set_permissions(permissions)
            .map_err(|error| io_error(&error))?;
        source.sync_all().map_err(|error| io_error(&error))?;
        let objects = store.root.join("objects");
        let destination = store.object_path(id);
        match fs::hard_link(&staged_path, &destination) {
            Ok(()) => {
                fs::File::open(&objects)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| io_error(&error))?;
                super::wait_test_link_barrier();
                if take_test_fault(12) {
                    return Err(StoreError::Io(
                        "injected interruption after object link".to_owned(),
                    ));
                }
                unlink_stage_file(staging, staging_name)?;
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(io_error(&error)),
        }
    }

    pub(in crate::durable) fn write_closure_descriptor(
        staging: &ArtifactDirectory,
        store: &FileStore,
        id: ClosureId,
        bytes: &[u8],
    ) -> Result<bool, StoreError> {
        let temp_path = staging.path.join(CLOSURE_TEMP_FILE);
        let mut temp = create_stage_file(staging, CLOSURE_TEMP_FILE)?;
        temp.write_all(bytes).map_err(|error| io_error(&error))?;
        let mut permissions = temp
            .metadata()
            .map_err(|error| io_error(&error))?
            .permissions();
        permissions.set_readonly(true);
        temp.set_permissions(permissions)
            .map_err(|error| io_error(&error))?;
        temp.sync_all().map_err(|error| io_error(&error))?;
        drop(temp);
        let destination = store.closure_path(id);
        let created = match fs::hard_link(&temp_path, &destination) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let existing = open_path(&destination)?.ok_or(StoreError::Corrupt)?;
                let mut existing_bytes = Vec::new();
                existing
                    .take(
                        u64::try_from(bytes.len())
                            .map_err(|_| StoreError::Bounds)?
                            .saturating_add(1),
                    )
                    .read_to_end(&mut existing_bytes)
                    .map_err(|error| io_error(&error))?;
                if existing_bytes != bytes {
                    return Err(StoreError::Corrupt);
                }
                false
            }
            Err(error) => return Err(io_error(&error)),
        };
        if created {
            fs::File::open(store.root.join("closures"))
                .and_then(|directory| directory.sync_all())
                .map_err(|error| io_error(&error))?;
            super::wait_test_link_barrier();
            if take_test_fault(13) {
                return Err(StoreError::Io(
                    "injected interruption after closure link".to_owned(),
                ));
            }
        }
        unlink_stage_file(staging, CLOSURE_TEMP_FILE)?;
        Ok(created)
    }

    pub(in crate::durable) fn cleanup_session(
        store: &FileStore,
        parent: &ArtifactDirectory,
        name: &str,
        directory: &ArtifactDirectory,
    ) -> Result<(), StoreError> {
        if simulate_process_crash() {
            return Ok(());
        }
        remove_session_at(parent, name, directory, Some(store))
    }
}

pub(super) use imp::{
    cleanup_session, create_session, create_stage_file, link_stage_object, open_closure,
    open_object, open_store_file, prepare_staging_root, reap_stale_sessions, unlink_stage_file,
    write_closure_descriptor,
};
