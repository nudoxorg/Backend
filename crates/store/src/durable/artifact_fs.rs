//! Descriptor-relative filesystem operations for the artifact stream.
//!
//! Artifact admission treats the store root as a configured trust boundary,
//! then resolves every mutable child relative to an opened directory with
//! symlink following disabled. CAS members must be regular files with exactly
//! one link; a link count above one would let another name mutate supposedly
//! immutable bytes.

use super::{ClosureId, FileStore, ObjectId, StoreError, io_error, lock_error};
use crate::UntrustedObjectId;
#[cfg(not(unix))]
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
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

fn valid_session_name(name: &str) -> bool {
    let Some((pid, ordinal)) = name
        .strip_prefix("session-")
        .and_then(|name| name.split_once('-'))
    else {
        return false;
    };
    pid.parse::<u32>()
        .is_ok_and(|value| value != 0 && value.to_string() == pid)
        && ordinal
            .parse::<u64>()
            .is_ok_and(|value| value.to_string() == ordinal)
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

/// Test-owned trees can contain sealed CAS members, aliases, and interrupted staging copies.
/// Keep cleanup alive through assertion unwinding, including before a fixture open succeeds.
#[cfg(test)]
pub(in crate::durable) struct TestDirectoryCleanup(PathBuf);

#[cfg(test)]
impl TestDirectoryCleanup {
    pub(in crate::durable) fn new(path: &std::path::Path) -> Self {
        Self(path.to_path_buf())
    }
}

#[cfg(test)]
impl Drop for TestDirectoryCleanup {
    fn drop(&mut self) {
        let _ = remove_test_directory(&self.0);
    }
}

#[cfg(test)]
pub(in crate::durable) fn remove_test_file(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let file = backend_platform::durability::open_regular_file_nofollow(path)?;
        backend_platform::durability::unlink_file_preserving_readonly(path, &file)
    }
    #[cfg(not(windows))]
    std::fs::remove_file(path)
}

/// Recurses only through ordinary directories in a test-owned tree. Reparse points are
/// removed as links, and regular files are unlinked by identity without clearing attributes.
#[cfg(test)]
pub(in crate::durable) fn remove_test_directory(
    path: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    #[cfg(windows)]
    {
        use std::os::windows::fs::{FileTypeExt as _, MetadataExt as _};
        const REPARSE_POINT: u32 = 0x400;
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_attributes() & REPARSE_POINT != 0 {
            return if metadata.is_dir() || metadata.file_type().is_symlink_dir() {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            };
        }
        if !metadata.is_dir() {
            return remove_test_file(path);
        }
        for entry in std::fs::read_dir(path)? {
            remove_test_directory(entry?.path())?;
        }
        std::fs::remove_dir(path)
    }
    #[cfg(not(windows))]
    std::fs::remove_dir_all(path)
}

/// Private provenance for the particular inode that completed the stream's barriers.
/// Kept inside the pinned receive session; admission borrows it together with that GC pin.
#[derive(Clone, Copy)]
pub(super) struct DurableObjectFile {
    object: ObjectId,
    identity: backend_platform::FileIdentity,
}

impl DurableObjectFile {
    pub(super) fn matches(&self, object: ObjectId, file: &File) -> Result<bool, StoreError> {
        Ok(self.object == object
            && self.identity
                == backend_platform::FileIdentity::of_file(file)
                    .map_err(|error| io_error(&error))?)
    }
}

#[cfg(not(windows))]
fn linked_object_receipt(
    source: &File,
    store: &FileStore,
    id: ObjectId,
) -> Result<DurableObjectFile, StoreError> {
    let destination = open_object(store, id)?.ok_or(StoreError::Corrupt)?;
    let identity =
        backend_platform::FileIdentity::of_file(source).map_err(|error| io_error(&error))?;
    if identity
        != backend_platform::FileIdentity::of_file(&destination)
            .map_err(|error| io_error(&error))?
    {
        return Err(StoreError::UnsafePath);
    }
    Ok(DurableObjectFile {
        object: id,
        identity,
    })
}

#[cfg(unix)]
mod imp {
    use super::*;
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, fchmod, linkat, mkdirat, open, openat, unlinkat};
    use rustix::io::Errno;
    use std::os::unix::fs::MetadataExt;

    fn error(error: Errno) -> StoreError {
        io_error(&std::io::Error::from(error))
    }

    fn path_error(error: Errno) -> StoreError {
        if error == Errno::LOOP || error == Errno::NOTDIR {
            StoreError::UnsafePath
        } else {
            self::error(error)
        }
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
        .map_err(path_error)
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
        .map_err(path_error)
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
            return Err(StoreError::UnsafePath);
        }
        Ok(())
    }

    fn seal_immutable_file(file: &File) -> Result<(), StoreError> {
        validate_single_link_file(file)?;
        fchmod(file, Mode::RUSR).map_err(error)?;
        file.sync_all().map_err(|error| io_error(&error))
    }

    fn seal_immutable_read(file: &File) -> Result<(), StoreError> {
        // Inspect the same descriptor that will supply the bytes. Already sealed
        // immutable reads must not dirty inode metadata or flush the filesystem.
        let metadata = file.metadata().map_err(|error| io_error(&error))?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(StoreError::UnsafePath);
        }
        if metadata.mode() & 0o7777 == 0o400 {
            return Ok(());
        }
        // Legacy members still become durable, owner-read-only files once.
        // Publication uses seal_immutable_file unconditionally.
        seal_immutable_file(file)
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
                        return Err(StoreError::UnsafePath);
                    }
                    if metadata.nlink() == 1 {
                        return Ok(Some(file));
                    }
                    if metadata.nlink() != 2 || attempt + 1 == LINK_SETTLE_ATTEMPTS {
                        return Err(StoreError::UnsafePath);
                    }
                }
                Err(error) if is_missing(error) => return Ok(None),
                Err(error) => {
                    return Err(path_error(error));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(StoreError::UnsafePath)
    }

    fn open_immutable_child_file(
        store: &FileStore,
        directory: &str,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        let parent = store_dir(store, directory)?;
        let file = open_file_at(&parent.file, name, OFlags::RDONLY)?;
        if let Some(file) = &file {
            seal_immutable_read(file)?;
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
            let name = next_session_name();
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
                lease.try_lock().map_err(lock_error)?;
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
            .map_err(error)?;
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
            .map_err(error)?;
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
            .map_err(error)?;
            let destination_directory = store_dir(store, &destination_directory_name)?;
            let destination = openat(
                &destination_directory.file,
                &destination_name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(error)?;
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
            unlinkat(directory, &stage_name, AtFlags::empty()).map_err(error)?;
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
        // Keep the lease until every payload removal is durable. An interrupted
        // retirement may leave only an empty, lease-less directory, never
        // unleased payloads that a later writer could mistake for abandoned data.
        for name in names.iter().filter(|name| name.as_str() != LOCK_FILE) {
            unlinkat(directory, name.as_str(), AtFlags::empty()).map_err(error)?;
        }
        directory.sync_all().map_err(|error| io_error(&error))?;
        if names.iter().any(|name| name == LOCK_FILE) {
            unlinkat(directory, LOCK_FILE, AtFlags::empty()).map_err(error)?;
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
        #[cfg(test)]
        if take_test_fault(17) {
            wait_test_link_barrier();
        }
        unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(error)?;
        parent.sync_all().map_err(|error| io_error(&error))
    }

    pub(in crate::durable) fn reap_stale_sessions(
        store: &FileStore,
        parent: &ArtifactDirectory,
    ) -> Result<(), StoreError> {
        for name in directory_names(&parent.file)? {
            if !valid_session_name(&name) {
                return Err(StoreError::Corrupt);
            }
            let directory = open_dir_at(parent, &name)?;
            let Some(lease) = open_file_at(&directory.file, LOCK_FILE, OFlags::RDWR)? else {
                // The caller holds the process lock, as do session creation and
                // teardown. Only an empty directory can be a completed cleanup
                // or interrupted mkdir; an unleased payload remains corruption.
                if !directory_names(&directory.file)?.is_empty() {
                    return Err(StoreError::Corrupt);
                }
                let current = open_dir_at(parent, &name)?;
                let held = directory
                    .file
                    .metadata()
                    .map_err(|error| io_error(&error))?;
                let named = current.file.metadata().map_err(|error| io_error(&error))?;
                if held.dev() != named.dev() || held.ino() != named.ino() {
                    return Err(StoreError::UnsafePath);
                }
                unlinkat(&parent.file, &name, AtFlags::REMOVEDIR).map_err(error)?;
                parent.sync_all()?;
                continue;
            };
            match lease.try_lock() {
                Ok(()) => {
                    remove_session_at(&parent.file, &name, &directory.file, Some(store))?;
                    drop(lease);
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(io_error(&error));
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
        expected: &File,
    ) -> Result<(), StoreError> {
        let staged =
            open_file_at(&directory.file, name, OFlags::RDONLY)?.ok_or(StoreError::Corrupt)?;
        let staged_metadata = staged.metadata().map_err(|error| io_error(&error))?;
        let expected_metadata = expected.metadata().map_err(|error| io_error(&error))?;
        if staged_metadata.dev() != expected_metadata.dev()
            || staged_metadata.ino() != expected_metadata.ino()
        {
            return Err(StoreError::UnsafePath);
        }
        drop(staged);
        unlinkat(&directory.file, name, AtFlags::empty()).map_err(error)
    }

    // A successful publication intentionally gives the staged inode two names until
    // the staging alias is removed. Do not use open_file_at here: it waits for
    // nlink == 1, which only this unlink can make true. Verify both held names
    // against the caller's descriptor and require the exact expected link count.
    fn unlink_published_stage_file(
        staging: &ArtifactDirectory,
        staging_name: &str,
        destination: &ArtifactDirectory,
        destination_name: &str,
        source: &File,
    ) -> Result<(), StoreError> {
        let staged = openat(
            &staging.file,
            staging_name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(path_error)?;
        let published = openat(
            &destination.file,
            destination_name,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(path_error)?;
        let staged_metadata = staged.metadata().map_err(|error| io_error(&error))?;
        let published_metadata = published.metadata().map_err(|error| io_error(&error))?;
        let source_metadata = source.metadata().map_err(|error| io_error(&error))?;
        let same_inode = |metadata: &std::fs::Metadata| {
            metadata.dev() == source_metadata.dev() && metadata.ino() == source_metadata.ino()
        };
        if !staged_metadata.is_file()
            || !published_metadata.is_file()
            || !same_inode(&staged_metadata)
            || !same_inode(&published_metadata)
            || staged_metadata.nlink() != 2
            || published_metadata.nlink() != 2
            || source_metadata.nlink() != 2
        {
            return Err(StoreError::UnsafePath);
        }
        drop(published);
        drop(staged);
        unlinkat(&staging.file, staging_name, AtFlags::empty()).map_err(error)
    }

    pub(in crate::durable) fn link_stage_object(
        staging: &ArtifactDirectory,
        staging_name: &str,
        source: &File,
        store: &FileStore,
        id: ObjectId,
    ) -> Result<Option<DurableObjectFile>, StoreError> {
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
                wait_test_link_barrier();
                if take_test_fault(12) {
                    return Err(StoreError::Io(
                        "injected interruption after object link".to_owned(),
                    ));
                }
                unlink_published_stage_file(staging, staging_name, &objects, &name, source)?;
                staging.sync_all()?;
                linked_object_receipt(source, store, id).map(Some)
            }
            Err(Errno::EXIST) => Ok(None),
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
            let _ = unlink_stage_file(staging, CLOSURE_TEMP_FILE, &temp);
            return Err(io_error(&error));
        }
        seal_immutable_file(&temp)?;
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
            wait_test_link_barrier();
            if take_test_fault(13) {
                return Err(StoreError::Io(
                    "injected interruption after closure link".to_owned(),
                ));
            }
            unlink_published_stage_file(staging, CLOSURE_TEMP_FILE, &closures, &name, &temp)?;
        } else {
            unlink_stage_file(staging, CLOSURE_TEMP_FILE, &temp)?;
        }
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

    #[cfg(test)]
    mod immutable_read_tests {
        #![allow(clippy::expect_used)]
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        struct Fixture {
            store: FileStore,
            root: PathBuf,
            path: PathBuf,
        }
        impl Fixture {
            fn new(mode: u32) -> Self {
                let root = std::env::temp_dir().join(format!(
                    "backend-sealed-read-{}-{}",
                    std::process::id(),
                    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
                ));
                let store = FileStore::open(&root, 1 << 20).expect("open private store");
                let path = root.join("nodes/read.ref");
                std::fs::write(&path, b"descriptor fixture").expect("write fixture");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                    .expect("set exact fixture mode");
                Self { store, root, path }
            }
            fn read(&self) -> Result<File, StoreError> {
                open_immutable_child_file(&self.store, "nodes", "read.ref")?
                    .ok_or(StoreError::Corrupt)
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
        fn identity(metadata: &std::fs::Metadata) -> (u64, u64, i64, i64) {
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        }

        fn publication_directories(store: &FileStore) -> (ArtifactDirectory, ArtifactDirectory) {
            let root = ArtifactDirectory {
                file: store_root(store).expect("open store root"),
                path: store.root.clone(),
            };
            (
                open_dir_at(&root, "nodes").expect("open staging fixture directory"),
                store_dir(store, "objects").expect("open object directory"),
            )
        }

        #[test]
        fn published_stage_unlink_accepts_only_the_verified_two_link_transition() {
            let fixture = Fixture::new(0o600);
            let (staging, destination) = publication_directories(&fixture.store);
            let mut source = create_stage_file(&staging, "stage.tmp").expect("stage file");
            source.write_all(b"published bytes").expect("stage payload");
            seal_immutable_file(&source).expect("seal staged inode");
            let published_name = "publication-test.object";
            linkat(
                &staging.file,
                "stage.tmp",
                &destination.file,
                published_name,
                AtFlags::empty(),
            )
            .expect("create CAS hard link");
            assert_eq!(source.metadata().expect("two-link inode").nlink(), 2);

            unlink_published_stage_file(
                &staging,
                "stage.tmp",
                &destination,
                published_name,
                &source,
            )
            .expect("remove only the verified staging alias");
            assert!(
                openat(
                    &staging.file,
                    "stage.tmp",
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .is_err()
            );
            let published = open_file_at(&destination.file, published_name, OFlags::RDONLY)
                .expect("open one-link published object")
                .expect("published object retained");
            assert_eq!(published.metadata().expect("published metadata").nlink(), 1);
        }

        #[test]
        fn published_stage_unlink_rejects_a_replaced_symlink_name() {
            let fixture = Fixture::new(0o600);
            let (staging, destination) = publication_directories(&fixture.store);
            let mut source = create_stage_file(&staging, "stage.tmp").expect("stage file");
            source.write_all(b"published bytes").expect("stage payload");
            seal_immutable_file(&source).expect("seal staged inode");
            let published_name = "publication-test.object";
            linkat(
                &staging.file,
                "stage.tmp",
                &destination.file,
                published_name,
                AtFlags::empty(),
            )
            .expect("create CAS hard link");
            unlinkat(&staging.file, "stage.tmp", AtFlags::empty())
                .expect("remove original staging name");
            std::os::unix::fs::symlink(
                fixture.store.root.join("objects").join(published_name),
                fixture.store.root.join("nodes/stage.tmp"),
            )
            .expect("replace stage name with symlink");
            assert!(matches!(
                unlink_published_stage_file(
                    &staging,
                    "stage.tmp",
                    &destination,
                    published_name,
                    &source,
                ),
                Err(StoreError::UnsafePath)
            ));
            assert!(
                std::fs::symlink_metadata(fixture.store.root.join("nodes/stage.tmp"))
                    .expect("symlink remains untouched")
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                open_file_at(&destination.file, published_name, OFlags::RDONLY)
                    .expect("open retained publication")
                    .expect("publication remains")
                    .metadata()
                    .expect("published metadata")
                    .nlink(),
                1
            );
        }

        #[test]
        fn published_stage_unlink_rejects_an_unrelated_third_link() {
            let fixture = Fixture::new(0o600);
            let (staging, destination) = publication_directories(&fixture.store);
            let mut source = create_stage_file(&staging, "stage.tmp").expect("stage file");
            source.write_all(b"published bytes").expect("stage payload");
            seal_immutable_file(&source).expect("seal staged inode");
            let published_name = "publication-test.object";
            linkat(
                &staging.file,
                "stage.tmp",
                &destination.file,
                published_name,
                AtFlags::empty(),
            )
            .expect("create CAS hard link");
            std::fs::hard_link(
                fixture.store.root.join("nodes/stage.tmp"),
                fixture.store.root.join("external-link"),
            )
            .expect("create unrelated alias");
            assert_eq!(source.metadata().expect("three-link inode").nlink(), 3);
            assert!(matches!(
                unlink_published_stage_file(
                    &staging,
                    "stage.tmp",
                    &destination,
                    published_name,
                    &source,
                ),
                Err(StoreError::UnsafePath)
            ));
            assert_eq!(source.metadata().expect("still linked inode").nlink(), 3);
        }

        #[test]
        fn already_sealed_read_preserves_inode_ctime() {
            let fixture = Fixture::new(0o400);
            let before = std::fs::metadata(&fixture.path).expect("before read");
            std::thread::sleep(std::time::Duration::from_millis(5));
            eprintln!("SEALED_READ_BEGIN");
            let file = fixture.read().expect("read sealed descriptor");
            eprintln!("SEALED_READ_END");
            assert_eq!(
                identity(&before),
                identity(&file.metadata().expect("same fd metadata"))
            );
        }

        #[test]
        fn legacy_read_tightens_all_permission_bits_once() {
            for mode in [0o600, 0o444, 0o4400, 0o1400, 0o2400] {
                let fixture = Fixture::new(mode);
                let file = fixture.read().expect("seal legacy descriptor");
                let sealed = file.metadata().expect("sealed metadata");
                assert_eq!(sealed.mode() & 0o7777, 0o400);
                std::thread::sleep(std::time::Duration::from_millis(5));
                let reopened = fixture.read().expect("read newly sealed descriptor");
                assert_eq!(
                    identity(&sealed),
                    identity(&reopened.metadata().expect("reopened metadata"))
                );
            }
        }

        #[test]
        fn sealed_object_admission_survives_concurrent_pinned_reopen_and_rejects_corruption() {
            use backend_version::{ObjectKey, Schema};
            struct Bytes;
            impl Schema for Bytes {
                const DOMAIN: u8 = 0xf2;
                const TYPE: u16 = 0x51f0;
                type Value = Vec<u8>;
                fn encode(value: &Self::Value, output: &mut Vec<u8>) {
                    output.extend_from_slice(value);
                }
            }
            let fixture = Fixture::new(0o400);
            let bytes = b"checked sealed immutable payload".to_vec();
            let object = crate::durable::TypedObject::from_value(
                &ObjectKey::<Bytes>::from_value(&bytes),
                &bytes,
            );
            fixture
                .store
                .write_object(&object)
                .expect("publish sealed object");
            let path = fixture.store.object_path(object.id());
            let before = std::fs::metadata(&path).expect("freshly published metadata");
            assert_eq!(before.mode() & 0o7777, 0o400);
            assert_eq!(before.nlink(), 1);
            std::thread::sleep(std::time::Duration::from_millis(5));
            // Exercise the first admission of the fresh writer's inode. A
            // prewarming read would hide a redundant permission/write barrier.
            assert_eq!(
                fixture
                    .store
                    .read_object(object.id())
                    .expect("first admission"),
                object
            );
            assert_eq!(
                identity(&before),
                identity(&std::fs::metadata(&path).expect("after first admission"))
            );
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    let root = fixture.root.clone();
                    let expected = object.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let store = FileStore::open(root, 1 << 20).expect("cold reopen");
                        let pin = store.pin_garbage_collection().expect("pin reopened store");
                        barrier.wait();
                        assert!(
                            !store
                                .write_object_with_receipt(&expected)
                                .expect("share existing immutable inode")
                                .created()
                        );
                        for _ in 0..20 {
                            assert_eq!(
                                store.read_object(expected.id()).expect("owned admission"),
                                expected
                            );
                            store
                                .with_verified_object_pinned(&pin, expected.id(), |view| {
                                    assert_eq!(view.bytes(), expected.bytes());
                                    Ok(())
                                })
                                .expect("pinned borrowed admission");
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker.join().expect("concurrent admitted reads");
            }
            assert_eq!(
                identity(&before),
                identity(&std::fs::metadata(&path).expect("after reads"))
            );
            let mut envelope = std::fs::read(&path).expect("read envelope");
            let last = envelope.len() - 1;
            envelope[last] ^= 1;
            std::fs::remove_file(&path).expect("remove fixture object");
            std::fs::write(&path, envelope).expect("replace corrupt fixture");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
                .expect("seal corrupt fixture");
            assert_eq!(
                fixture.store.write_object(&object),
                Err(StoreError::Corrupt)
            );
            assert_eq!(
                fixture.store.read_object(object.id()),
                Err(StoreError::Corrupt)
            );
            assert_eq!(
                fixture.store.with_verified_object(object.id(), |_| Ok(())),
                Err(StoreError::Corrupt)
            );
        }

        #[test]
        fn sealed_read_still_refuses_symlink_and_hardlink() {
            let fixture = Fixture::new(0o400);
            let alias = fixture.root.join("alias");
            std::fs::hard_link(&fixture.path, &alias).expect("hostile hardlink");
            assert!(matches!(fixture.read(), Err(StoreError::UnsafePath)));
            std::fs::remove_file(&alias).expect("remove alias");
            std::fs::rename(&fixture.path, &alias).expect("move fixture");
            std::os::unix::fs::symlink(&alias, &fixture.path).expect("hostile symlink");
            assert!(matches!(fixture.read(), Err(StoreError::UnsafePath)));
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;
    use std::fs;
    #[cfg(not(windows))]
    use std::fs::OpenOptions;
    use std::io::ErrorKind;

    fn check_regular(path: &Path, file: &File) -> Result<(), StoreError> {
        let path_metadata = fs::symlink_metadata(path).map_err(|error| io_error(&error))?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !file.metadata().map_err(|error| io_error(&error))?.is_file()
        {
            return Err(StoreError::UnsafePath);
        }
        // A type check alone does not establish that `file` is the file named by path.
        // The no-follow opener rejects every Windows reparse point on its actual handle.
        let named = backend_platform::durability::open_regular_file_nofollow(path)
            .map_err(|error| io_error(&error))?;
        if backend_platform::FileIdentity::of_file(&named).map_err(|error| io_error(&error))?
            != backend_platform::FileIdentity::of_file(file).map_err(|error| io_error(&error))?
        {
            return Err(StoreError::UnsafePath);
        }
        Ok(())
    }

    fn check_directory(path: &Path) -> Result<File, StoreError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| io_error(&error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoreError::UnsafePath);
        }
        // Windows refuses `File::open` on a directory and refuses to flush a
        // read-only directory handle; the platform seam opens one that can be
        // flushed, rejects reparse points, and checks the type on that handle.
        backend_platform::durability::open_directory_nofollow(path)
            .map_err(|error| io_error(&error))
    }

    #[cfg(not(windows))]
    fn ensure_directory(path: &Path) -> Result<File, StoreError> {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(&error)),
        }
        check_directory(path)
    }

    fn read_directory_names(directory: &ArtifactDirectory) -> Result<Vec<String>, StoreError> {
        #[cfg(windows)]
        return backend_platform::durability::directory_child_names(&directory.file)
            .map_err(|error| io_error(&error));
        #[cfg(not(windows))]
        {
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

    #[cfg(not(windows))]
    fn create_lease(path: &Path) -> Result<File, StoreError> {
        let lease = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| io_error(&error))?;
        check_regular(path, &lease)?;
        lease.try_lock().map_err(lock_error)?;
        Ok(lease)
    }

    fn remove_session_contents(
        directory: &ArtifactDirectory,
        _store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        #[cfg(windows)]
        {
            backend_platform::durability::unlink_directory_files_preserving_readonly(
                &directory.file,
                |name| name == LOCK_FILE || valid_temp_name(name),
            )
            .map_err(|error| io_error(&error))?;
            return directory.sync_all();
        }
        #[cfg(not(windows))]
        {
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
                remove_stage_path(&path)?;
            }
            directory.sync_all()
        }
    }

    fn remove_session_at(
        parent: &ArtifactDirectory,
        name: &str,
        directory: &ArtifactDirectory,
        store: Option<&FileStore>,
    ) -> Result<(), StoreError> {
        remove_session_contents(directory, store)?;
        #[cfg(test)]
        if take_test_fault(17) {
            wait_test_link_barrier();
        }
        #[cfg(windows)]
        backend_platform::durability::remove_empty_directory_child(
            &parent.file,
            name,
            &directory.file,
        )
        .map_err(|error| io_error(&error))?;
        #[cfg(not(windows))]
        fs::remove_dir(&directory.path).map_err(|error| io_error(&error))?;
        parent.sync_all()?;
        let _ = name;
        Ok(())
    }

    #[cfg(any(not(windows), test))]
    fn open_path(path: &Path) -> Result<Option<File>, StoreError> {
        for attempt in 0..100 {
            let file = match backend_platform::durability::open_regular_file_nofollow(path) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(io_error(&error)),
            };
            check_regular(path, &file)?;
            match backend_platform::file_identity::number_of_links(&file)
                .map_err(|error| io_error(&error))?
            {
                1 => return Ok(Some(file)),
                2 if attempt < 99 => std::thread::sleep(std::time::Duration::from_millis(1)),
                _ => return Err(StoreError::UnsafePath),
            }
        }
        Err(StoreError::UnsafePath)
    }

    pub(in crate::durable) fn open_object(
        store: &FileStore,
        id: ObjectId,
    ) -> Result<Option<File>, StoreError> {
        let name = format!("{}.object", super::super::hex(id.as_bytes()));
        open_store_file(store, "objects", &name)
    }

    #[cfg(windows)]
    pub(in crate::durable) fn open_directory_file(
        parent: &File,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        for attempt in 0..100 {
            let file = match backend_platform::durability::open_regular_file_child_nofollow(
                parent, name,
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(io_error(&error)),
            };
            match backend_platform::file_identity::number_of_links(&file)
                .map_err(|error| io_error(&error))?
            {
                1 => return Ok(Some(file)),
                2 if attempt < 99 => std::thread::sleep(std::time::Duration::from_millis(1)),
                _ => return Err(StoreError::UnsafePath),
            }
        }
        Err(StoreError::UnsafePath)
    }

    pub(in crate::durable) fn open_store_file(
        store: &FileStore,
        directory: &str,
        name: &str,
    ) -> Result<Option<File>, StoreError> {
        #[cfg(windows)]
        {
            let root = backend_platform::durability::open_directory_readonly_nofollow(&store.root)
                .map_err(|error| io_error(&error))?;
            let parent = backend_platform::durability::open_directory_readonly_child_nofollow(
                &root, directory,
            )
            .map_err(|error| io_error(&error))?;
            return open_directory_file(&parent, name);
        }
        #[cfg(not(windows))]
        open_path(&store.root.join(directory).join(name))
    }

    pub(in crate::durable) fn open_closure(
        store: &FileStore,
        id: ClosureId,
    ) -> Result<File, StoreError> {
        let name = format!("{}.closure", super::super::hex(id.as_bytes()));
        open_store_file(store, "closures", &name)?.ok_or(StoreError::Corrupt)
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
        #[cfg(windows)]
        let staging_file = ensure_child_directory(&root_dir.file, "staging")?;
        #[cfg(not(windows))]
        let staging_file = ensure_directory(&staging_path)?;
        let staging = ArtifactDirectory {
            file: staging_file,
            path: staging_path,
        };
        let artifacts_path = staging.path.join("artifacts");
        #[cfg(windows)]
        let artifacts_file = ensure_child_directory(&staging.file, "artifacts")?;
        #[cfg(not(windows))]
        let artifacts_file = ensure_directory(&artifacts_path)?;
        let artifacts = ArtifactDirectory {
            file: artifacts_file,
            path: artifacts_path,
        };
        root_dir.sync_all()?;
        staging.sync_all()?;
        Ok(artifacts)
    }

    #[cfg(windows)]
    fn ensure_child_directory(parent: &File, name: &str) -> Result<File, StoreError> {
        match backend_platform::durability::create_directory_child_nofollow(parent, name) {
            Ok(file) => Ok(file),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                backend_platform::durability::open_directory_child_nofollow(parent, name)
                    .map_err(|error| io_error(&error))
            }
            Err(error) => Err(io_error(&error)),
        }
    }

    pub(in crate::durable) fn create_session(
        parent: &ArtifactDirectory,
    ) -> Result<SessionDirectory, StoreError> {
        let parent_copy = parent.try_clone()?;
        loop {
            let name = super::next_session_name();
            let path = parent.path.join(&name);
            #[cfg(windows)]
            let session_file = match backend_platform::durability::create_directory_child_nofollow(
                &parent.file,
                &name,
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_error(&error)),
            };
            #[cfg(not(windows))]
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(io_error(&error)),
            }
            let directory = ArtifactDirectory {
                #[cfg(windows)]
                file: session_file,
                #[cfg(not(windows))]
                file: check_directory(&path)?,
                path: path.clone(),
            };
            #[cfg(windows)]
            let lease = {
                let result = (|| {
                    let lease = backend_platform::durability::create_regular_file_child_nofollow(
                        &directory.file,
                        LOCK_FILE,
                    )
                    .map_err(|error| io_error(&error))?;
                    lease.try_lock().map_err(lock_error)?;
                    directory.sync_all()?;
                    parent.sync_all()?;
                    Ok(lease)
                })();
                match result {
                    Ok(lease) => lease,
                    Err(error) => {
                        let _ = remove_session_at(parent, &name, &directory, None);
                        return Err(error);
                    }
                }
            };
            #[cfg(not(windows))]
            let lease = create_lease(&path.join(LOCK_FILE))?;
            #[cfg(not(windows))]
            directory.sync_all()?;
            #[cfg(not(windows))]
            parent.sync_all()?;
            return Ok(SessionDirectory {
                name,
                parent: parent_copy,
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
            if !valid_session_name(&name) {
                return Err(StoreError::Corrupt);
            }
            let path = parent.path.join(&name);
            let directory = ArtifactDirectory {
                #[cfg(windows)]
                file: backend_platform::durability::open_directory_child_nofollow(
                    &parent.file,
                    &name,
                )
                .map_err(|error| io_error(&error))?,
                #[cfg(not(windows))]
                file: check_directory(&path)?,
                path: path.clone(),
            };
            let lease_path = path.join(LOCK_FILE);
            let metadata = match fs::symlink_metadata(&lease_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if !read_directory_names(&directory)?.is_empty() {
                        return Err(StoreError::Corrupt);
                    }
                    // Creation and cleanup hold the process lock. A lease-less
                    // directory is recoverable only when it contains no payload.
                    #[cfg(windows)]
                    backend_platform::durability::remove_empty_directory_child(
                        &parent.file,
                        &name,
                        &directory.file,
                    )
                    .map_err(|error| io_error(&error))?;
                    #[cfg(not(windows))]
                    {
                        check_directory(&path)?;
                        fs::remove_dir(&path).map_err(|error| io_error(&error))?;
                    }
                    parent.sync_all()?;
                    continue;
                }
                Err(error) => return Err(io_error(&error)),
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(StoreError::Corrupt);
            }
            #[cfg(windows)]
            let lease = backend_platform::durability::open_regular_file_readwrite_child_nofollow(
                &directory.file,
                LOCK_FILE,
            )
            .map_err(|error| io_error(&error))?;
            #[cfg(not(windows))]
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
                    return Err(io_error(&error));
                }
            }
        }
        Ok(())
    }

    pub(in crate::durable) fn create_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
    ) -> Result<File, StoreError> {
        #[cfg(windows)]
        return backend_platform::durability::create_regular_file_child_nofollow(
            &directory.file,
            name,
        )
        .map_err(|error| io_error(&error));
        #[cfg(not(windows))]
        {
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
    }

    pub(in crate::durable) fn unlink_stage_file(
        directory: &ArtifactDirectory,
        name: &str,
        expected: &File,
    ) -> Result<(), StoreError> {
        #[cfg(windows)]
        {
            backend_platform::durability::unlink_file_preserving_readonly_at(
                &directory.file,
                name,
                expected,
            )
            .map_err(|error| io_error(&error))?;
        }
        #[cfg(not(windows))]
        {
            let _ = expected;
            remove_stage_path(&directory.path.join(name))?;
        }
        directory.sync_all()?;
        Ok(())
    }

    #[cfg(not(windows))]
    fn remove_stage_path(path: &Path) -> Result<(), StoreError> {
        fs::remove_file(path).map_err(|error| io_error(&error))
    }

    /// Marks an object's bytes immutable and flushes them. The read-only
    /// attribute is the closest portable seal; a hard link to the sealed file
    /// shares it, so the published name is sealed too.
    fn seal_immutable_file(file: &File) -> Result<(), StoreError> {
        if backend_platform::file_identity::number_of_links(file)
            .map_err(|error| io_error(&error))?
            != 1
        {
            return Err(StoreError::UnsafePath);
        }
        let mut permissions = file
            .metadata()
            .map_err(|error| io_error(&error))?
            .permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
            .map_err(|error| io_error(&error))?;
        file.sync_all().map_err(|error| io_error(&error))
    }

    /// One lazily created, leased private directory for an admission's bounded repair work.
    /// The admission borrows its existing GC pin; this never opens a per-member GC lease.
    #[cfg(windows)]
    pub(in crate::durable) struct DurabilityRepair<'store> {
        store: &'store FileStore,
        _pin: &'store super::super::layout::GcPinLease,
        session: SessionDirectory,
        next: AtomicU64,
    }

    #[cfg(windows)]
    impl<'store> DurabilityRepair<'store> {
        pub(in crate::durable) fn new(
            store: &'store FileStore,
            pin: &'store super::super::layout::GcPinLease,
        ) -> Result<Self, StoreError> {
            if !pin.covers_root(&store.root)? {
                return Err(StoreError::UnsafePath);
            }
            let parent = prepare_staging_root(store)?;
            let session = create_session(&parent)?;
            Ok(Self {
                store,
                _pin: pin,
                session,
                next: AtomicU64::new(0),
            })
        }

        /// Copies at most `maximum` bytes, verifies that private copy, then seals and flushes
        /// its actual writable handle before a single extended replacing rename. The old
        /// inode is never made writable, including on any failure path.
        pub(in crate::durable) fn replace_verified(
            &self,
            destination_parent: &File,
            name: &str,
            source: &mut File,
            maximum: u64,
            verify: impl FnOnce(File) -> Result<(), StoreError>,
        ) -> Result<(), StoreError> {
            let ordinal = self
                .next
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .map_err(|_| StoreError::Bounds)?;
            let stage_name = format!("object-{ordinal}.tmp");
            let mut copied = create_stage_file(&self.session.directory, &stage_name)?;
            let result = (|| {
                let length = source.metadata().map_err(|error| io_error(&error))?.len();
                if length > maximum {
                    return Err(StoreError::Bounds);
                }
                source
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| io_error(&error))?;
                let mut remaining = length;
                let mut buffer = [0_u8; super::super::artifact::VERIFY_BUFFER_BYTES];
                while remaining != 0 {
                    let count = usize::try_from(remaining.min(buffer.len() as u64))
                        .map_err(|_| StoreError::Bounds)?;
                    source
                        .read_exact(&mut buffer[..count])
                        .map_err(|error| io_error(&error))?;
                    copied
                        .write_all(&buffer[..count])
                        .map_err(|error| io_error(&error))?;
                    remaining -= count as u64;
                }
                let mut extra = [0_u8; 1];
                if source.read(&mut extra).map_err(|error| io_error(&error))? != 0 {
                    return Err(StoreError::Corrupt);
                }
                copied
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| io_error(&error))?;
                verify(copied.try_clone().map_err(|error| io_error(&error))?)?;
                if take_test_fault(31) {
                    return Err(StoreError::Io(
                        "injected failure before immutable repair barrier".to_owned(),
                    ));
                }
                seal_immutable_file(&copied)?;
                let named = backend_platform::durability::open_regular_file_child_nofollow(
                    destination_parent,
                    name,
                )
                .map_err(|error| io_error(&error))?;
                if backend_platform::FileIdentity::of_file(&named)
                    .map_err(|error| io_error(&error))?
                    != backend_platform::FileIdentity::of_file(source)
                        .map_err(|error| io_error(&error))?
                {
                    return Err(StoreError::UnsafePath);
                }
                if take_test_fault(32) {
                    return Err(StoreError::Io(
                        "injected failure before immutable repair rename".to_owned(),
                    ));
                }
                backend_platform::durability::replace_file_preserving_readonly_at(
                    &self.session.directory.file,
                    &stage_name,
                    &copied,
                    destination_parent,
                    name,
                )
                .map_err(|error| io_error(&error))?;
                // Both affected directories precede the receipt. On a failed barrier the new
                // name can be visible, but no caller receives permission to publish it.
                destination_parent
                    .sync_all()
                    .map_err(|error| io_error(&error))?;
                self.session.directory.sync_all()?;
                let current = backend_platform::durability::open_regular_file_child_nofollow(
                    destination_parent,
                    name,
                )
                .map_err(|error| io_error(&error))?;
                if backend_platform::file_identity::number_of_links(&current)
                    .map_err(|error| io_error(&error))?
                    != 1
                {
                    return Err(StoreError::UnsafePath);
                }
                if backend_platform::FileIdentity::of_file(&current)
                    .map_err(|error| io_error(&error))?
                    != backend_platform::FileIdentity::of_file(&copied)
                        .map_err(|error| io_error(&error))?
                {
                    return Err(StoreError::UnsafePath);
                }
                Ok(())
            })();
            if result.is_err() {
                // This can only remove our private name. It never clears a shared inode's
                // attributes, even after the copy was sealed or a rename failed.
                let _ = backend_platform::durability::unlink_file_preserving_readonly_at(
                    &self.session.directory.file,
                    &stage_name,
                    &copied,
                );
            }
            result
        }
    }

    #[cfg(windows)]
    impl Drop for DurabilityRepair<'_> {
        fn drop(&mut self) {
            let _ = cleanup_session(
                self.store,
                &self.session.parent,
                &self.session.name,
                &self.session.directory,
            );
        }
    }

    #[cfg(all(windows, test))]
    mod windows_repair_controls {
        #![allow(clippy::expect_used)]
        use super::*;
        use backend_platform::FileIdentity;

        struct Fixture {
            root: PathBuf,
            store: FileStore,
            path: PathBuf,
            _cleanup: super::super::TestDirectoryCleanup,
        }

        impl Fixture {
            fn new() -> Self {
                let root = std::env::temp_dir().join(format!(
                    "backend-windows-repair-{}-{}",
                    std::process::id(),
                    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
                ));
                let cleanup = super::super::TestDirectoryCleanup::new(&root);
                let store = FileStore::open(&root, 1 << 20).expect("open Windows repair store");
                let path = root.join("nodes/member.ref");
                fs::write(&path, b"verified member").expect("inject unsynchronized bytes");
                let mut permissions = fs::metadata(&path).expect("metadata").permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&path, permissions)
                    .expect("inject read-only bit without provenance");
                Self {
                    root,
                    store,
                    path,
                    _cleanup: cleanup,
                }
            }

            fn directory(&self) -> File {
                check_directory(&self.root.join("nodes")).expect("held fixture nodes directory")
            }

            fn assert_old(&self, identity: FileIdentity) {
                assert_eq!(
                    identity,
                    FileIdentity::of_path_nofollow(&self.path).expect("old name retained")
                );
                assert_eq!(
                    fs::read(&self.path).expect("old bytes readable"),
                    b"verified member"
                );
                assert!(
                    fs::metadata(&self.path)
                        .expect("old seal retained")
                        .permissions()
                        .readonly()
                );
            }
        }

        fn verify(file: File) -> Result<(), StoreError> {
            let mut bytes = Vec::new();
            file.take(16)
                .read_to_end(&mut bytes)
                .map_err(|error| io_error(&error))?;
            if bytes != b"verified member" {
                return Err(StoreError::Corrupt);
            }
            Ok(())
        }

        #[test]
        fn replacement_preserves_a_held_old_inode_and_its_readonly_alias() {
            let fixture = Fixture::new();
            let pin = fixture.store.acquire_gc_pin().expect("existing GC pin");
            let repair = DurabilityRepair::new(&fixture.store, &pin).expect("repair directory");
            let mut source = open_path(&fixture.path)
                .expect("open old name")
                .expect("old member");
            let old = FileIdentity::of_file(&source).expect("old identity");
            let alias = fixture.root.join("old-alias");
            fs::hard_link(&fixture.path, &alias).expect("retain a link to the old inode");
            repair
                .replace_verified(&fixture.directory(), "member.ref", &mut source, 15, verify)
                .expect("replace through the extended readonly rename capability");
            assert_ne!(
                old,
                FileIdentity::of_path_nofollow(&fixture.path).expect("new inode")
            );
            assert_eq!(
                old,
                FileIdentity::of_path_nofollow(&alias).expect("old alias identity")
            );
            assert!(
                source
                    .metadata()
                    .expect("old handle seal")
                    .permissions()
                    .readonly()
            );
            assert!(
                fs::metadata(&alias)
                    .expect("old alias seal")
                    .permissions()
                    .readonly()
            );
            assert!(
                fs::metadata(&fixture.path)
                    .expect("new seal")
                    .permissions()
                    .readonly()
            );
            source.seek(SeekFrom::Start(0)).expect("rewind old reader");
            let mut old_bytes = Vec::new();
            source
                .read_to_end(&mut old_bytes)
                .expect("held old reader remains usable");
            assert_eq!(old_bytes, b"verified member");
            assert_eq!(fs::read(&fixture.path).expect("new bytes"), old_bytes);
        }

        #[test]
        fn private_stage_create_failure_leaves_old_readonly_name_and_bytes() {
            let fixture = Fixture::new();
            let pin = fixture.store.acquire_gc_pin().expect("existing GC pin");
            let repair =
                DurabilityRepair::new(&fixture.store, &pin).expect("one leased repair directory");
            let _collision = create_stage_file(&repair.session.directory, "object-0.tmp")
                .expect("force actual create_new failure");
            let mut source = open_path(&fixture.path)
                .expect("open old name")
                .expect("old member");
            let identity = FileIdentity::of_file(&source).expect("old identity");
            assert!(
                repair
                    .replace_verified(&fixture.directory(), "member.ref", &mut source, 15, verify)
                    .is_err()
            );
            fixture.assert_old(identity);
            assert!(
                fixture
                    .store
                    .try_with_gc_exclusive_lease(|| Ok(()))
                    .expect("GC cannot cut across repair")
                    .is_none()
            );
            // `repair` borrows the existing pin; release the staged collision
            // handle and repair session before releasing GC custody.
            drop(_collision);
            drop(repair);
            drop(pin);
            assert!(
                fixture
                    .store
                    .try_with_gc_exclusive_lease(|| Ok(()))
                    .expect("GC released after repair")
                    .is_some()
            );
        }

        #[test]
        fn copy_bounds_verification_and_prepublication_failures_leave_old_inode_sealed() {
            for point in [0, 30, 31, 32] {
                let fixture = Fixture::new();
                let pin = fixture.store.acquire_gc_pin().expect("existing GC custody");
                let repair = DurabilityRepair::new(&fixture.store, &pin).expect("repair directory");
                let mut source = open_path(&fixture.path)
                    .expect("open old name")
                    .expect("old member");
                let identity = FileIdentity::of_file(&source).expect("old identity");
                if matches!(point, 31 | 32) {
                    super::super::set_test_fault(point);
                }
                let result = repair.replace_verified(
                    &fixture.directory(),
                    "member.ref",
                    &mut source,
                    if point == 0 { 1 } else { 15 },
                    |file| {
                        if point == 30 {
                            Err(StoreError::Corrupt)
                        } else {
                            verify(file)
                        }
                    },
                );
                assert!(result.is_err());
                fixture.assert_old(identity);
                // These fault points model a returned error, so let normal fixture cleanup run.
                let _ = super::super::simulate_process_crash();
            }
        }

        #[test]
        fn stage_identity_check_refuses_a_different_regular_file() {
            let fixture = Fixture::new();
            let foreign_path = fixture.root.join("foreign");
            fs::write(&foreign_path, b"verified member").expect("different regular inode");
            let foreign = File::open(&foreign_path).expect("foreign handle");
            assert_eq!(
                check_regular(&fixture.path, &foreign),
                Err(StoreError::UnsafePath)
            );
            let identity =
                FileIdentity::of_path_nofollow(&fixture.path).expect("original fixture identity");
            let directory = ArtifactDirectory {
                file: check_directory(&fixture.root.join("nodes"))
                    .expect("actual fixture directory"),
                path: fixture.root.join("nodes"),
            };
            assert!(unlink_stage_file(&directory, "member.ref", &foreign).is_err());
            fixture.assert_old(identity);
        }
    }

    #[cfg(windows)]
    fn checked_linked_file(
        parent: &File,
        name: &str,
        source: &File,
        links: u64,
    ) -> Result<File, StoreError> {
        let named = backend_platform::durability::open_regular_file_child_nofollow(parent, name)
            .map_err(|error| io_error(&error))?;
        if backend_platform::FileIdentity::of_file(&named).map_err(|error| io_error(&error))?
            != backend_platform::FileIdentity::of_file(source).map_err(|error| io_error(&error))?
            || backend_platform::file_identity::number_of_links(&named)
                .map_err(|error| io_error(&error))?
                != links
        {
            return Err(StoreError::UnsafePath);
        }
        Ok(named)
    }

    pub(in crate::durable) fn link_stage_object(
        staging: &ArtifactDirectory,
        staging_name: &str,
        source: &File,
        store: &FileStore,
        id: ObjectId,
    ) -> Result<Option<DurableObjectFile>, StoreError> {
        let staged_path = staging.path.join(staging_name);
        check_regular(&staged_path, source)?;
        seal_immutable_file(source)?;
        #[cfg(windows)]
        let objects_handle = {
            let root = check_directory(&store.root)?;
            backend_platform::durability::open_directory_child_nofollow(&root, "objects")
                .map_err(|error| io_error(&error))?
        };
        #[cfg(not(windows))]
        let objects = store.root.join("objects");
        let destination = store.object_path(id);
        match fs::hard_link(&staged_path, &destination) {
            Ok(()) => {
                #[cfg(windows)]
                {
                    // A receipt covers the actual destination under this held directory,
                    // never whichever directory a later path lookup happens to resolve.
                    let name = format!("{}.object", super::super::hex(id.as_bytes()));
                    checked_linked_file(&objects_handle, &name, source, 2)?;
                    objects_handle
                        .sync_all()
                        .map_err(|error| io_error(&error))?;
                }
                #[cfg(not(windows))]
                crate::durable::sync_directory(&objects)?;
                super::wait_test_link_barrier();
                if take_test_fault(12) {
                    return Err(StoreError::Io(
                        "injected interruption after object link".to_owned(),
                    ));
                }
                unlink_stage_file(staging, staging_name, source)?;
                #[cfg(windows)]
                {
                    let name = format!("{}.object", super::super::hex(id.as_bytes()));
                    let named = checked_linked_file(&objects_handle, &name, source, 1)?;
                    let identity = backend_platform::FileIdentity::of_file(&named)
                        .map_err(|error| io_error(&error))?;
                    Ok(Some(DurableObjectFile {
                        object: id,
                        identity,
                    }))
                }
                #[cfg(not(windows))]
                linked_object_receipt(source, store, id).map(Some)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(None),
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
        seal_immutable_file(&temp)?;
        #[cfg(windows)]
        let closures = {
            let root = check_directory(&store.root)?;
            backend_platform::durability::open_directory_child_nofollow(&root, "closures")
                .map_err(|error| io_error(&error))?
        };
        #[cfg(windows)]
        let name = format!("{}.closure", super::super::hex(id.as_bytes()));
        let destination = store.closure_path(id);
        let created = match fs::hard_link(&temp_path, &destination) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                #[cfg(windows)]
                let existing = backend_platform::durability::open_regular_file_child_nofollow(
                    &closures, &name,
                )
                .map_err(|error| io_error(&error))?;
                #[cfg(not(windows))]
                let existing = open_closure(store, id)?;
                #[cfg(windows)]
                if backend_platform::file_identity::number_of_links(&existing)
                    .map_err(|error| io_error(&error))?
                    != 1
                {
                    return Err(StoreError::UnsafePath);
                }
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
                #[cfg(windows)]
                {
                    // Equal old bytes and a read-only bit do not prove the descriptor's
                    // durability. The canonical temp already completed its actual barrier.
                    backend_platform::durability::replace_file_preserving_readonly_at(
                        &staging.file,
                        CLOSURE_TEMP_FILE,
                        &temp,
                        &closures,
                        &name,
                    )
                    .map_err(|error| io_error(&error))?;
                    closures.sync_all().map_err(|error| io_error(&error))?;
                    staging.sync_all()?;
                    checked_linked_file(&closures, &name, &temp, 1)?;
                    return Ok(false);
                }
                #[cfg(not(windows))]
                false
            }
            Err(error) => return Err(io_error(&error)),
        };
        if created {
            #[cfg(windows)]
            {
                checked_linked_file(&closures, &name, &temp, 2)?;
                closures.sync_all().map_err(|error| io_error(&error))?;
            }
            #[cfg(not(windows))]
            crate::durable::sync_directory(&store.root.join("closures"))?;
            super::wait_test_link_barrier();
            if take_test_fault(13) {
                return Err(StoreError::Io(
                    "injected interruption after closure link".to_owned(),
                ));
            }
        }
        unlink_stage_file(staging, CLOSURE_TEMP_FILE, &temp)?;
        #[cfg(windows)]
        checked_linked_file(&closures, &name, &temp, 1)?;
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

#[cfg(windows)]
pub(super) use imp::{DurabilityRepair, open_directory_file};

/// Recovers the staging sessions of processes that died mid-write, when the
/// store has ever staged one.
///
/// A process killed between linking an object into `objects/` and unlinking
/// its stage leaves that object with two links, and every read refuses a
/// twice-linked object as unsafe. Write sessions reap stale sessions when
/// they begin, but the next process can read that object first (a closure
/// write asks whether its relation nodes are already stored before it opens a
/// session), so open reaps too. A live session keeps its lease locked and is
/// left alone. The caller holds the process lock.
pub(super) fn reap_stale_sessions_on_open(store: &FileStore) -> Result<(), StoreError> {
    let sessions = store.root.join("staging").join("artifacts");
    let left = std::fs::read_dir(&sessions)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if !left {
        return Ok(());
    }
    let staging_root = prepare_staging_root(store)?;
    reap_stale_sessions(store, &staging_root)
}
